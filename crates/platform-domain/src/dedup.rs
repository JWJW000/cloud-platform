//! 第 8.2 节：全局图书去重键。
//!
//! 规则优先级：
//! 1. 有合法 ISBN → 以规范化 ISBN-13 作为强唯一键，核验状态 `已确认`；
//! 2. 无 ISBN 但书名、作者、出版社齐全 → 三者规范化组合，核验状态 `已确认`；
//! 3. 无 ISBN 且作者或出版社缺失 → 仅按书名归并，核验状态 `待确认`，等待人工确认。
//!
//! 规范化只用于匹配，**不覆盖原始导入文本**：`BookIdentity` 同时保留原文与去重键。

use crate::enums::VerifyStatus;
use crate::isbn::normalize_isbn;

/// 去重键。`storage_key` 会写入数据库唯一索引。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupKey {
    /// 依据规范化 ISBN-13。
    Isbn(String),
    /// 依据「书名 + 作者 + 出版社」。
    TitleAuthorPublisher(String),
    /// 仅依据书名，需要人工确认。
    TitleOnly(String),
}

impl DedupKey {
    /// 数据库存储形态。前缀保证三类键不会互相冲突。
    pub fn storage_key(&self) -> String {
        match self {
            Self::Isbn(value) => format!("isbn:{value}"),
            Self::TitleAuthorPublisher(value) => format!("tap:{value}"),
            Self::TitleOnly(value) => format!("title:{value}"),
        }
    }

    /// 该键是否足以自动确认为同一本书。
    pub fn verify_status(&self) -> VerifyStatus {
        match self {
            Self::Isbn(_) | Self::TitleAuthorPublisher(_) => VerifyStatus::Confirmed,
            Self::TitleOnly(_) => VerifyStatus::NeedsConfirm,
        }
    }
}

/// 一条导入记录的图书身份：保留原文，同时给出规范化字段与去重键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookIdentity {
    /// 原始书名（导入文本，展示用）。
    pub raw_title: String,
    /// 原始作者。
    pub raw_author: Option<String>,
    /// 原始出版社。
    pub raw_publisher: Option<String>,
    /// 原始 ISBN 文本。
    pub raw_isbn: Option<String>,
    /// 规范化书名（匹配用）。
    pub normalized_title: String,
    /// 规范化作者。
    pub normalized_author: Option<String>,
    /// 规范化出版社。
    pub normalized_publisher: Option<String>,
    /// 通过校验位验证的 ISBN-13；非法 ISBN 会被丢弃并退化匹配。
    pub normalized_isbn: Option<String>,
    /// 去重键。
    pub dedup_key: DedupKey,
    /// 核验状态。
    pub verify_status: VerifyStatus,
}

impl BookIdentity {
    /// 由导入的原始字段计算图书身份。
    pub fn from_raw(
        title: &str,
        author: Option<&str>,
        publisher: Option<&str>,
        isbn: Option<&str>,
    ) -> Option<Self> {
        let normalized_title = normalize_title(title);
        if normalized_title.is_empty() {
            // 书名为空的行无法参与去重，交由导入层记录为无效行。
            return None;
        }

        let normalized_author = author.map(normalize_person).filter(|s| !s.is_empty());
        let normalized_publisher = publisher.map(normalize_person).filter(|s| !s.is_empty());
        let normalized_isbn = isbn
            .and_then(normalize_isbn)
            .map(|value| value.as_str().to_string());

        let dedup_key = match (
            normalized_isbn.as_deref(),
            normalized_author.as_deref(),
            normalized_publisher.as_deref(),
        ) {
            (Some(isbn), _, _) => DedupKey::Isbn(isbn.to_string()),
            (None, Some(author), Some(publisher)) => {
                DedupKey::TitleAuthorPublisher(format!("{normalized_title}|{author}|{publisher}"))
            }
            _ => DedupKey::TitleOnly(normalized_title.clone()),
        };
        let verify_status = dedup_key.verify_status();

        Some(Self {
            raw_title: title.trim().to_string(),
            raw_author: author
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            raw_publisher: publisher
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            raw_isbn: isbn.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            normalized_title,
            normalized_author,
            normalized_publisher,
            normalized_isbn,
            dedup_key,
            verify_status,
        })
    }

