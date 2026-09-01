//! 图书馆总库实体解析与消歧引擎（第 5 节）。
//!
//! 实现三层去重与实体消歧：
//! 1. 来源行幂等；
//! 2. 文件实体识别；
//! 3. 书目规范化解析（DOI -> ISBN -> 外部编号 -> 书名+作者+出版社 -> 待消歧）。

use platform_domain::{
    clean_text, extract_isbns, normalize_doi, normalize_format, normalize_md5, normalize_person,
    normalize_title, parse_publish_year, AcquisitionStatus, ContributorRole, ResolutionStatus,
    SubjectType, WorkType,
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppResult;

/// 规范化输入结构体，由解析器产生。
#[derive(Debug, Clone, Default)]
pub struct ParsedCatalogItem {
    /// 来源原始外部编号（如书目 ID / damsCode）。
    pub external_id: Option<String>,
    /// 作品类型（整书/章节/论文/合集）。
    pub work_type: WorkType,
    /// 原始标题。
    pub raw_title: String,
    /// 原始作者文本。
    pub raw_author: Option<String>,
    /// 原始出版社。
    pub raw_publisher: Option<String>,
    /// 原始 ISBN（可能含多个）。
    pub raw_isbn: Option<String>,
    /// 原始 DOI。
    pub raw_doi: Option<String>,
    /// 原始出版年份或日期。
    pub raw_year: Option<String>,
    /// 原始语言（zh/en/ot）。
    pub raw_language: Option<String>,
    /// 原始分类或中图分类号。
    pub raw_category: Option<String>,
    /// 简介。
    pub intro: Option<String>,
    /// 文件格式（如 pdf/epub/azw3）。
    pub format: Option<String>,
    /// 文件 MD5 哈希。
    pub md5: Option<String>,
    /// 声明文件大小（字节）。
    pub filesize: Option<i64>,
    /// 原始完整行数据 JSON。
    pub raw_payload: serde_json::Value,
    /// 所属专著的 DOI 或标题（针对章节）。
    pub parent_work_hint: Option<String>,
}

/// 解析与消歧结果摘要。
#[derive(Debug, Clone)]
pub struct ResolutionResult {
    /// 规范作品编号。
    pub work_id: Uuid,
    /// 规范版本编号。
    pub edition_id: Uuid,
    /// 是否为新创建的作品。
    pub is_new_work: bool,
    /// 是否为新创建的版本。
    pub is_new_edition: bool,
    /// 匹配方法。
    pub match_method: String,
    /// 置信度（0.0 ~ 1.0）。
    pub confidence: f64,
    /// 消歧状态。
    pub status: ResolutionStatus,
}

/// 将一条来源记录解析并归并到规范书目体系中。
pub async fn resolve_item(
    tx: &mut Transaction<'_, Postgres>,
    source_id: Uuid,
    source_record_id: Uuid,
    item: &ParsedCatalogItem,
) -> AppResult<ResolutionResult> {
    let clean_title = clean_text(&item.raw_title);
    let norm_title = normalize_title(&clean_title);
    let norm_author = item
        .raw_author
        .as_deref()
        .map(normalize_person)
        .filter(|s| !s.is_empty());
    let norm_publisher = item
        .raw_publisher
        .as_deref()
        .map(normalize_person)
        .filter(|s| !s.is_empty());
    let pub_year = item.raw_year.as_deref().and_then(parse_publish_year);
    let primary_lang = item
        .raw_language
        .as_deref()
        .unwrap_or("zh")
        .trim()
        .to_lowercase();
    let norm_doi = item.raw_doi.as_deref().and_then(normalize_doi);
    let isbns = item
        .raw_isbn
        .as_deref()
        .map(extract_isbns)
        .unwrap_or_default();
    let norm_md5 = item.md5.as_deref().and_then(normalize_md5);
    let clean_fmt = item.format.as_deref().map(normalize_format);

    // 1. 尝试按合法 DOI 匹配
    if let Some(ref doi) = norm_doi {
        let existing_edition: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT e.id, e.work_id FROM identifiers i \
             JOIN editions e ON e.id = i.object_id \
             WHERE i.identifier_type = 'doi' AND i.normalized_value = $1 AND i.is_valid \
             LIMIT 1",
        )
        .bind(doi)
        .fetch_optional(&mut **tx)
        .await?;

        if let Some((edition_id, work_id)) = existing_edition {
            let res = attach_source_and_assets(
                tx,
                source_record_id,
                work_id,
                edition_id,
                clean_fmt.as_deref(),
                item.filesize,
                norm_md5.as_deref(),
                "doi",
                1.0,
                ResolutionStatus::Confirmed,
            )
            .await?;
            return Ok(res);
        }
    }

    // 2. 尝试按合法 ISBN 匹配版本
    for isbn in &isbns {
        let existing_edition: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT e.id, e.work_id FROM identifiers i \
             JOIN editions e ON e.id = i.object_id \
             WHERE i.identifier_type IN ('isbn13', 'isbn10') AND i.normalized_value = $1 AND i.is_valid \
             LIMIT 1"
        )
        .bind(isbn.as_str())
        .fetch_optional(&mut **tx)
        .await?;

        if let Some((edition_id, work_id)) = existing_edition {
            let res = attach_source_and_assets(
                tx,
                source_record_id,
                work_id,
                edition_id,
                clean_fmt.as_deref(),
                item.filesize,
                norm_md5.as_deref(),
                "isbn",
                1.0,
                ResolutionStatus::Confirmed,
            )
            .await?;
            return Ok(res);
        }
    }

    // 3. 尝试按同来源的稳定外部 ID 匹配
    if let Some(ref ext_id) = item.external_id {
        let existing_res: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT rr.work_id, rr.edition_id FROM source_records sr \
             JOIN record_resolutions rr ON rr.source_record_id = sr.id \
             WHERE sr.source_id = $1 AND sr.external_id = $2 AND rr.work_id IS NOT NULL AND rr.edition_id IS NOT NULL \
             LIMIT 1"
        )
        .bind(source_id)
        .bind(ext_id)
        .fetch_optional(&mut **tx)
        .await?;

        if let Some((work_id, edition_id)) = existing_res {
            let res = attach_source_and_assets(
                tx,
                source_record_id,
                work_id,
                edition_id,
                clean_fmt.as_deref(),
                item.filesize,
                norm_md5.as_deref(),
                "source_external_id",
                0.95,
                ResolutionStatus::Confirmed,
            )
            .await?;
            return Ok(res);
        }
    }

    // 4. 尝试按书名+作者+出版社唯一匹配
    if !norm_title.is_empty() && norm_author.is_some() && norm_publisher.is_some() {
        let author_str = norm_author.as_deref().unwrap();
        let pub_str = norm_publisher.as_deref().unwrap();

        let matches: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT e.id, e.work_id FROM editions e \
             JOIN works w ON w.id = e.work_id \
             WHERE w.normalized_title = $1 AND e.publisher ILIKE $2 \
               AND EXISTS (SELECT 1 FROM edition_contributors ec JOIN contributors c ON c.id = ec.contributor_id WHERE ec.edition_id = e.id AND c.normalized_name = $3) \
             LIMIT 2"
        )
        .bind(&norm_title)
        .bind(pub_str)
        .bind(author_str)
        .fetch_all(&mut **tx)
        .await?;

        if matches.len() == 1 {
            let (edition_id, work_id) = matches[0];
            let res = attach_source_and_assets(
                tx,
                source_record_id,
                work_id,
                edition_id,
                clean_fmt.as_deref(),
                item.filesize,
                norm_md5.as_deref(),
                "title_author_publisher",
                0.85,
                ResolutionStatus::Confirmed,
            )
            .await?;
            return Ok(res);
        }
    }

    // 5. 若无精确命中，则新建实体
    let work_id = Uuid::new_v4();
    let edition_id = Uuid::new_v4();
    let res_status = if isbns.is_empty()
        && norm_doi.is_none()
        && (norm_author.is_none() || norm_publisher.is_none())
    {
        ResolutionStatus::Ambiguous
    } else {
        ResolutionStatus::Confirmed
    };

    // 章节关联父作品
    let mut parent_work_id = None;
    if item.work_type == WorkType::Chapter {
        if let Some(ref hint) = item.parent_work_hint {
            let parent_norm = normalize_title(hint);
            let found_parent: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM works WHERE (normalized_title = $1 OR preferred_title ILIKE $2) AND work_type != '章节' LIMIT 1"
            )
            .bind(&parent_norm)
            .bind(hint)
            .fetch_optional(&mut **tx)
            .await?;
            parent_work_id = found_parent;
        }
    }

    // 创建 Work
    sqlx::query(
        "INSERT INTO works (id, work_type, preferred_title, normalized_title, primary_language, parent_work_id, resolution_status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)"
    )
    .bind(work_id)
    .bind(item.work_type.as_str())
    .bind(&clean_title)
    .bind(&norm_title)
    .bind(&primary_lang)
    .bind(parent_work_id)
    .bind(res_status.as_str())
    .execute(&mut **tx)
    .await?;

    // 解析并绑定出版社主档（若存在）
    let mut publisher_id: Option<Uuid> = None;
    if let Some(ref pub_name) = item.raw_publisher {
        let clean_pub = clean_text(pub_name);
        if !clean_pub.is_empty() {
            let norm_pub = crate::store::publishers::normalize_publisher_name(&clean_pub);
            // 查别名表或主表
            let found_pub: Option<Uuid> = sqlx::query_scalar(
                "SELECT publisher_id FROM publisher_aliases WHERE normalized_alias = $1 \
                 UNION \
                 SELECT id FROM publishers WHERE normalized_name = $1 LIMIT 1",
            )
            .bind(&norm_pub)
            .fetch_optional(&mut **tx)
            .await?;

            if let Some(pid) = found_pub {
                publisher_id = Some(pid);
            } else {
                let new_pub_id = Uuid::new_v4();
                let pid: Option<Uuid> = sqlx::query_scalar(
                    "INSERT INTO publishers (id, name, normalized_name) \
                     VALUES ($1, $2, $3) \
                     ON CONFLICT (normalized_name) DO UPDATE SET name = EXCLUDED.name \
                     RETURNING id",
                )
                .bind(new_pub_id)
                .bind(&clean_pub)
                .bind(&norm_pub)
                .fetch_optional(&mut **tx)
                .await?;
                publisher_id = pid.or(Some(new_pub_id));
            }
        }
    }

    // 创建 Edition
    sqlx::query(
        "INSERT INTO editions (id, work_id, edition_title, language, publisher, publisher_id, publish_year, publish_date_text, intro, format_summary, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
    )
    .bind(edition_id)
    .bind(work_id)
    .bind(&clean_title)
    .bind(&primary_lang)
    .bind(item.raw_publisher.as_deref())
    .bind(publisher_id)
    .bind(pub_year)
    .bind(item.raw_year.as_deref())
    .bind(item.intro.as_deref())
    .bind(clean_fmt.as_deref())
    .bind(res_status.as_str())
    .execute(&mut **tx)
    .await?;

    // 写入标识符
    if let Some(ref doi) = norm_doi {
        sqlx::query(
            "INSERT INTO identifiers (id, object_type, object_id, identifier_type, raw_value, normalized_value, is_valid) \
             VALUES ($1, 'edition', $2, 'doi', $3, $4, TRUE)"
        )
        .bind(Uuid::new_v4())
        .bind(edition_id)
        .bind(item.raw_doi.as_deref().unwrap_or(doi))
        .bind(doi)
        .execute(&mut **tx)
        .await?;
    }

    for isbn in &isbns {
        sqlx::query(
            "INSERT INTO identifiers (id, object_type, object_id, identifier_type, raw_value, normalized_value, is_valid) \
             VALUES ($1, 'edition', $2, 'isbn13', $3, $4, TRUE)"
        )
        .bind(Uuid::new_v4())
        .bind(edition_id)
        .bind(isbn.as_str())
        .bind(isbn.as_str())
        .execute(&mut **tx)
        .await?;
    }

    if let Some(ref ext_id) = item.external_id {
        sqlx::query(
            "INSERT INTO identifiers (id, object_type, object_id, identifier_type, raw_value, normalized_value, is_valid) \
             VALUES ($1, 'source_record', $2, 'external_id', $3, $3, TRUE)"
        )
        .bind(Uuid::new_v4())
        .bind(source_record_id)
        .bind(ext_id)
        .execute(&mut **tx)
        .await?;
    }

    // 写入贡献者（作者）
    if let Some(ref author) = item.raw_author {
        let clean_author = clean_text(author);
        if !clean_author.is_empty() {
            let norm_auth = normalize_person(&clean_author);
            let contrib_id = Uuid::new_v4();
            let actual_contrib_id: Uuid = sqlx::query_scalar(
                "INSERT INTO contributors (id, name, normalized_name) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (normalized_name) DO UPDATE SET name = EXCLUDED.name \
                 RETURNING id",
            )
            .bind(contrib_id)
            .bind(&clean_author)
            .bind(&norm_auth)
            .fetch_one(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO edition_contributors (id, edition_id, contributor_id, role, sort_order) \
                 VALUES ($1, $2, $3, $4, 0) \
                 ON CONFLICT (edition_id, contributor_id, role) DO NOTHING"
            )
            .bind(Uuid::new_v4())
            .bind(edition_id)
            .bind(actual_contrib_id)
            .bind(ContributorRole::Author.as_str())
            .execute(&mut **tx)
            .await?;
        }
    }

    // 写入主题分类
    if let Some(ref cat) = item.raw_category {
        let clean_cat = clean_text(cat);
        if !clean_cat.is_empty() {
            let subj_id = Uuid::new_v4();
            let actual_subj_id: Uuid = sqlx::query_scalar(
                "INSERT INTO subjects (id, subject_type, name) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (subject_type, name) DO UPDATE SET name = EXCLUDED.name \
                 RETURNING id",
            )
            .bind(subj_id)
            .bind(SubjectType::Category.as_str())
            .bind(&clean_cat)
            .fetch_one(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO edition_subjects (id, edition_id, subject_id) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (edition_id, subject_id) DO NOTHING",
            )
            .bind(Uuid::new_v4())
            .bind(edition_id)
            .bind(actual_subj_id)
            .execute(&mut **tx)
            .await?;
        }
    }

    // 绑定映射、候选文件与获取目标
    let mut res = attach_source_and_assets(
        tx,
        source_record_id,
        work_id,
        edition_id,
        clean_fmt.as_deref(),
        item.filesize,
        norm_md5.as_deref(),
        "new_record",
        1.0,
        res_status,
    )
    .await?;
    res.is_new_work = true;
    res.is_new_edition = true;

    Ok(res)
}

