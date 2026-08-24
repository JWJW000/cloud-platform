//! 下载文件校验（第 9.2 节入库前的第一道闸，第 15.3 节补充签名校验与结构化证据）。
//!
//! 四项检查缺一不可：
//! 1. **扩展名**必须是目标格式——站点会把 epub 链接挂在 pdf 卡片上；
//! 2. **文件名必须与目标书名匹配**——多槽位共享暂存目录时，防止把别的任务
//!    下载的书误记为本任务成功；
//! 3. **大小下限**——站点偶尔返回几 KB 的错误页而不是图书；
//! 4. **文件签名**——大小达标也可能是一张登录跳转页或一段 JSON 错误。
//!    只有真正读到 `%PDF-` 或 ZIP/OCF 头，才能说「下到的是一本书」。
//!
//! [`verify_and_collect`] 把这四项检查连同 SHA-256 一起做完，返回
//! [`FileEvidence`]。这份证据是 Worker 上报、Master 记账、事后追责共同依赖的
//! 事实来源：只写一个「校验通过」的布尔值，出问题时没人能复原当时到底校验了什么。

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// 校验失败的原因。文本会进入执行记录，并被 `classify_failure` 判为不可重试。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// 扩展名与目标格式不一致。
    #[error("文件名不匹配：扩展名为 `{actual}`，期望 `{expected}`")]
    UnexpectedExtension {
        /// 实际扩展名。
        actual: String,
        /// 期望扩展名。
        expected: String,
    },
    /// 文件名与目标书名不匹配。
    #[error("文件名不匹配：`{file_name}` 与目标书名《{title}》不符")]
    TitleMismatch {
        /// 实际文件名。
        file_name: String,
        /// 目标书名。
        title: String,
    },
    /// 文件过小，疑似错误页。
    #[error("文件过小：{size} 字节，低于下限 {minimum} 字节")]
    TooSmall {
        /// 实际大小。
        size: u64,
        /// 允许的最小大小。
        minimum: u64,
    },
    /// 文件内容不是声明的格式（第 15.3 节）。
    ///
    /// 这条比扩展名检查更根本：站点在配额耗尽或会话过期时会返回一张 HTML 页，
    /// 而它的文件名和扩展名都可能完全正确。
    #[error("文件签名不符合 {format} 格式：{detail}")]
    BadSignature {
        /// 期望格式。
        format: String,
        /// 中文细节，写入执行记录。
        detail: String,
    },
    /// 文件读不出来（不存在、权限不足、正在被写入）。
    #[error("文件无法读取：{detail}")]
    Unreadable {
        /// 中文细节。
        detail: String,
    },
}

/// 校验暂存目录中的下载文件是否可以入库。
pub fn verify_downloaded_file(
    path: &Path,
    title: &str,
    expected_format: &str,
    size_bytes: u64,
    minimum_size_bytes: u64,
) -> Result<(), VerifyError> {
    let expected = expected_format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let actual = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if actual != expected {
        return Err(VerifyError::UnexpectedExtension { actual, expected });
    }

    if size_bytes < minimum_size_bytes {
        return Err(VerifyError::TooSmall {
            size: size_bytes,
            minimum: minimum_size_bytes,
        });
    }

    if !filename_matches_title(path, title) {
        return Err(VerifyError::TitleMismatch {
            file_name: path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            title: title.to_string(),
        });
    }

    Ok(())
}

/// 判断下载到的文件名是否指向目标书（归一化后的模糊匹配）。
///
/// 规则沿用现有桌面版的实战经验：
/// - 双向包含：站点标题可能带「第 2 版」等后缀，也可能被 CSV 书名包含；
/// - 冠词容忍：站点会省略书名开头的 `On`/`The`/`A`/`An`；
/// - 过短书名（归一化后不足 3 字符）无法可靠判定，直接放行。
pub fn filename_matches_title(path: &Path, title: &str) -> bool {
    title_match_basis(path, title).is_some()
}

