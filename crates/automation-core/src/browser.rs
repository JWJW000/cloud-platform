//! Chromium 内核浏览器探测（三平台）。
//!
//! 与现有桌面版一致的探测顺序，并补充 Linux 分支——异地 Worker 包含 Linux 机器。

use std::path::PathBuf;

use anyhow::{bail, Result};

/// 探测可用的 Chromium 内核浏览器。
///
/// `preference` 为 `auto` 或空字符串时按操作系统预设路径探测；
/// 否则必须是存在的可执行文件路径，不存在直接报错（避免静默回落到别的浏览器）。
pub fn detect_browser(preference: &str) -> Result<PathBuf> {
    if !preference.is_empty() && preference != "auto" {
        let path = PathBuf::from(preference);
        if path.exists() {
            return Ok(path);
        }
        bail!("配置的浏览器路径不存在：{}", path.display());
    }

    for candidate in candidates() {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Ok(path);
        }
    }

    bail!("未找到受支持的浏览器，请在 Worker 配置中显式指定可执行文件路径")
}

fn candidates() -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string(),
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".to_string(),
            "/Applications/Chromium.app/Contents/MacOS/Chromium".to_string(),
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser".to_string(),
        ]
    } else if cfg!(target_os = "windows") {
        let program_files =
            std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        let program_files_x86 =
            std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        let local_app_data = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| r"C:\Users\default\AppData\Local".into());
        vec![
            format!(r"{program_files}\Google\Chrome\Application\chrome.exe"),
            format!(r"{program_files}\Microsoft\Edge\Application\msedge.exe"),
            format!(r"{program_files_x86}\Google\Chrome\Application\chrome.exe"),
            format!(r"{program_files_x86}\Microsoft\Edge\Application\msedge.exe"),
            format!(r"{local_app_data}\Google\Chrome\Application\chrome.exe"),
            format!(r"{local_app_data}\Microsoft\Edge\Application\msedge.exe"),
        ]
    } else {
        // Linux：优先发行版包名，其次 snap/flatpak 常见路径
        vec![
            "/usr/bin/google-chrome".to_string(),
            "/usr/bin/google-chrome-stable".to_string(),
            "/usr/bin/chromium".to_string(),
            "/usr/bin/chromium-browser".to_string(),
            "/usr/bin/microsoft-edge".to_string(),
            "/snap/bin/chromium".to_string(),
        ]
    }
}

/// 生成会话专属的浏览器启动参数。
///
/// `proxy_endpoint` 是本机固定转发端口（如 `127.0.0.1:19001`）；
/// 会话期间该端口固定指向同一个上游代理，因此不需要任何按连接轮换的逻辑。
pub fn launch_args(
    profile_dir: &std::path::Path,
    download_dir: &std::path::Path,
    proxy_endpoint: Option<&str>,
    headless: bool,
) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-background-networking".to_string(),
        format!("--download-directory={}", download_dir.display()),
    ];
    if let Some(endpoint) = proxy_endpoint {
        args.push(format!("--proxy-server=http://{endpoint}"));
        // 本机转发端口必须走代理，因此不能把 127.0.0.1 加入 bypass 列表
        args.push("--proxy-bypass-list=<-loopback>".to_string());
    }
    if headless {
        args.push("--headless=new".to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn explicit_missing_path_is_error() {
        assert!(detect_browser("/definitely/missing/browser").is_err());
    }

    #[test]
    fn launch_args_pin_profile_and_proxy() {
        let args = launch_args(
            Path::new("/tmp/profiles/session-1"),
            Path::new("/tmp/staging/task-1"),
            Some("127.0.0.1:19001"),
            true,
        );
        assert!(args
            .iter()
            .any(|a| a == "--user-data-dir=/tmp/profiles/session-1"));
        assert!(args
            .iter()
            .any(|a| a == "--proxy-server=http://127.0.0.1:19001"));
        assert!(args.iter().any(|a| a == "--proxy-bypass-list=<-loopback>"));
        assert!(args.iter().any(|a| a == "--headless=new"));
    }

    #[test]
    fn launch_args_without_proxy_have_no_proxy_flag() {
        let args = launch_args(Path::new("/tmp/p"), Path::new("/tmp/d"), None, false);
        assert!(!args.iter().any(|a| a.starts_with("--proxy-server")));
        assert!(!args.iter().any(|a| a == "--headless=new"));
    }
}
