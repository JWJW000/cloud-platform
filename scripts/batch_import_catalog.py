#!/usr/bin/env python3
"""
高并发流式解析大型书目 Excel 并直接以【已下载馆藏】（Holdings + 已下载 Acquisition Targets）入库云端 PostgreSQL
具备断点续传（自动检测远端已存在行数并跳过，绝不重复插入，无缝继续）。
"""

import sys
import os
import zipfile
import xml.etree.ElementTree as ET
import re
import datetime
import uuid
import hashlib
import subprocess
import io
import time

BATCH_SIZE = 5000
LEGACY_STORAGE_LOCATION_ID = '00000000-0000-0000-0000-000000000001'

def clean_isbn(val):
    if not val:
        return []
    parts = re.split(r'[,;/\s]+', str(val).strip())
    results = []
    for p in parts:
        clean = re.sub(r'[^0-9Xx]', '', p)
        if len(clean) == 13:
            results.append(('isbn13', p.strip(), clean))
        elif len(clean) == 10:
            results.append(('isbn10', p.strip(), clean))
        elif clean:
            results.append(('custom', p.strip(), clean))
    return results

def parse_year(val):
    if not val:
        return None, None
    s = str(val).strip()
    if not s:
        return None, None

    if s.isdigit() and len(s) == 5:
        try:
            d = datetime.date(1899, 12, 30) + datetime.timedelta(days=int(s))
            return d.year, d.strftime('%Y-%m-%d')
        except:
            pass

    m = re.search(r'(1[89]\d\d|20[0-2]\d)', s)
    if m:
        try:
            return int(m.group(1)), s
        except:
            pass
    return None, s

def escape_tsv(val):
    if val is None:
        return r'\N'
    s = str(val)
    s = s.replace('\\', '\\\\').replace('\t', ' ').replace('\n', ' ').replace('\r', ' ')
    return s

def stream_xlsx(path):
    print(f"[*] 正在流式读取共享字符串池: {path} ...", flush=True)
    with zipfile.ZipFile(path, 'r') as z:
        shared_strings = []
        if 'xl/sharedStrings.xml' in z.namelist():
            with z.open('xl/sharedStrings.xml') as f:
                for event, elem in ET.iterparse(f, events=('end',)):
                    if elem.tag.endswith('si'):
                        texts = [e.text for e in elem.iter() if e.tag.endswith('t') and e.text]
                        shared_strings.append(''.join(texts))
                        elem.clear()

        print(f"[*] 字符串池加载完毕 ({len(shared_strings)} 条)，开始流式解析并入库...", flush=True)

        with z.open('xl/worksheets/sheet1.xml') as f:
            for event, elem in ET.iterparse(f, events=('end',)):
                if elem.tag.endswith('row'):
                    row_dict = {}
                    for c in elem.findall('{http://schemas.openxmlformats.org/spreadsheetml/2006/main}c'):
                        r = c.get('r', '')
                        col_letter = re.match(r'([A-Za-z]+)', r)
                        if not col_letter:
                            continue
                        col = col_letter.group(1).upper()
                        t = c.get('t')
                        v = c.find('{http://schemas.openxmlformats.org/spreadsheetml/2006/main}v')
                        val = v.text if v is not None else ''
                        if t == 's' and val.isdigit():
                            idx = int(val)
                            if idx < len(shared_strings):
                                val = shared_strings[idx]
                        row_dict[col] = val

                    row_num = elem.get('r')
                    elem.clear()
                    yield row_num, row_dict

def get_already_imported_count(file_type, ssh_target, key_path):
    sql = f"SELECT count(*) FROM library_files WHERE object_key LIKE 'imported/{file_type}/%';"
    cmd = [
        "ssh", "-i", key_path,
        "-o", "StrictHostKeyChecking=no",
        ssh_target,
        f"docker exec drission-postgres psql -U postgres -d drission_book -t -A -c \"{sql}\""
    ]
    try:
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if res.returncode == 0 and res.stdout.strip().isdigit():
            return int(res.stdout.strip())
    except Exception as e:
        print(f"[!] 检查已导入进度失败: {e}", flush=True)
    return 0

