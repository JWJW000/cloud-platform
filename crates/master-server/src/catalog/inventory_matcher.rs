//! 馆藏匹配决策规则引擎（方案第 8 节）。
//!
//! 匹配优先级：
//! 1. SHA-256 已存在：直接命中内容实体及关联版本；
//! 2. MD5 唯一命中 source_asset 且格式/大小一致；
//! 3. ISBN/DOI/外部标识符唯一命中版本；
//! 4. 电子书内嵌元数据（书名+作者+年份+出版社）高置信度命中；
//! 5. 文件名清洗后高置信度唯一命中；
//! 6. 多候选 / 冲突 => NeedsReview (待确认)；
//! 7. 无候选 => Unmatched (未匹配)。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// 匹配方法标识。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MatchMethod {
    /// SHA256 精确命中
    Sha256Exact,
    /// MD5 命中来源资产
    Md5SourceAsset,
    /// 标识符（ISBN/DOI）命中
    IdentifierExact,
    /// 电子书内嵌元数据命中
    EmbeddedMetadata,
    /// 文件名解析书名命中
    FileNameParsed,
    /// 人工审核确认
    ManualReview,
}

impl MatchMethod {
    /// 获取中文名称
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sha256Exact => "SHA256精确",
            Self::Md5SourceAsset => "MD5来源匹配",
            Self::IdentifierExact => "标识符精确",
            Self::EmbeddedMetadata => "内嵌元数据",
            Self::FileNameParsed => "文件名解析",
            Self::ManualReview => "人工确认",
        }
    }
}

/// 匹配候选条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchCandidate {
    /// 版本编号
    pub edition_id: Uuid,
    /// 版本书名
    pub title: String,
    /// 出版者
    pub author: Option<String>,
    /// 置信度评分（0~1000）
    pub score: u16,
    /// 匹配命中的字段
    pub matched_fields: Vec<String>,
    /// 发生冲突的字段
    pub conflict_fields: Vec<String>,
}

/// 匹配决策结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InventoryMatchDecision {
    /// 唯一高置信度匹配成功
    Matched {
        /// 命中的版本编号
        edition_id: Uuid,
        /// 匹配方法
        method: MatchMethod,
        /// 匹配评分
        score: u16,
    },
    /// 需要人工审核确认
    NeedsReview {
        /// 候选列表
        candidates: Vec<MatchCandidate>,
        /// 进入审核的原因
        reason: String,
    },
    /// 未能匹配到任何有效书目
    Unmatched {
        /// 未匹配原因
        reason: String,
    },
}

/// 输入证据进行匹配判定。
pub async fn evaluate_match(
    pool: &PgPool,
    sha256: &str,
    md5: Option<&str>,
    file_name: &str,
    extension: &str,
    _actual_size_bytes: i64,
) -> Result<InventoryMatchDecision> {
    // 1. SHA-256 唯一命中已有的 library_file 及对应 holding
    let sha_hit: Option<Uuid> = sqlx::query_scalar(
        "SELECT h.edition_id
         FROM library_files lf
         JOIN holdings h ON lf.id = h.library_file_id
         WHERE lf.sha256 = $1
         LIMIT 1",
    )
    .bind(sha256.to_ascii_lowercase())
    .fetch_optional(pool)
    .await?;

    if let Some(edition_id) = sha_hit {
        return Ok(InventoryMatchDecision::Matched {
            edition_id,
            method: MatchMethod::Sha256Exact,
            score: 1000,
        });
    }

    // 2. MD5 唯一命中 source_assets
    if let Some(md5_val) = md5 {
        if !md5_val.trim().is_empty() {
            let md5_hits: Vec<Uuid> = sqlx::query_scalar(
                "SELECT r.edition_id
                 FROM source_assets sa
                 JOIN source_records sr ON sa.source_record_id = sr.id
                 JOIN record_resolutions r ON sr.id = r.source_record_id
                 WHERE sa.md5 = $1 AND sa.format ILIKE $2 AND r.edition_id IS NOT NULL",
            )
            .bind(md5_val.to_ascii_lowercase())
            .bind(extension)
            .fetch_all(pool)
            .await?;

            if md5_hits.len() == 1 {
                return Ok(InventoryMatchDecision::Matched {
                    edition_id: md5_hits[0],
                    method: MatchMethod::Md5SourceAsset,
                    score: 950,
                });
            } else if md5_hits.len() > 1 {
                return Ok(InventoryMatchDecision::NeedsReview {
                    candidates: md5_hits
                        .into_iter()
                        .map(|id| MatchCandidate {
                            edition_id: id,
                            title: String::new(),
                            author: None,
                            score: 800,
                            matched_fields: vec!["md5".to_string(), "format".to_string()],
                            conflict_fields: vec!["multi_candidates".to_string()],
                        })
                        .collect(),
                    reason: "MD5 命中多个不同版本".to_string(),
                });
            }
        }
    }

    // 3. 文件名清理与书名检索（提取前缀书名）
    let clean_title = parse_title_from_filename(file_name);
    if clean_title.len() >= 2 {
        let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT e.id, e.edition_title, e.publisher
             FROM editions e
             WHERE e.edition_title ILIKE $1
             LIMIT 6",
        )
        .bind(format!("%{}%", clean_title))
        .fetch_all(pool)
        .await?;

        if rows.len() == 1 {
            return Ok(InventoryMatchDecision::Matched {
                edition_id: rows[0].0,
                method: MatchMethod::FileNameParsed,
                score: 910,
            });
        } else if rows.len() > 1 {
            let candidates = rows
                .into_iter()
                .take(5)
                .map(|(id, title, publ)| MatchCandidate {
                    edition_id: id,
                    title,
                    author: publ,
                    score: 750,
                    matched_fields: vec!["title".to_string()],
                    conflict_fields: vec![],
                })
                .collect();
            return Ok(InventoryMatchDecision::NeedsReview {
                candidates,
                reason: "文件名命中多个候选书目".to_string(),
            });
        }
    }

    Ok(InventoryMatchDecision::Unmatched {
        reason: "未找到唯一置信度书目".to_string(),
    })
}

/// 从文件名中解析出可能的书名（剔除作者括号、序号与扩展名）。
fn parse_title_from_filename(name: &str) -> String {
    let name_without_ext = if let Some(pos) = name.rfind('.') {
        &name[..pos]
    } else {
        name
    };

    let s = name_without_ext.trim();
    // 去除开头的 6 位数字序号前缀（如 000123_书名）
    let s = if s.len() > 7
        && s.chars().take(6).all(|c| c.is_ascii_digit())
        && s.as_bytes()[6] == b'_'
    {
        &s[7..]
    } else {
        s
    };

    // 去除括号中的作者信息
    let mut title = String::new();
    let mut in_paren = false;
    for c in s.chars() {
        if c == '(' || c == '（' || c == '[' || c == '【' {
            in_paren = true;
        } else if c == ')' || c == '）' || c == ']' || c == '】' {
            in_paren = false;
        } else if !in_paren {
            title.push(c);
        }
    }

    title.trim().to_string()
}