#[allow(clippy::too_many_arguments)]
async fn attach_source_and_assets(
    tx: &mut Transaction<'_, Postgres>,
    source_record_id: Uuid,
    work_id: Uuid,
    edition_id: Uuid,
    format: Option<&str>,
    filesize: Option<i64>,
    md5: Option<&str>,
    match_method: &str,
    confidence: f64,
    status: ResolutionStatus,
) -> AppResult<ResolutionResult> {
    // 进入“我的书目总库”的显式导入会把已存在的候选版本转为已拥有；仅下载
    // 调度创建的候选版本保持 owned_at 为空，直至文件校验成功。
    sqlx::query(
        "UPDATE editions SET owned_at = COALESCE(owned_at, now()), updated_at = now() WHERE id = $1",
    )
    .bind(edition_id)
    .execute(&mut **tx)
    .await?;

    // 写入消歧映射
    sqlx::query(
        "INSERT INTO record_resolutions (id, source_record_id, work_id, edition_id, match_method, confidence, rule_version, is_manual) \
         VALUES ($1, $2, $3, $4, $5, $6, 'v1', FALSE) \
         ON CONFLICT (source_record_id) DO UPDATE SET \
             work_id = EXCLUDED.work_id, \
             edition_id = EXCLUDED.edition_id, \
             match_method = EXCLUDED.match_method, \
             confidence = EXCLUDED.confidence, \
             updated_at = now()"
    )
    .bind(Uuid::new_v4())
    .bind(source_record_id)
    .bind(work_id)
    .bind(edition_id)
    .bind(match_method)
    .bind(confidence)
    .execute(&mut **tx)
    .await?;

    // 写入来源候选资产
    if let Some(fmt) = format {
        let asset_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO source_assets (id, source_record_id, format, declared_size_bytes, md5, status) \
             VALUES ($1, $2, $3, $4, $5, '可用')"
        )
        .bind(asset_id)
        .bind(source_record_id)
        .bind(fmt)
        .bind(filesize)
        .bind(md5)
        .execute(&mut **tx)
        .await?;

        // 检查是否有匹配的已入库馆藏文件（按 MD5）
        if let Some(md5_str) = md5 {
            let matching_file: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM library_files WHERE md5 = $1 AND verify_status = '有效' LIMIT 1",
            )
            .bind(md5_str)
            .fetch_optional(&mut **tx)
            .await?;

            if let Some(lib_file_id) = matching_file {
                sqlx::query(
                    "INSERT INTO holdings (id, edition_id, library_file_id, source_asset_id, match_type, meets_strategy) \
                     VALUES ($1, $2, $3, $4, 'MD5命中', TRUE) \
                     ON CONFLICT (edition_id, library_file_id) DO NOTHING"
                )
                .bind(Uuid::new_v4())
                .bind(edition_id)
                .bind(lib_file_id)
                .bind(asset_id)
                .execute(&mut **tx)
                .await?;
            }
        }
    }

    Ok(ResolutionResult {
        work_id,
        edition_id,
        is_new_work: false,
        is_new_edition: false,
        match_method: match_method.to_string(),
        confidence,
        status,
    })
}

