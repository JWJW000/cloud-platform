//! 图书馆总库规范化与数据清洗工具库。
//!
//! 包含：
//! - 多 ISBN 提取与单项校验；
//! - DOI 规范化（去除前缀、统一小写）；
//! - MD5 / SHA-256 哈希规范化与校验；
//! - 文件格式受控映射；
//! - 标题、作者、出版社、分类文本清洗。

use crate::isbn::{normalize_isbn, Isbn};

/// 从自由文本中提取所有合法的 ISBN-13。
///
/// 来源数据中的 `isbns` 字段常见以逗号、分号、斜杠、空格或竖线分隔多个 ISBN，
/// 本函数将所有片段拆解并分别校验，去重后返回合法集合。
pub fn extract_isbns(raw: &str) -> Vec<Isbn> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for token in raw.split(|c| matches!(c, ',' | ';' | '/' | '|' | '\n' | '\r' | '\t' | '，' | '；' | '、')) {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(isbn) = normalize_isbn(trimmed) {
            if seen.insert(isbn.as_str().to_string()) {
                results.push(isbn);
            }
        }
    }
    results
}

/// 规范化 DOI：去除 `https://doi.org/`、`http://dx.doi.org/`、`doi:` 等常见前缀，
/// 并统一转为小写。
pub fn normalize_doi(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut cleaned = trimmed;
    for prefix in &[
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi.org/",
        "dx.doi.org/",
        "doi:",
        "DOI:",
    ] {
        if cleaned.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
            cleaned = &cleaned[prefix.len()..];
            break;
        }
    }

    let cleaned = cleaned.trim();
    if cleaned.starts_with("10.") && cleaned.contains('/') {
        Some(cleaned.to_ascii_lowercase())
    } else {
        None
    }
}

/// 规范化 32 位 MD5 哈希：去除前后空格、全半角，校验 32 位十六进制字符并转小写。
pub fn normalize_md5(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s.to_ascii_lowercase())
    } else {
        None
    }
}

/// 规范化 64 位 SHA-256 哈希：去除前后空格、校验 64 位十六进制字符并转小写。
pub fn normalize_sha256(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s.to_ascii_lowercase())
    } else {
        None
    }
}

/// 规范化文件扩展名/格式（保持英文小写，去除前导点）。
pub fn normalize_format(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    match trimmed.as_str() {
        "epub" => "epub".to_string(),
        "pdf" => "pdf".to_string(),
        "azw3" | "azw" | "kfx" => "azw3".to_string(),
        "mobi" | "prc" => "mobi".to_string(),
        "djvu" | "djv" => "djvu".to_string(),
        "fb2" | "fb2.zip" => "fb2".to_string(),
        "txt" => "txt".to_string(),
        "cbz" | "cbr" | "cbt" => "cbz".to_string(),
        "docx" | "doc" => "docx".to_string(),
        "rar" | "zip" | "7z" => "zip".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "unknown".to_string(),
    }
}

/// 清洗自由文本（去 HTML 实体、转半角、去控制字符）。
pub fn clean_text(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return s;
    }
    // 基础 HTML 实体替换
    s = s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect::<String>()
        .trim()
        .to_string()
}

/// 规范化年份/出版日期。
pub fn parse_publish_year(raw: &str) -> Option<i32> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // 4位连续数字
    let mut cur_digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur_digits.push(c);
            if cur_digits.len() == 4 {
                if let Ok(year) = cur_digits.parse::<i32>() {
                    if (1000..=2100).contains(&year) {
                        return Some(year);
                    }
                }
            }
        } else {
            cur_digits.clear();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_isbns() {
        let raw = "978-7-111-40701-0, 0-262-03384-4; 9787111407010 / 123456";
        let isbns = extract_isbns(raw);
        assert_eq!(isbns.len(), 2);
        assert_eq!(isbns[0].as_str(), "9787111407010");
        assert_eq!(isbns[1].as_str(), "9780262033848");
    }

    #[test]
    fn test_normalize_doi() {
        assert_eq!(
            normalize_doi("https://doi.org/10.1007/978-3-030-12345-6").unwrap(),
            "10.1007/978-3-030-12345-6"
        );
        assert_eq!(
            normalize_doi("doi:10.1016/j.jvb.2018.01.001").unwrap(),
            "10.1016/j.jvb.2018.01.001"
        );
        assert!(normalize_doi("not-a-doi").is_none());
    }

    #[test]
    fn test_normalize_md5_and_sha256() {
        assert_eq!(
            normalize_md5("d41d8cd98f00b204e9800998ecf8427e").unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert!(normalize_md5("invalid-md5").is_none());

        assert_eq!(
            normalize_sha256("E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(normalize_sha256("short-sha").is_none());
    }

    #[test]
    fn test_normalize_format() {
        assert_eq!(normalize_format(".EPUB"), "epub");
        assert_eq!(normalize_format("PDF"), "pdf");
        assert_eq!(normalize_format("azw3"), "azw3");
        assert_eq!(normalize_format("mobi"), "mobi");
    }

    #[test]
    fn test_parse_publish_year() {
        assert_eq!(parse_publish_year("2021-08-15"), Some(2021));
        assert_eq!(parse_publish_year("2018年9月"), Some(2018));
        assert_eq!(parse_publish_year("1999"), Some(1999));
        assert_eq!(parse_publish_year(""), None);
    }
}
