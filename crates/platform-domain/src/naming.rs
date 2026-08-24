//! 第 8.3 与 9.2 节：NAS 目录结构、最终文件名与「上传中」临时文件名。
//!
//! 文件名前置 6 位图书编号以避免同名冲突；界面展示仍使用原始书名。

use std::path::{Path, PathBuf};

/// NAS 目录布局。相对路径由 Master 持有，绝对挂载路径由各 Worker 本地配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NasLayout {
    /// 图书文件目录，默认 `文件`。
    pub files_dir: String,
    /// 系统目录，默认 `.系统`。
    pub system_dir: String,
    /// 清单子目录，默认 `清单`。
    pub manifest_dir: String,
    /// 文件名中书名部分的最大字符数。
    pub max_title_chars: usize,
}

impl Default for NasLayout {
    fn default() -> Self {
        Self {
            files_dir: "文件".to_string(),
            system_dir: ".系统".to_string(),
            manifest_dir: "清单".to_string(),
            max_title_chars: 80,
        }
    }
}

impl NasLayout {
    /// 图书最终文件的 NAS 相对路径，例如 `文件/000001-算法导论.pdf`。
    ///
    /// `book_seq` 为图书主数据的全局序号，`format` 为技术标识（`pdf`/`epub`，保持小写）。
    pub fn final_relative_path(&self, book_seq: i64, title: &str, format: &str) -> String {
        format!(
            "{}/{}",
            self.files_dir,
            self.final_file_name(book_seq, title, format)
        )
    }

    /// 最终文件名，例如 `000001-算法导论.pdf`。
    pub fn final_file_name(&self, book_seq: i64, title: &str, format: &str) -> String {
        let safe_title = truncate_chars(&sanitize_filename(title), self.max_title_chars);
        let safe_title = if safe_title.is_empty() {
            "未命名".to_string()
        } else {
            safe_title
        };
        let ext = format.trim().trim_start_matches('.').to_ascii_lowercase();
        format!("{book_seq:06}-{safe_title}.{ext}")
    }

    /// 「上传中」临时文件名，例如
    /// `000001-算法导论.pdf.上传中-<任务编号>-<节点编号>`。
    ///
    /// 临时名带任务与节点编号，保证两个 Worker 同时重试同一本书也不会互相覆盖。
    pub fn uploading_file_name(
        &self,
        final_file_name: &str,
        task_id: &str,
        node_id: &str,
    ) -> String {
        format!("{final_file_name}.上传中-{task_id}-{node_id}")
    }

    /// 清单目录相对路径，例如 `.系统/清单`。
    pub fn manifest_relative_dir(&self) -> String {
        format!("{}/{}", self.system_dir, self.manifest_dir)
    }

    /// 将 NAS 相对路径拼接到本机挂载根目录。
    ///
    /// 相对路径统一使用 `/` 分隔，拼接时按本机分隔符展开，
    /// 因此同一份 Master 数据可以同时服务 Windows（UNC）、macOS 与 Linux 挂载点。
    pub fn absolute_path(&self, mount_root: &Path, relative: &str) -> PathBuf {
        let mut path = mount_root.to_path_buf();
        for segment in relative.split('/').filter(|s| !s.is_empty()) {
            path.push(segment);
        }
        path
    }
}

/// 清理文件名中的非法字符，保证在 SMB/NFS、Windows、macOS 与 Linux 上都可写。
pub fn sanitize_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                ' '
            } else {
                ch
            }
        })
        .collect();

    // 折叠连续空白，并去掉 Windows 不允许的结尾点与空格
    let collapsed = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', ' '])
        .to_string();
    collapsed
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim_end_matches(['.', ' '])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_documented_layout() {
        let layout = NasLayout::default();
        assert_eq!(
            layout.final_relative_path(1, "算法导论", "pdf"),
            "文件/000001-算法导论.pdf"
        );
        assert_eq!(
            layout.final_relative_path(2, "计算机网络", "PDF"),
            "文件/000002-计算机网络.pdf"
        );
        assert_eq!(layout.manifest_relative_dir(), ".系统/清单");
    }

    #[test]
    fn uploading_name_includes_task_and_node() {
        let layout = NasLayout::default();
        let final_name = layout.final_file_name(1, "算法导论", "pdf");
        assert_eq!(
            layout.uploading_file_name(&final_name, "任务A", "节点1"),
            "000001-算法导论.pdf.上传中-任务A-节点1"
        );
    }

    #[test]
    fn strips_path_and_reserved_characters() {
        assert_eq!(sanitize_filename("../etc/passwd"), ".. etc passwd");
        assert_eq!(sanitize_filename("书名:副标题?"), "书名 副标题");
        assert_eq!(sanitize_filename("结尾点. "), "结尾点");
        // 清理后不得再包含任何路径分隔符
        let name = NasLayout::default().final_file_name(3, "a/b\\c", "epub");
        assert!(!name.contains('/') && !name.contains('\\'));
    }

    #[test]
    fn truncates_overlong_titles() {
        let layout = NasLayout {
            max_title_chars: 5,
            ..NasLayout::default()
        };
        assert_eq!(
            layout.final_file_name(9, "一二三四五六七八", "pdf"),
            "000009-一二三四五.pdf"
        );
    }

    #[test]
    fn empty_title_gets_placeholder() {
        assert_eq!(
            NasLayout::default().final_file_name(4, "  ", "pdf"),
            "000004-未命名.pdf"
        );
    }

    #[test]
    fn absolute_path_uses_local_separator() {
        let layout = NasLayout::default();
        let path = layout.absolute_path(Path::new("/Volumes/books"), "文件/000001-算法导论.pdf");
        assert_eq!(
            path,
            PathBuf::from("/Volumes/books/文件/000001-算法导论.pdf")
        );
    }
}
