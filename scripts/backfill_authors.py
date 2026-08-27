#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
一次性回填脚本：为已入库的馆藏补写作者（contributors + edition_contributors）。

背景：batch_import_catalog.py 此前解析了 author 列却从未写入数据库，导致
contributors / edition_contributors 全空、前端作者显示「未知」。本脚本遍历三个
Excel，按与原脚本完全一致的公式重算 file_sha256，利用临时表 + COPY + 关系集操作批量
反查 edition_id 并补齐作者关联。

匹配公式（必须与原脚本逐字节一致，否则无法命中）：
    file_sha256 = sha256(f"{file_type}:{data_row_ordinal}:{title.strip()}:{isbn_raw}")
其中 data_row_ordinal 是跳过表头后的全文件数据行序号（含无书名行，1 起）。

作者清洗规则（保持与主脚本同源、且与 Rust normalize_title 规范化一致）：
    1. 剥离开头 (国别/朝代) 或 [国别] 或「英 / 美 …」前缀；
    2. 按 [、,;；/，] 及 2+ 连续空白切分多人；
    3. 剥离尾部「等人 / 等」与编著/主编/编译/著/编/译/撰/绘 等角色后缀；
    4. 整格即角色词（如「主编」）整体丢弃；
    5. normalize_person 复刻 dedup.rs::normalize_title：全角→半角、去空白与标点、小写。

架构与性能：
    本地流式解析 Excel → 生成 (sha256, contributor_id, order) 及 (contributor_id, name, norm)
    → SSH 管道执行 CREATE TEMP TABLE + COPY 批量载入临时表
    → SQL 集合操作 JOIN library_files + holdings ON CONFLICT DO NOTHING 批量落库。
    单个大文件在数秒内完成百万级关系落库，无内存暴涨与网络往返开销。

用法：
    python3 scripts/backfill_authors.py                 # 全量回填 3 个文件
    python3 scripts/backfill_authors.py --only-file export_books_202607
    python3 scripts/backfill_authors.py --probe 50      # 只看清洗结果，不写库
    python3 scripts/backfill_authors.py --dry-run       # 仅统计，不写库