    /// 数据库存储用的去重键字符串。
    pub fn storage_key(&self) -> String {
        self.dedup_key.storage_key()
    }
}

/// 书名规范化：全角转半角、去除空白与标点、统一小写。
///
/// 刻意 **不** 剥离「第 2 版」「上册」等版次信息：那属于不同实体的判断，
/// 应交由管理员的「合并图书 / 拆分图书」操作决定。
pub fn normalize_title(raw: &str) -> String {
    raw.chars()
        .map(fullwidth_to_halfwidth)
        .filter(|c| !c.is_whitespace() && !is_ignorable_punctuation(*c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 作者与出版社规范化。额外去掉「出版社」「大学出版社」之类的后缀差异容易误合并，
/// 因此只做与书名一致的字符级规范化。
pub fn normalize_person(raw: &str) -> String {
    normalize_title(raw)
}

fn fullwidth_to_halfwidth(ch: char) -> char {
    match ch as u32 {
        // 全角 ！(FF01) ~ ～(FF5E) 映射到 ASCII 0x21 ~ 0x7E
        code @ 0xFF01..=0xFF5E => char::from_u32(code - 0xFEE0).unwrap_or(ch),
        // 全角空格
        0x3000 => ' ',
        _ => ch,
    }
}

fn is_ignorable_punctuation(ch: char) -> bool {
    const PUNCTUATION: &str =
        "()[]{}【】《》〈〉「」『』,.;:!?、。，．；：！？—–-_·~`'\"“”‘’/\\|*+=&#@$%^";
    PUNCTUATION.contains(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_isbn_wins_over_title() {
        let identity =
            BookIdentity::from_raw("算法导论", Some("Cormen"), None, Some("978-7-111-40701-0"))
                .unwrap();
        assert_eq!(
            identity.dedup_key,
            DedupKey::Isbn("9787111407010".to_string())
        );
        assert_eq!(identity.storage_key(), "isbn:9787111407010");
        assert_eq!(identity.verify_status, VerifyStatus::Confirmed);
    }

    #[test]
    fn falls_back_to_title_author_publisher() {
        let identity =
            BookIdentity::from_raw("计算机网络", Some("谢希仁"), Some("电子工业出版社"), None)
                .unwrap();
        assert_eq!(identity.verify_status, VerifyStatus::Confirmed);
        assert_eq!(
            identity.storage_key(),
            "tap:计算机网络|谢希仁|电子工业出版社"
        );
    }

    #[test]
    fn title_only_needs_manual_confirm() {
        let identity = BookIdentity::from_raw("电路", None, None, None).unwrap();
        assert_eq!(identity.dedup_key, DedupKey::TitleOnly("电路".to_string()));
        assert_eq!(identity.verify_status, VerifyStatus::NeedsConfirm);
    }

    #[test]
    fn invalid_isbn_degrades_instead_of_merging() {
        // 脏 ISBN（订货号）不得作为强唯一键，否则会把不同的书合并
        let a = BookIdentity::from_raw("图书甲", Some("作者甲"), Some("出版社甲"), Some("12345"))
            .unwrap();
        let b = BookIdentity::from_raw("图书乙", Some("作者乙"), Some("出版社乙"), Some("12345"))
            .unwrap();
        assert_ne!(a.storage_key(), b.storage_key());
        assert!(a.normalized_isbn.is_none());
    }

    #[test]
    fn normalization_matches_across_punctuation_and_width() {
        let a = BookIdentity::from_raw("Python 编程：从入门到实践", None, None, None).unwrap();
        let b = BookIdentity::from_raw("ＰＹＴＨＯＮ编程,从入门到实践", None, None, None).unwrap();
        assert_eq!(a.storage_key(), b.storage_key());
    }

    #[test]
    fn keeps_raw_text_untouched() {
        let identity = BookIdentity::from_raw("  算法导论 (第3版) ", None, None, None).unwrap();
        assert_eq!(identity.raw_title, "算法导论 (第3版)");
        assert_eq!(identity.normalized_title, "算法导论第3版");
    }

    #[test]
    fn empty_title_is_rejected() {
        assert!(BookIdentity::from_raw("   ", None, None, None).is_none());
    }
}