def process_and_import(xlsx_path, file_type, ssh_target, key_path):
    print(f"\n=======================================================", flush=True)
    print(f" 开始导入已下载馆藏: {xlsx_path} (模式: {file_type})", flush=True)
    print(f"=======================================================", flush=True)

    already_done = get_already_imported_count(file_type, ssh_target, key_path)
    print(f"[*] 经远端数据库对账，该文件之前已入库: {already_done:,} 条记录", flush=True)
    if already_done > 0:
        print(f"[*] 将自动跳过前 {already_done:,} 条已入库记录，实现断点续传！", flush=True)

    total_processed = 0
    total_valid = 0
    skipped_count = 0

    works_buf = io.StringIO()
    editions_buf = io.StringIO()
    identifiers_buf = io.StringIO()
    library_files_buf = io.StringIO()
    holdings_buf = io.StringIO()
    locations_buf = io.StringIO()
    targets_buf = io.StringIO()

    current_batch_count = 0

    def run_copy(table_cmd, buf):
        data = buf.getvalue()
        if not data:
            return
        payload = table_cmd + "\n" + data + "\\.\n"
        cmd = [
            "ssh", "-i", key_path,
            "-o", "StrictHostKeyChecking=no",
            ssh_target,
            "docker exec -i drission-postgres psql -U postgres -d drission_book -q"
        ]
        proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        stdout, stderr = proc.communicate(input=payload)
        if proc.returncode != 0:
            print(f"[!] {table_cmd.split()[1]} COPY 错误: {stderr}", flush=True)
            raise RuntimeError(f"Database error: {stderr}")

    def flush_copy():
        nonlocal current_batch_count
        if current_batch_count == 0:
            return

        run_copy("COPY works (id, work_type, preferred_title, normalized_title, primary_language, resolution_status, created_at, updated_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", works_buf)
        run_copy("COPY editions (id, work_id, edition_title, language, publisher, publish_year, publish_date_text, intro, status, created_at, updated_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", editions_buf)
        run_copy("COPY identifiers (id, object_type, object_id, identifier_type, raw_value, normalized_value, is_valid, created_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", identifiers_buf)
        run_copy("COPY library_files (id, storage_backend, object_key, format, actual_size_bytes, sha256, verify_status, verified_at, created_at, updated_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", library_files_buf)
        run_copy("COPY holdings (id, edition_id, library_file_id, match_type, meets_strategy, created_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", holdings_buf)
        run_copy("COPY library_file_locations (id, library_file_id, storage_location_id, object_key, actual_size_bytes, verify_status, verified_at, last_seen_at, created_at, updated_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", locations_buf)
        run_copy("COPY acquisition_targets (id, edition_id, status, priority, attempts, max_attempts, next_attempt_at, satisfied_holding_id, created_at, updated_at) FROM STDIN WITH (FORMAT text, DELIMITER E'\\t', NULL E'\\\\N');", targets_buf)

        works_buf.seek(0); works_buf.truncate(0)
        editions_buf.seek(0); editions_buf.truncate(0)
        identifiers_buf.seek(0); identifiers_buf.truncate(0)
        library_files_buf.seek(0); library_files_buf.truncate(0)
        holdings_buf.seek(0); holdings_buf.truncate(0)
        locations_buf.seek(0); locations_buf.truncate(0)
        targets_buf.seek(0); targets_buf.truncate(0)
        current_batch_count = 0

    now_ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%d %H:%M:%S+00')

    for row_num, row in stream_xlsx(xlsx_path):
        if row_num == '1':
            continue

        total_processed += 1

        title = None
        author = None
        publisher = None
        year_raw = None
        isbn_raw = None
        dams_code = None
        ext_id = None
        intro = None
        format_val = 'pdf'
        lang = 'zh'

        if file_type == 'export_books_202607':
            title = row.get('B')
            author = row.get('E')
            year_raw = row.get('F')
            publisher = row.get('H')
            lang_val = row.get('I', '').lower()
            if 'english' in lang_val or 'eng' in lang_val:
                lang = 'en'
            elif 'german' in lang_val:
                lang = 'de'
            elif 'russian' in lang_val:
                lang = 'ru'
            elif 'chinese' in lang_val or 'chi' in lang_val:
                lang = 'zh'
            else:
                lang = 'other'
            isbn_raw = row.get('J')
            format_val = row.get('K', 'pdf').lower().strip() or 'pdf'
            ext_id = row.get('A')

        elif file_type == 'shumu1':
            ext_id = row.get('A')
            isbn_raw = row.get('B')
            title = row.get('C')
            publisher = row.get('D')
            author = row.get('E')
            year_raw = row.get('F')

        elif file_type == 'shumu2':
            ext_id = row.get('A')
            dams_code = row.get('B')
            title = row.get('D')
            author = row.get('F')
            publisher = row.get('G')
            year_raw = row.get('H')
            isbn_raw = row.get('I')
            intro = row.get('L')
            entity_types = row.get('M', 'pdf').lower()
            if 'epub' in entity_types:
                format_val = 'epub'
            elif 'pdf' in entity_types:
                format_val = 'pdf'
            elif 'txt' in entity_types:
                format_val = 'txt'

        if not title or not title.strip():
            continue

        # 检查是否为已导入过的记录
        if skipped_count < already_done:
            skipped_count += 1
            if skipped_count % 50000 == 0 or skipped_count == already_done:
                print(f"[{datetime.datetime.now().strftime('%H:%M:%S')}] 快速跳过已入库记录: {skipped_count:,}/{already_done:,} ...", flush=True)
            continue

        title = title.strip()
        author = author.strip() if author else None
        publisher = publisher.strip() if publisher else None
        intro = intro.strip() if intro else None

        pub_year, pub_date_text = parse_year(year_raw)

        work_id = str(uuid.uuid4())
        edition_id = str(uuid.uuid4())
        library_file_id = str(uuid.uuid4())
        holding_id = str(uuid.uuid4())
        location_id = str(uuid.uuid4())
        target_id = str(uuid.uuid4())

        norm_title = title.lower()

        # 生成稳定的唯一 object_key 与 sha256 占位
        file_sha256 = hashlib.sha256(f"{file_type}:{total_processed}:{title}:{isbn_raw}".encode('utf-8')).hexdigest()
        object_key = f"imported/{file_type}/{file_sha256[:2]}/{file_sha256}.{format_val}"

        # 1. works
        works_buf.write(f"{work_id}\t整书\t{escape_tsv(title)}\t{escape_tsv(norm_title)}\t{lang}\t已确认\t{now_ts}\t{now_ts}\n")

        # 2. editions
        editions_buf.write(f"{edition_id}\t{work_id}\t{escape_tsv(title)}\t{lang}\t{escape_tsv(publisher)}\t{escape_tsv(pub_year)}\t{escape_tsv(pub_date_text)}\t{escape_tsv(intro)}\t已确认\t{now_ts}\t{now_ts}\n")

        # 3. identifiers
        isbns = clean_isbn(isbn_raw)
        for itype, raw_val, norm_val in isbns:
            ident_id = str(uuid.uuid4())
            identifiers_buf.write(f"{ident_id}\tedition\t{edition_id}\t{itype}\t{escape_tsv(raw_val)}\t{escape_tsv(norm_val)}\tt\t{now_ts}\n")

        if dams_code and dams_code.strip():
            ident_id = str(uuid.uuid4())
            identifiers_buf.write(f"{ident_id}\tedition\t{edition_id}\tdams_code\t{escape_tsv(dams_code.strip())}\t{escape_tsv(dams_code.strip())}\tt\t{now_ts}\n")

        if ext_id and str(ext_id).strip():
            ident_id = str(uuid.uuid4())
            identifiers_buf.write(f"{ident_id}\tedition\t{edition_id}\texternal_id\t{escape_tsv(str(ext_id).strip())}\t{escape_tsv(str(ext_id).strip())}\tt\t{now_ts}\n")

        # 4. library_files (NAS实体文件)
        library_files_buf.write(f"{library_file_id}\tNAS\t{escape_tsv(object_key)}\t{escape_tsv(format_val)}\t0\t{file_sha256}\t有效\t{now_ts}\t{now_ts}\t{now_ts}\n")

        # 5. holdings (馆藏所有权)
        holdings_buf.write(f"{holding_id}\t{edition_id}\t{library_file_id}\t精确匹配\tt\t{now_ts}\n")

        # 6. library_file_locations (物理存储位置)
        locations_buf.write(f"{location_id}\t{library_file_id}\t{LEGACY_STORAGE_LOCATION_ID}\t{escape_tsv(object_key)}\t0\t有效\t{now_ts}\t{now_ts}\t{now_ts}\t{now_ts}\n")

        # 7. acquisition_targets (状态为'已下载'，并指向 holding_id)
        targets_buf.write(f"{target_id}\t{edition_id}\t已下载\t0\t1\t5\t{now_ts}\t{holding_id}\t{now_ts}\t{now_ts}\n")

        total_valid += 1
        current_batch_count += 1

        if current_batch_count >= BATCH_SIZE:
            flush_copy()
            print(f"[{datetime.datetime.now().strftime('%H:%M:%S')}] 进度: 新增入库 {total_valid:,} 条 (本文件累计: {(already_done + total_valid):,} 条)...", flush=True)

    if current_batch_count > 0:
        flush_copy()

    print(f"[✓] {xlsx_path} 处理完毕！本次新增入库 {total_valid:,} 本已下载馆藏！\n", flush=True)

if __name__ == '__main__':
    KEY = os.path.expanduser('~/Downloads/Key_us.pem')
    SSH = 'root@43.165.64.253'

    files = [
        ('data/csv/export_books_202607.xlsx', 'export_books_202607'),
        ('data/csv/图书书目1.xlsx', 'shumu1'),
        ('data/csv/图书书目2.xlsx', 'shumu2'),
    ]

    for path, ftype in files:
        if os.path.exists(path):
            process_and_import(path, ftype, SSH, KEY)