/// 与 [`filename_matches_title`] 同一套规则，但返回**命中的是哪一条规则**。
///
/// 匹配依据必须能写进证据：「文件名与书名匹配」这句话在事后毫无价值，
/// 而「书名被文件名包含」和「过短书名跳过校验」是两种完全不同的可信度。
pub fn title_match_basis(path: &Path, title: &str) -> Option<String> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let name_norm = normalize_title(name);
    let title_norm = normalize_title(title);

    if title_norm.chars().count() < 3 {
        return Some("书名过短，跳过文件名校验".to_string());
    }
    if name_norm.contains(&title_norm) {
        return Some("文件名包含目标书名".to_string());
    }
    if name_norm.chars().count() >= 3 && title_norm.contains(&name_norm) {
        return Some("目标书名包含文件名".to_string());
    }
    for leading in ["on", "the", "a", "an"] {
        if let Some(rest) = title_norm.strip_prefix(leading) {
            if !rest.is_empty() && name_norm.contains(rest) {
                return Some(format!("忽略书名前导冠词 `{leading}` 后匹配"));
            }
        }
    }
    None
}

/// 归一化书名/文件名：去掉空白与常见标点，统一小写。
///
/// 站点标题里的「（第3版）」「[美]」「·」这类噪声不承载身份信息，
/// 留着它们只会让同一本书在不同页面上看起来是两本。
pub fn normalize_title(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace() && !"（）()【】《》,.，。-_·：:、".contains(*c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// macOS 在 ExFAT 等外部卷上会为每个文件生成 `._` AppleDouble 伴生文件，
/// 扫描暂存目录时必须过滤，否则会误判「下载文件与书名不匹配」。
pub fn is_companion_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    name.starts_with("._") || name == ".DS_Store"
}

/// 浏览器下载中的临时文件（`.crdownload`/`.part`/`.tmp`）。
pub fn is_partial_download(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".crdownload") || name.ends_with(".part") || name.ends_with(".tmp")
}

/// 一次文件校验的结构化证据（第 15.3 节）。
///
/// Worker 把它转成 `FileEvidence` 上报，Master 用其中的 `sha256`、`size_bytes`、
/// `format` 与任务记录逐项比对后才敢写 `book_files`。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileEvidence {
    /// 暂存目录中的文件名。
    pub file_name: String,
    /// 字节数（以实际读到的为准，不用站点声明的值）。
    pub size_bytes: u64,
    /// 内容 SHA-256，小写十六进制。
    pub sha256: String,
    /// 校验通过的格式（`pdf`/`epub`）。
    pub format: String,
    /// 签名校验的中文结论，例如「PDF 文件头 `%PDF-1.7`」。
    pub signature_detail: String,
    /// 文件名与书名的匹配依据。
    pub title_basis: String,
    /// 校验完成时刻。
    pub verified_at: chrono::DateTime<chrono::Utc>,
}

/// 目前支持签名校验的格式。
const SIGNATURE_HEAD_BYTES: usize = 1024;