/// 显式创建补文件任务时，确保版本对应的获取目标存在并处于正确状态。
///
/// 总库导入不再自动调用本函数：总库中的书已经属于“已拥有”，没有 NAS 文件也不
/// 等于待下载。只有用户主动发起补文件任务时才应建立获取目标。
pub async fn ensure_acquisition_target(
    tx: &mut Transaction<'_, Postgres>,
    edition_id: Uuid,
) -> AppResult<()> {
    let holding_exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM holdings WHERE edition_id = $1 AND meets_strategy LIMIT 1",
    )
    .bind(edition_id)
    .fetch_optional(&mut **tx)
    .await?;

    let initial_status = if holding_exists.is_some() {
        AcquisitionStatus::Acquired
    } else {
        AcquisitionStatus::Pending
    };

    sqlx::query(
        "INSERT INTO acquisition_targets (id, edition_id, status, priority, satisfied_holding_id) \
         VALUES ($1, $2, $3, 0, $4) \
         ON CONFLICT (edition_id) DO UPDATE SET \
             status = CASE \
                 WHEN EXCLUDED.status = '已下载' THEN '已下载' \
                 ELSE acquisition_targets.status \
             END, \
             satisfied_holding_id = COALESCE(EXCLUDED.satisfied_holding_id, acquisition_targets.satisfied_holding_id), \
             updated_at = now()"
    )
    .bind(Uuid::new_v4())
    .bind(edition_id)
    .bind(initial_status.as_str())
    .bind(holding_exists)
    .execute(&mut **tx)
    .await?;

    Ok(())
}
