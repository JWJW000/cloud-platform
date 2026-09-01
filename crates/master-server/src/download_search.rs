//! 下载站点搜索参数的校验与持久化读取。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::store;

/// `settings` 表中的固定键。
pub const SETTING_KEY: &str = "download_search_options";

/// 下发给 Worker 的搜索查询参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadSearchOptions {
    /// 站点 `order` 查询参数。
    pub order: String,
    /// 站点 `extensions[index]` 查询参数；空数组表示按任务目标格式自动生成。
    pub extensions: Vec<String>,
}

impl Default for DownloadSearchOptions {
    fn default() -> Self {
        Self {
            order: "bestmatch".to_string(),
            extensions: Vec::new(),
        }
    }
}

impl DownloadSearchOptions {
    /// 规范化并校验管理员输入，避免把任意查询片段注入站点 URL。
    pub fn normalized(self) -> AppResult<Self> {
        let order = self.order.trim().to_ascii_lowercase();
        if order.is_empty() || order.len() > 32 || !safe_token(&order) {
            return Err(AppError::bad(
                "order 必须是 1–32 位字母、数字、下划线或连字符",
            ));
        }

        if self.extensions.len() > 10 {
            return Err(AppError::bad("extensions 最多配置 10 项"));
        }
        let mut seen = HashSet::new();
        let mut extensions = Vec::new();
        for raw in self.extensions {
            let extension = raw.trim().trim_start_matches('.').to_ascii_lowercase();
            if extension.is_empty() || extension.len() > 16 || !safe_token(&extension) {
                return Err(AppError::bad(
                    "extension 必须是 1–16 位字母、数字、下划线或连字符",
                ));
            }
            if seen.insert(extension.clone()) {
                extensions.push(extension);
            }
        }

        Ok(Self { order, extensions })
    }
}

fn safe_token(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// 从数据库读取当前设置；迁移尚未执行时回退到兼容默认值。
pub async fn load(pool: &PgPool) -> AppResult<DownloadSearchOptions> {
    let Some(value) = store::admin::get_setting(pool, SETTING_KEY).await? else {
        return Ok(DownloadSearchOptions::default());
    };
    let options: DownloadSearchOptions = serde_json::from_value(value)
        .map_err(|error| AppError::bad(format!("下载搜索参数格式无效：{error}")))?;
    options.normalized()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_extensions_and_removes_duplicates() {
        let options = DownloadSearchOptions {
            order: " BestMatch ".to_string(),
            extensions: vec![".PDF".to_string(), "pdf".to_string(), " EPUB ".to_string()],
        }
        .normalized()
        .unwrap();

        assert_eq!(options.order, "bestmatch");
        assert_eq!(options.extensions, vec!["pdf", "epub"]);
    }

    #[test]
    fn rejects_query_injection_tokens() {
        assert!(DownloadSearchOptions {
            order: "bestmatch&admin=true".to_string(),
            extensions: Vec::new(),
        }
        .normalized()
        .is_err());
        assert!(DownloadSearchOptions {
            order: "bestmatch".to_string(),
            extensions: vec!["pdf&order=bad".to_string()],
        }
        .normalized()
        .is_err());
    }
}