/// 校验文件内容确实是声明的格式，返回中文结论（第 15.3 节）。
///
/// PDF 允许 `%PDF-` 出现在文件开头 1024 字节内（规范允许前置垃圾字节，
/// 而站点的下载常带 BOM 或空行）；EPUB 必须是 ZIP 容器，
/// 并优先按 OCF 规范核对首个条目 `mimetype` 的内容。
pub fn check_signature(path: &Path, expected_format: &str) -> Result<String, VerifyError> {
    let format = expected_format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let head = read_head(path, SIGNATURE_HEAD_BYTES)?;

    match format.as_str() {
        "pdf" => match find(&head, b"%PDF-") {
            Some(at) => {
                let version: String = head[at + 5..]
                    .iter()
                    .take(3)
                    .map(|b| *b as char)
                    .filter(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                Ok(if version.is_empty() {
                    "PDF 文件头 `%PDF-`".to_string()
                } else {
                    format!("PDF 文件头 `%PDF-{version}`")
                })
            }
            None => Err(VerifyError::BadSignature {
                format,
                detail: format!(
                    "开头 {} 字节内未找到 `%PDF-`，{}",
                    head.len(),
                    describe(&head)
                ),
            }),
        },
        "epub" => {
            if !head.starts_with(b"PK\x03\x04") {
                return Err(VerifyError::BadSignature {
                    format,
                    detail: format!("不是 ZIP 容器，{}", describe(&head)),
                });
            }
            // OCF 规定首个条目必须是未压缩的 `mimetype`，内容为 application/epub+zip。
            // 本地文件头固定 30 字节，其后紧跟文件名。
            const NAME_AT: usize = 30;
            const MIME: &[u8] = b"application/epub+zip";
            if head.len() >= NAME_AT + 8 + MIME.len() && &head[NAME_AT..NAME_AT + 8] == b"mimetype"
            {
                let value = &head[NAME_AT + 8..NAME_AT + 8 + MIME.len()];
                if value == MIME {
                    return Ok("EPUB OCF 容器，mimetype 为 application/epub+zip".to_string());
                }
                return Err(VerifyError::BadSignature {
                    format,
                    detail: format!(
                        "ZIP 首个条目 mimetype 内容为 `{}`，不是 application/epub+zip",
                        String::from_utf8_lossy(value)
                    ),
                });
            }
            // 少数站点重新打包后 mimetype 不在首位。这仍然是一个 ZIP，
            // 不是错误页，因此放行，但把差异写进证据而不是悄悄当作标准 EPUB。
            Ok("ZIP 容器（首个条目非 mimetype，未按 OCF 规范打包）".to_string())
        }
        other => Err(VerifyError::BadSignature {
            format: other.to_string(),
            detail: "不支持的目标格式，仅支持 pdf 与 epub".to_string(),
        }),
    }
}

/// 完整校验暂存文件并产出结构化证据（第 15.3 节）。
///
/// 顺序是刻意的：先做便宜的元数据检查，最后才读全文算 SHA-256。
/// 一本几百 MB 的书，没必要为了发现「扩展名不对」而先哈希一遍。
pub fn verify_and_collect(
    path: &Path,
    title: &str,
    expected_format: &str,
    minimum_size_bytes: u64,
) -> Result<FileEvidence, VerifyError> {
    let metadata = std::fs::metadata(path).map_err(|err| VerifyError::Unreadable {
        detail: format!("读取文件信息失败：{err}"),
    })?;
    if !metadata.is_file() {
        return Err(VerifyError::Unreadable {
            detail: "目标路径不是普通文件".to_string(),
        });
    }
    let size_bytes = metadata.len();

    verify_downloaded_file(path, title, expected_format, size_bytes, minimum_size_bytes)?;
    let signature_detail = check_signature(path, expected_format)?;

    Ok(FileEvidence {
        file_name: path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string(),
        size_bytes,
        sha256: sha256_file(path)?,
        format: expected_format
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase(),
        signature_detail,
        title_basis: title_match_basis(path, title).unwrap_or_else(|| "未匹配".to_string()),
        verified_at: chrono::Utc::now(),
    })
}

/// 流式计算文件 SHA-256，小写十六进制。
///
/// 分块读取而不是一次读进内存：图书文件可以有几百 MB，
/// 而 Worker 上会有多个槽位同时走到这一步。
pub fn sha256_file(path: &Path) -> Result<String, VerifyError> {
    let mut file = std::fs::File::open(path).map_err(|err| VerifyError::Unreadable {
        detail: format!("打开文件失败：{err}"),
    })?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|err| VerifyError::Unreadable {
            detail: format!("读取文件内容失败：{err}"),
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn read_head(path: &Path, limit: usize) -> Result<Vec<u8>, VerifyError> {
    let mut file = std::fs::File::open(path).map_err(|err| VerifyError::Unreadable {
        detail: format!("打开文件失败：{err}"),
    })?;
    let mut buf = vec![0u8; limit];
    let mut filled = 0;
    while filled < limit {
        let read = file
            .read(&mut buf[filled..])
            .map_err(|err| VerifyError::Unreadable {
                detail: format!("读取文件头失败：{err}"),
            })?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    buf.truncate(filled);
    Ok(buf)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 给失败原因加一句「那它到底是什么」，否则排查只能靠猜。
fn describe(head: &[u8]) -> String {
    let lowered: Vec<u8> = head
        .iter()
        .take(256)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    if find(&lowered, b"<!doctype html").is_some() || find(&lowered, b"<html").is_some() {
        return "实际内容是 HTML 页面（很可能是登录页或错误页）".to_string();
    }
    if head.starts_with(b"{") || head.starts_with(b"[") {
        return "实际内容像是 JSON 响应".to_string();
    }
    if head.starts_with(b"PK\x03\x04") {
        return "实际内容是 ZIP 容器".to_string();
    }
    format!(
        "开头字节为 `{}`",
        String::from_utf8_lossy(&head.iter().take(16).copied().collect::<Vec<u8>>()).escape_debug()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const MIN: u64 = 32 * 1024;

    #[test]
    fn accepts_matching_file() {
        let path = PathBuf::from("Python编程：从入门到实践.pdf");
        verify_downloaded_file(&path, "Python编程", "pdf", 2_000_000, MIN).unwrap();
    }

    #[test]
    fn rejects_wrong_extension() {
        let path = PathBuf::from("算法导论.epub");
        let err = verify_downloaded_file(&path, "算法导论", "pdf", 2_000_000, MIN).unwrap_err();
        assert!(matches!(err, VerifyError::UnexpectedExtension { .. }));
        // 归类为不可重试，避免反复下载同一个错误文件
        assert_eq!(
            platform_domain::classify_failure(&err.to_string(), None),
            platform_domain::FailureClass::Fatal
        );
    }

    #[test]
    fn rejects_other_task_file() {
        let path = PathBuf::from("Cosmos (Carl Sagan).pdf");
        let err =
            verify_downloaded_file(&path, "Pale Blue Dot", "pdf", 2_000_000, MIN).unwrap_err();
        assert!(matches!(err, VerifyError::TitleMismatch { .. }));
    }

    #[test]
    fn rejects_error_page_sized_file() {
        let path = PathBuf::from("算法导论.pdf");
        let err = verify_downloaded_file(&path, "算法导论", "pdf", 1024, MIN).unwrap_err();
        assert!(matches!(err, VerifyError::TooSmall { .. }));
    }

    #[test]
    fn tolerates_edition_suffix_and_leading_article() {
        assert!(filename_matches_title(
            &PathBuf::from("水利工程建设投资控制 第2版 (作者).pdf"),
            "水利工程建设投资控制"
        ));
        assert!(filename_matches_title(
            &PathBuf::from("The Origin Of Species 150th Anniversary Edition.epub"),
            "On the Origin of Species"
        ));
    }

    #[test]
    fn short_titles_bypass_name_check() {
        assert!(filename_matches_title(
            &PathBuf::from("随便什么.pdf"),
            "电路"
        ));
    }

    #[test]
    fn filters_companion_and_partial_files() {
        assert!(is_companion_file(&PathBuf::from("._算法导论.pdf")));
        assert!(is_companion_file(&PathBuf::from(".DS_Store")));
        assert!(is_partial_download(&PathBuf::from(
            "算法导论.pdf.crdownload"
        )));
        assert!(!is_partial_download(&PathBuf::from("算法导论.pdf")));
    }

    /// 造一个体积达标、扩展名正确、文件名也对，但内容是 `body` 的文件。
    fn write_file(dir: &Path, name: &str, body: &[u8], pad_to: usize) -> PathBuf {
        let path = dir.join(name);
        let mut bytes = body.to_vec();
        bytes.resize(bytes.len().max(pad_to), b' ');
        std::fs::write(&path, &bytes).unwrap();
        path
    }

    fn epub_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PK\x03\x04");
        bytes.extend_from_slice(&[0u8; 26]); // 本地文件头其余部分
        bytes.extend_from_slice(b"mimetype");
        bytes.extend_from_slice(b"application/epub+zip");
        bytes
    }

    #[test]
    fn html_error_page_with_a_correct_name_is_rejected() {
        // 最隐蔽的失败：名字对、扩展名对、大小也够，内容却是登录页。
        // 只查大小的校验会把它当成一本书写进 NAS。
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "算法导论.pdf",
            b"<!DOCTYPE html><html><body>\xe8\xaf\xb7\xe7\x99\xbb\xe5\xbd\x95</body></html>",
            64 * 1024,
        );
        let err = verify_and_collect(&path, "算法导论", "pdf", MIN).unwrap_err();
        match err {
            VerifyError::BadSignature { ref detail, .. } => {
                assert!(detail.contains("HTML"), "原因应指出实际是 HTML：{detail}");
            }
            other => panic!("应判为签名不符，实际 {other:?}"),
        }
    }

    #[test]
    fn real_pdf_produces_evidence_with_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "算法导论.pdf",
            b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n",
            64 * 1024,
        );
        let evidence = verify_and_collect(&path, "算法导论", "pdf", MIN).unwrap();

        assert_eq!(evidence.file_name, "算法导论.pdf");
        assert_eq!(evidence.size_bytes, 64 * 1024);
        assert_eq!(evidence.format, "pdf");
        assert_eq!(evidence.sha256.len(), 64);
        assert_eq!(evidence.sha256, sha256_file(&path).unwrap());
        assert!(evidence.signature_detail.contains("1.7"), "{evidence:?}");
        assert_eq!(evidence.title_basis, "文件名包含目标书名");
    }

    #[test]
    fn epub_requires_the_ocf_mimetype() {
        let dir = tempfile::tempdir().unwrap();
        let good = write_file(dir.path(), "算法导论.epub", &epub_bytes(), 64 * 1024);
        let evidence = verify_and_collect(&good, "算法导论", "epub", MIN).unwrap();
        assert!(evidence.signature_detail.contains("OCF"), "{evidence:?}");

        let mut wrong = epub_bytes();
        let at = 30 + 8;
        wrong[at..at + 20].copy_from_slice(b"application/zip     ");
        let bad = write_file(dir.path(), "算法导论2.epub", &wrong, 64 * 1024);
        assert!(matches!(
            verify_and_collect(&bad, "算法导论2", "epub", MIN),
            Err(VerifyError::BadSignature { .. })
        ));
    }

    #[test]
    fn pdf_extension_on_epub_content_is_rejected() {
        // 站点把 epub 链接挂在 pdf 卡片上：扩展名骗过第一道闸，签名骗不过
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "算法导论.pdf", &epub_bytes(), 64 * 1024);
        let err = verify_and_collect(&path, "算法导论", "pdf", MIN).unwrap_err();
        match err {
            VerifyError::BadSignature { ref detail, .. } => {
                assert!(detail.contains("ZIP"), "{detail}");
            }
            other => panic!("应判为签名不符，实际 {other:?}"),
        }
    }

    #[test]
    fn wrong_extension_fails_before_content_is_read() {
        // 元数据检查在签名与哈希之前：内容是完好的 PDF 也救不了错误的扩展名
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "算法导论.epub", b"%PDF-1.7\n", 64 * 1024);
        assert!(matches!(
            verify_and_collect(&path, "算法导论", "pdf", MIN),
            Err(VerifyError::UnexpectedExtension { .. })
        ));
    }

    #[test]
    fn missing_file_reports_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let err = verify_and_collect(&dir.path().join("没有这本.pdf"), "没有这本", "pdf", MIN)
            .unwrap_err();
        assert!(matches!(err, VerifyError::Unreadable { .. }));
    }

    #[test]
    fn unsupported_format_is_rejected_not_assumed_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "算法导论.mobi", b"anything", 64 * 1024);
        assert!(matches!(
            check_signature(&path, "mobi"),
            Err(VerifyError::BadSignature { .. })
        ));
    }
}