"""

import datetime
import hashlib
import io
import os
import re
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from batch_import_catalog import stream_xlsx

KEY = os.path.expanduser('~/Downloads/Key_us.pem')
SSH = 'root@43.165.64.253'

# (path, file_type, title_col, isbn_col, author_col)
FILES = [
    ('data/csv/export_books_202607.xlsx', 'export_books_202607', 'B', 'J', 'E'),
    ('data/csv/图书书目1.xlsx', 'shumu1', 'C', 'B', 'E'),
    ('data/csv/图书书目2.xlsx', 'shumu2', 'D', 'I', 'F'),
]

# 确定性 contributor 命名空间
NS_AUTHOR = uuid.UUID('8a3e1a80-6e4f-4f5e-9a1c-3d7e5f2a9b00')

# 多人分隔符
AUTHOR_SPLIT = re.compile(r'[、,;；/，]')
MULTI_SPACE = re.compile(r'\s{2,}')

# 国别/朝代词表（用于剥离 (美)/(唐)/[美] 及「英 作者名」前缀）
COUNTRY_DYNASTY = {
    '周', '春秋', '战国', '秦', '汉', '西汉', '东汉', '三国', '晋', '西晋', '东晋',
    '南北朝', '隋', '唐', '五代', '十国', '宋', '北宋', '南宋', '辽', '金', '元',
    '明', '清', '民国', '现代', '当代', '古代', '近现代', '晚清', '先秦', '两汉',
    '美', '英', '法', '德', '俄', '日', '意', '韩', '朝', '印', '奥', '加', '澳',
    '瑞', '荷', '比', '丹', '苏', '波', '捷', '匈', '罗', '保', '乌', '哈', '蒙',
    '泰', '越', '新', '马', '菲', '印尼', '波兰', '捷克', '匈牙利', '罗马尼亚',
    '保加利亚', '南斯拉夫', '乌克兰', '白俄罗斯', '哈萨克', '蒙古', '巴基斯坦',
    '伊朗', '伊拉克', '沙特', '土耳其', '摩洛哥', '巴西', '阿根廷', '智利', '秘鲁',
    '哥伦比亚', '委内瑞拉', '墨西哥', '美国', '英国', '法国', '德国', '俄国',
    '苏联', '前苏联', '俄罗斯', '意大利', '日本', '朝鲜', '韩国', '印度',
    '澳大利亚', '新西兰', '新加坡', '菲律宾', '马来西亚', '南非', '加拿大',
    '奥地利', '比利时', '荷兰', '瑞士', '瑞典', '挪威', '芬兰', '丹麦', '西班牙',
    '葡萄牙', '希腊', '埃及', '以色列', '古巴',
}
BARE_PREFIX = re.compile(
    r'^(' + '|'.join(sorted(COUNTRY_DYNASTY, key=len, reverse=True)) + r')\s'
)
BRACKET_PREFIX = re.compile(r'^([（(\[【])([^（()\[\]【】]{1,10})([）)\]】])')

# 纯角色词（整格即丢弃）
ROLE_WORDS = {
    '著', '编', '译', '撰', '绘', '摄', '述', '辑', '纂', '注', '校', '写',
    '主编', '编著', '编译', '编写', '编撰', '选编', '编辑', '总编', '总编辑',
    '整理', '改编', '改写', '执笔', '点校', '校注', '校订', '补订', '重编',
    '口述', '辑录', '纂修', '注释', '评注', '编译者', '编者', '著者', '译者',
    '等', '等人', 'unknown', 'unknown author', '佚名', '无名氏',
}

# 角色后缀
ROLE_SUFFIXES = (
    '总编辑', '主编', '编著', '编译', '编写', '编撰', '选编', '编辑',
    '整理', '校注', '校订', '补订', '重编', '改写', '口述', '辑录',
    '纂修', '注释', '评注', '点校', '校译', '编绘', '绘图', '执笔',
    '摄制', '监制', '顾问', '撰稿', '编委会', '原著', '译著',
    '著', '编', '译', '撰', '绘', '摄',
)
ETC_RE = re.compile(r'(?:等人|等)$')
MAX_AUTHORS_PER_EDITION = 12

PUNCT = set("()[]{}【】《》〈〉「」『』,.;:!?、。，．；：！？—–-_·~`'\"“”‘’/\\|*+=&#@$%^")


def normalize_person(raw):
    """复刻 Rust dedup.rs::normalize_title：全角→半角、去空白与标点、小写。"""
    out = []
    for ch in raw:
        cp = ord(ch)
        if 0xFF01 <= cp <= 0xFF5E:
            out.append(chr(cp - 0xFEE0))
        elif cp == 0x3000:
            out.append(' ')
        else:
            out.append(ch)
    s = ''.join(out)
    s = ''.join(c for c in s if not c.isspace() and c not in PUNCT)
    return s.lower()


def strip_prefix(seg):
    """剥离开头 (国别/朝代)、[国别]、裸国别码（后随空白）前缀。"""
    while True:
        m = BRACKET_PREFIX.match(seg)
        if m and m.group(2) in COUNTRY_DYNASTY:
            seg = seg[m.end():].strip()
            continue
        m = BARE_PREFIX.match(seg)
        if m:
            seg = seg[m.end():].strip()
            continue
        break
    return seg


def clean_authors(raw):
    """把一格作者字符串清洗为作者名列表（保持出现顺序，去重）。"""
    if raw is None:
        return []
    raw = str(raw).strip()
    if not raw:
        return []

    segs = []
    for part in AUTHOR_SPLIT.split(raw):
        for sub in MULTI_SPACE.split(part):
            sub = sub.strip()
            if sub:
                segs.append(sub)

    out = []
    for seg in segs:
        seg = strip_prefix(seg)
        if not seg or seg.lower() in ROLE_WORDS:
            continue
        seg = ETC_RE.sub('', seg).strip()
        if not seg or seg.lower() in ROLE_WORDS:
            continue
        for suf in ROLE_SUFFIXES:
            if seg.endswith(suf) and len(seg) > len(suf):
                seg = seg[:-len(suf)].strip()
                break
        if not seg or seg.lower() in ROLE_WORDS:
            continue
        if seg not in out:
            out.append(seg)
        if len(out) >= MAX_AUTHORS_PER_EDITION:
            break
    return out


def escape_tsv(val):
    if val is None:
        return r'\N'
    s = str(val).replace('\\', '\\\\').replace('\t', ' ').replace('\n', ' ').replace('\r', '')
    return s


def run_remote_sql(sql):
    """在远端 postgres 执行单条 SQL（-c），返回 (stdout, stderr, returncode)。"""
    cmd = [
        "ssh", "-i", KEY,
        "-o", "StrictHostKeyChecking=no",
        SSH,
        "docker exec drission-postgres psql -U postgres -d drission_book -t -A -c \"" +
        sql.replace('"', '\\"') + '"',
    ]
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return res.stdout, res.stderr, res.returncode


def run_remote_pipeline(script_generator):
    """
    通过 SSH stdin 管道流式传输大型 SQL / COPY 脚本给 remote psql。
    支持以生成器分块输出，避免在本地内存中构造过大字符串。
    """
    cmd = [
        "ssh", "-i", KEY,
        "-o", "StrictHostKeyChecking=no",
        SSH,
        "docker exec -i drission-postgres psql -U postgres -d drission_book -q -v ON_ERROR_STOP=1",
    ]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True)
    for chunk in script_generator:
        proc.stdin.write(chunk)
    proc.stdin.close()
    stdout = proc.stdout.read()
    stderr = proc.stderr.read()
    proc.wait()
    if proc.returncode != 0:
        raise RuntimeError(f"psql 远程执行错误 (exit {proc.returncode}): {stderr}")
    return stdout


def process_file(path, ftype, tcol, icol, acol, probe=0, dry_run=False):
    print(f"\n=======================================================", flush=True)
    print(f" 开始回填作者: {path} (模式: {ftype})", flush=True)
    print(f"=======================================================", flush=True)

    t0 = time.time()
    total_processed = 0
    valid_titles = 0
    nonempty_authors = 0

    contributors_dict = {}  # norm -> (cid, name, norm)
    sha_author_entries = [] # list of (file_sha256, cid, sort_order)

    for row_num, row in stream_xlsx(path):
        if row_num == '1':
            continue
        total_processed += 1

        title_raw = row.get(tcol)
        if not title_raw or not str(title_raw).strip():
            continue
        title = str(title_raw).strip()
        valid_titles += 1

        isbn_raw = row.get(icol)
        author_raw = row.get(acol)

        if probe:
            if author_raw and str(author_raw).strip():
                cleaned = clean_authors(author_raw)
                print(f"    原始: {str(author_raw)[:80]!r}", flush=True)
                print(f"       →  {cleaned}", flush=True)
                nonempty_authors += 1
                if nonempty_authors >= probe:
                    break
            continue

        file_sha256 = hashlib.sha256(
            f"{ftype}:{total_processed}:{title}:{isbn_raw}".encode('utf-8')
        ).hexdigest()

        if not author_raw or not str(author_raw).strip():
            continue

        authors = clean_authors(author_raw)
        if not authors:
            continue

        nonempty_authors += 1
        for order, aname in enumerate(authors):
            norm = normalize_person(aname)
            if not norm:
                continue
            cid = str(uuid.uuid5(NS_AUTHOR, norm))
            if norm not in contributors_dict:
                contributors_dict[norm] = (cid, aname, norm)
            sha_author_entries.append((file_sha256, cid, order))

    if probe:
        return 0

    t_parse = time.time()
    print(f"[*] 解析完成: 数据行 {total_processed:,}，有效书名 {valid_titles:,}，"
          f"含有效作者 {nonempty_authors:,} 本 (耗时 {t_parse - t0:.1f}s)", flush=True)
    print(f"[*] 提取唯一作者 {len(contributors_dict):,} 个，"
          f"待对账映射条目 {len(sha_author_entries):,} 条", flush=True)

    if dry_run:
        print("[*] --dry-run 模式：跳过写入远端数据库", flush=True)
        return nonempty_authors

    # 构造 SQL 流水线生成器
    def generate_sql():
        # 1. 创建临时表
        yield (
            "CREATE TEMP TABLE tmp_contributors (\n"
            "    id UUID,\n"
            "    name TEXT,\n"
            "    normalized_name TEXT\n"
            ");\n"
            "CREATE TEMP TABLE tmp_file_authors (\n"
            "    sha256 TEXT,\n"
            "    contributor_id UUID,\n"
            "    sort_order INT\n"
            ");\n"
        )

        # 2. COPY 写入 tmp_contributors
        yield "COPY tmp_contributors (id, name, normalized_name) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');\n"
        buf = io.StringIO()
        for cid, name, norm in contributors_dict.values():
            buf.write(f"{cid}\t{escape_tsv(name)}\t{escape_tsv(norm)}\n")
            if buf.tell() > 256 * 1024:
                yield buf.getvalue()
                buf.seek(0)
                buf.truncate(0)
        if buf.tell() > 0:
            yield buf.getvalue()
        yield "\\.\n"

        # 3. COPY 写入 tmp_file_authors
        yield "COPY tmp_file_authors (sha256, contributor_id, sort_order) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');\n"
        buf = io.StringIO()
        for sha, cid, order in sha_author_entries:
            buf.write(f"{sha}\t{cid}\t{order}\n")
            if buf.tell() > 256 * 1024:
                yield buf.getvalue()
                buf.seek(0)
                buf.truncate(0)
        if buf.tell() > 0:
            yield buf.getvalue()
        yield "\\.\n"

        # 4. 执行集合写入与对账
        yield (
            "INSERT INTO contributors (id, name, normalized_name)\n"
            "SELECT DISTINCT id, name, normalized_name FROM tmp_contributors\n"
            "ON CONFLICT (normalized_name) DO NOTHING;\n"
            "\n"
            "INSERT INTO edition_contributors (id, edition_id, contributor_id, role, sort_order)\n"
            "SELECT\n"
            "    gen_random_uuid(),\n"
            "    h.edition_id,\n"
            "    t.contributor_id,\n"
            "    '作者',\n"
            "    t.sort_order\n"
            "FROM tmp_file_authors t\n"
            "JOIN library_files lf ON lf.sha256 = t.sha256\n"
            "JOIN holdings h ON h.library_file_id = lf.id\n"
            "ON CONFLICT (edition_id, contributor_id, role) DO NOTHING;\n"
        )

    print(f"[*] 正在向远端数据库推入临时表并执行关联入库...", flush=True)
    t_push_start = time.time()
    run_remote_pipeline(generate_sql())
    t_done = time.time()

    print(f"[✓] {ftype} 回填完成！(数据库写入耗时: {t_done - t_push_start:.1f}s, 总耗时: {t_done - t0:.1f}s)", flush=True)
    return nonempty_authors


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument('--only-file', default=None, help='只回填指定 file_type (export_books_202607, shumu1, shumu2)')
    ap.add_argument('--probe', type=int, default=0, help='只展示前 N 条清洗结果，不写库')
    ap.add_argument('--dry-run', action='store_true', help='只统计命中数，不写库')
    args = ap.parse_args()

    total = 0
    t_all = time.time()
    for path, ftype, tcol, icol, acol in FILES:
        if args.only_file and ftype != args.only_file:
            continue
        if not os.path.exists(path):
            print(f"[!] 文件不存在: {path}", flush=True)
            continue
        total += process_file(path, ftype, tcol, icol, acol,
                              probe=args.probe, dry_run=args.dry_run)

    if not args.dry_run and not args.probe:
        print(f"\n=======================================================", flush=True)
        print(f" 回填后全库作者对账统计 (总耗时: {time.time() - t_all:.1f}s)", flush=True)
        print(f"=======================================================", flush=True)
        out_ec, _, _ = run_remote_sql("SELECT count(*) FROM edition_contributors;")
        out_c, _, _ = run_remote_sql("SELECT count(*) FROM contributors;")
        out_missing, _, _ = run_remote_sql(
            "SELECT count(*) FROM editions e WHERE NOT EXISTS "
            "(SELECT 1 FROM edition_contributors ec WHERE ec.edition_id = e.id);"
        )
        print(f"edition_contributors 关联总数: {out_ec.strip():>10}", flush=True)
        print(f"contributors 作者实体总数:      {out_c.strip():>10}", flush=True)
        print(f"当前仍无作者的版本数:          {out_missing.strip():>10}", flush=True)


if __name__ == '__main__':
    main()
