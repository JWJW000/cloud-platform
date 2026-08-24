//! 本地 NAS 文件写入与原子入库（第 9 节、第 14.4 节、第 11 节、V3 方案第 8 节）。
//!
//! 流程约束：
//! 1. 临时下载在 Worker 本机暂存目录：`staging/task-{task_id}`
//! 2. 严格防止路径逃逸（验证叶子文件名、父目录 containment）
//! 3. 写入 NAS 目录时先创建独占唯一临时文件：`.<最终文件名>.上传中-<task_id>-<execution_id>-<node_id>-<随机数>`
//! 4. 流式拷贝并对 NAS 临时文件执行 `sync_all`，核对 SHA-256 与字节数
//! 5. 使用跨平台不覆盖的原子提交（`commit_noreplace`），杜绝并发 Worker 相互覆盖
//! 6. 若目标文件已存在：哈希一致判定为幂等成功；哈希不同绝不覆盖，保留现有文件并返回冲突告警。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::StorageConfig;

/// 文件入库证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestResult {
    /// NAS 相对路径（如 000001-000500/000123-书名.pdf）。
    pub nas_relative_path: String,
    /// 最终文件名。
    pub file_name: String,
    /// 字节数。
    pub size_bytes: u64,
    /// SHA-256 十六进制字符串。
    pub sha256: String,
}

/// 入库最终裁决结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// 原子创建并入库成功。
    Success(IngestResult),
    /// 目标文件已存在且哈希完全一致（幂等成功）。
    AlreadyExistsSameHash(IngestResult),
    /// 目标文件已存在但哈希不同（冲突，禁止覆盖）。
    ConflictDifferentHash {
        existing_sha256: String,
        local_sha256: String,
        final_path: String,
    },
}

/// 跨平台不覆盖提交结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// 原子创建成功。
    Created,
    /// 目标文件已存在，未做替换。
    AlreadyExists,
    /// 当前操作系统与文件系统组合不提供可靠的 no-replace 语义。
    ///
    /// 出现该结果时调用方必须把节点置为「存储异常」，禁止用任何不安全回退继续下载
    /// （V4 方案第 9.2 节：能力不足时失败关闭）。
    Unsupported,
}

/// NAS 能力探测结论（第 9.3 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NasCapability {
    /// 是否支持可靠的 no-replace 提交。
    pub no_replace_supported: bool,
    /// 实际采用的提交策略（如 renameat2 / renamex_np / MoveFileEx / hard_link）。
    pub strategy: &'static str,
    /// 探测过程中的关键信息（操作系统、错误码结论），不含敏感路径。
    pub detail: String,
}

impl NasCapability {
    /// 是否允许承接下载任务。
    pub fn usable(&self) -> bool {
        self.no_replace_supported
    }
}

/// 校验叶子文件名，防止目录遍历与特殊控制字符。
pub fn validate_leaf_file_name(name: &str) -> Result<()> {
    let raw = name.trim();
    if raw.is_empty() {
        bail!("叶子文件名不能为空");
    }
    if raw == "." || raw == ".." {
        bail!("非法叶子文件名: {raw}");
    }
    if raw.contains('/') || raw.contains('\\') || raw.contains(':') {
        bail!("叶子文件名不得包含路径分隔符或盘符: {raw}");
    }
    if raw.chars().any(|c| c.is_control() || c == '\0') {
        bail!("叶子文件名不得包含控制字符或 NUL: {raw}");
    }
    if raw.len() > 255 {
        bail!("叶子文件名过长（> 255 字节）: {raw}");
    }
    // Windows 保留设备名（CON/PRN/AUX/NUL/COM1..9/LPT1..9 等），
    // 即使运行在 Unix 上也要拒绝：文件可能被同步到 Windows 挂载的 NAS 上。
    let stem = raw
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(raw)
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        bail!("叶子文件名为 Windows 保留设备名: {raw}");
    }
    let p = Path::new(raw);
    if p.components().count() != 1 {
        bail!("叶子文件名包含多级路径组件: {raw}");
    }
    Ok(())
}

/// 检查路径是否具有跨平台逃逸风险。
pub fn validate_relative_path(path: &str) -> Result<()> {
    let raw = path.trim();
    if raw.is_empty() {
        bail!("相对路径为空");
    }
    // 检查 Windows 盘符（如 C: 或 c:）与常见绝对路径前缀
    if raw.starts_with('/') || raw.starts_with('\\') {
        bail!("拒绝根目录前缀路径：{raw}");
    }
    if raw.len() >= 2
        && raw.chars().next().unwrap().is_ascii_alphabetic()
        && raw.chars().nth(1).unwrap() == ':'
    {
        bail!("拒绝 Windows 盘符绝对路径：{raw}");
    }

    let p = Path::new(raw);
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                bail!("路径包含父目录跳转 (..)，已被拦截：{raw}")
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                bail!("路径包含根目录或盘符前缀：{raw}")
            }
            _ => {}
        }
    }
    Ok(())
}

/// 校验目标目录是否严格位于 NAS 挂载根目录内（Containment 校验）。
///
/// V4 方案第 9.5 节：canonicalize **已存在的最近父目录** 后确认其位于
/// canonical NAS 根之下；目标目录自身尚未创建（NAS 路径通常逐级新建）时，
/// 向上找到最近存在的祖先再 canonicalize，避免「目录还不存在 → canonicalize
/// 失败 → 回退到未解析路径」的绕过。
pub fn ensure_containment(mount_root: &Path, target_dir: &Path) -> Result<()> {
    let canonical_root = canonicalize_existing(mount_root)
        .with_context(|| format!("NAS 挂载根目录不可解析: {}", mount_root.display()))?;
    let canonical_target = canonicalize_existing(target_dir)
        .with_context(|| format!("目标目录不可解析: {}", target_dir.display()))?;

    if !canonical_target.starts_with(&canonical_root) {
        bail!(
            "目标目录逃逸出 NAS 挂载根目录：{} 不在 {} 内部",
            canonical_target.display(),
            canonical_root.display()
        );
    }
    Ok(())
}

/// 对路径的**最近存在的祖先**执行 canonicalize（路径本身不存在时逐级向上）。
fn canonicalize_existing(path: &Path) -> Result<PathBuf> {
    let mut current = path;
    loop {
        match current.symlink_metadata() {
            Ok(_) => return current.canonicalize().map_err(Into::into),
            Err(_) => match current.parent() {
                Some(parent) => current = parent,
                None => anyhow::bail!("路径不存在且没有可解析的祖先：{}", path.display()),
            },
        }
    }
}

/// 检查最终路径是否落在符号链接 / 重解析点上（V4 方案第 9.5 节）。
///
/// 目录创建发生在 containment 校验之后，但最终文件若已存在且是符号链接，
/// 复制/提交会把内容写到链接指向的任意位置——必须拒绝。
fn ensure_final_path_not_symlink(final_path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(final_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("NAS 最终路径是符号链接，拒绝写入：{}", final_path.display())
        }
        Ok(_) => Ok(()),
        Err(_) => Ok(()), // 不存在 = 安全
    }
}

/// 跨平台不覆盖原子提交。
///
/// 三种结果之外**不存在**「不确定但当作成功」或「锁失败后继续 rename」的分支
/// （V4-05）。当前平台提供原生 no-replace 原语时使用之；否则尝试 hard_link
/// 独占创建；两者都不可用时返回 [`CommitOutcome::Unsupported`]，由调用方
/// 把节点置为「存储异常」并停止申请下载会话。
pub async fn commit_noreplace(temp: &Path, final_path: &Path) -> Result<CommitOutcome> {
    let temp_buf = temp.to_path_buf();
    let final_buf = final_path.to_path_buf();

    tokio::task::spawn_blocking(move || commit_noreplace_sync(&temp_buf, &final_buf))
        .await
        .context("执行 commit_noreplace 任务失败")?
}

fn commit_noreplace_sync(temp: &Path, final_path: &Path) -> Result<CommitOutcome> {
    // 1. 原生 no-replace 原语（平台各自独立编译，互不影响）
    #[cfg(target_os = "macos")]
    {
        if let Some(outcome) = native_commit_macos(temp, final_path)? {
            return Ok(outcome);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(outcome) = native_commit_linux(temp, final_path)? {
            return Ok(outcome);
        }
    }
    #[cfg(windows)]
    {
        if let Some(outcome) = native_commit_windows(temp, final_path)? {
            return Ok(outcome);
        }
    }

    // 2. 原生原语不可用（文件系统不支持）时，尝试 hard_link 独占创建：
    //    目标已存在时 hard_link 必然失败（EEXIST），天然具备 no-replace 语义。
    match std::fs::hard_link(temp, final_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(temp);
            return Ok(CommitOutcome::Created);
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(CommitOutcome::AlreadyExists);
        }
        Err(_) => {}
    }

    // 3. 无可靠方案：失败关闭，禁止 exists()+rename() / 忽略锁创建结果的回退
    tracing::warn!(
        temp = %temp.display(),
        final_path = %final_path.display(),
        "当前操作系统与文件系统的组合不支持可靠的 no-replace 提交，判定为存储异常"
    );
    Ok(CommitOutcome::Unsupported)
}

/// 判断一个 errno 是否表示「该文件系统不支持该操作」。
fn is_unsupported_errno(errno: i32) -> bool {
    #[allow(unreachable_patterns)]
    matches!(
        errno,
        libc::EINVAL | libc::ENOSYS | libc::ENOTSUP | libc::EOPNOTSUPP | libc::ENOTTY
    )
}

/// Linux：`renameat2(..., RENAME_NOREPLACE)`。
#[cfg(target_os = "linux")]
fn native_commit_linux(temp: &Path, final_path: &Path) -> Result<Option<CommitOutcome>> {
    if let (Some(temp_str), Some(final_str)) = (temp.to_str(), final_path.to_str()) {
        if let (Ok(c_src), Ok(c_dst)) = (
            std::ffi::CString::new(temp_str),
            std::ffi::CString::new(final_str),
        ) {
            let ret = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    c_src.as_ptr(),
                    libc::AT_FDCWD,
                    c_dst.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if ret == 0 {
                return Ok(Some(CommitOutcome::Created));
            }
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == libc::EEXIST {
                return Ok(Some(CommitOutcome::AlreadyExists));
            }
            if is_unsupported_errno(errno) {
                // 文件系统不支持 renameat2（例如部分网络文件系统），交给 hard_link 分支
                return Ok(None);
            }
            return Err(std::io::Error::from_raw_os_error(errno))
                .with_context(|| format!("renameat2 提交失败: {}", final_path.display()));
        }
    }
    Ok(None)
}

/// macOS：`renamex_np(..., RENAME_EXCL)`。
#[cfg(target_os = "macos")]
fn native_commit_macos(temp: &Path, final_path: &Path) -> Result<Option<CommitOutcome>> {
    if let (Some(temp_str), Some(final_str)) = (temp.to_str(), final_path.to_str()) {
        if let (Ok(c_src), Ok(c_dst)) = (
            std::ffi::CString::new(temp_str),
            std::ffi::CString::new(final_str),
        ) {
            let ret =
                unsafe { libc::renamex_np(c_src.as_ptr(), c_dst.as_ptr(), libc::RENAME_EXCL) };
            if ret == 0 {
                return Ok(Some(CommitOutcome::Created));
            }
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == libc::EEXIST {
                return Ok(Some(CommitOutcome::AlreadyExists));
            }
            if is_unsupported_errno(errno) {
                return Ok(None);
            }
            return Err(std::io::Error::from_raw_os_error(errno))
                .with_context(|| format!("renamex_np 提交失败: {}", final_path.display()));
        }
    }
    Ok(None)
}

/// Windows：`MoveFileExW` 且**不带** `MOVEFILE_REPLACE_EXISTING`——
/// 目标存在时返回 `ERROR_FILE_EXISTS` / `ERROR_ALREADY_EXISTS`，天然 no-replace。
#[cfg(windows)]
fn native_commit_windows(temp: &Path, final_path: &Path) -> Result<Option<CommitOutcome>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let src: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let dst: Vec<u16> = final_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let ret = unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    if ret != 0 {
        return Ok(Some(CommitOutcome::Created));
    }
    let err = std::io::Error::last_os_error();
    let code = err.raw_os_error().unwrap_or(0);
    // ERROR_FILE_EXISTS = 80；ERROR_ALREADY_EXISTS = 183
    if code == 80 || code == 183 {
        return Ok(Some(CommitOutcome::AlreadyExists));
    }
    Err(err).with_context(|| format!("MoveFileEx 提交失败: {}", final_path.display()))
}

/// NAS no-replace 能力探测（第 9.3 节）。
///
/// 在 NAS 隐藏探测目录中：
/// 1. 创建源文件 A；
/// 2. 创建已存在目标 B；
/// 3. 调用正式 `commit_noreplace`；
/// 4. 必须返回 AlreadyExists，且 B 内容未变化；
/// 5. 清理探测文件；
/// 6. 记录结论，但不记录敏感路径。
///
/// 应在启动时与 NAS 重新挂载后执行，不需要每个心跳执行。
pub async fn probe_nas_capability(storage: &StorageConfig, node_id: &str) -> NasCapability {
    let probe_dir = storage
        .nas_mount
        .join(format!(".能力探测-{node_id}-{:08x}", rand::random::<u32>()));
    if let Err(err) = tokio::fs::create_dir_all(&probe_dir).await {
        return NasCapability {
            no_replace_supported: false,
            strategy: "none",
            detail: format!("无法创建能力探测目录：{err}"),
        };
    }

    let source = probe_dir.join("source-A");
    let target = probe_dir.join("target-B");

    let result = async {
        // 1. 源文件 A
        tokio::fs::write(&source, b"A").await?;
        // 2. 已存在目标 B
        tokio::fs::write(&target, b"B").await?;

        // 3. 正式提交路径（复用真实的 commit_noreplace）
        let outcome = commit_noreplace(&source, &target).await?;
        if outcome != CommitOutcome::AlreadyExists {
            anyhow::bail!("探测期望 AlreadyExists，实际得到 {outcome:?}");
        }

        // 4. 验证 B 内容未变化
        let content = tokio::fs::read(&target).await?;
        if content != b"B" {
            anyhow::bail!(
                "探测发现目标文件内容被改动：{}",
                String::from_utf8_lossy(&content)
            );
        }

        // 额外验证一次「目标不存在」路径返回 Created
        let fresh = probe_dir.join("target-fresh");
        let outcome = commit_noreplace(&source, &fresh).await?;
        if outcome != CommitOutcome::Created {
            anyhow::bail!("探测期望 Created，实际得到 {outcome:?}");
        }

        Ok(())
    }
    .await;

    let _ = tokio::fs::remove_file(&source).await;
    let _ = tokio::fs::remove_file(&target).await;
    let _ = tokio::fs::remove_file(probe_dir.join("target-fresh")).await;
    let _ = tokio::fs::remove_dir_all(&probe_dir).await;

    match result {
        Ok(()) => NasCapability {
            no_replace_supported: true,
            strategy: platform_strategy(),
            detail: format!(
                "no-replace 能力探测通过（{}，已有目标不覆盖、新建目标可创建）",
                platform_strategy()
            ),
        },
        Err(err) => NasCapability {
            no_replace_supported: false,
            strategy: "none",
            detail: format!("no-replace 能力探测失败：{err}"),
        },
    }
}

/// NAS 能力探测管理器（第 9.3 节）。
///
/// 启动时探测一次；此后若挂载点从「不可用」恢复为「可用」（NAS 重新挂载），
/// 自动重新探测。每个心跳读取当前结论，不重复执行探测。
#[derive(Clone)]
pub struct NasProbeManager {
    storage: StorageConfig,
    node_id: String,
    inner: std::sync::Arc<std::sync::Mutex<NasProbeInner>>,
}

struct NasProbeInner {
    capability: NasCapability,
    /// 上一次心跳时挂载点是否不可用；用于检测「重新挂载」事件。
    mount_was_unhealthy: bool,
}

impl NasProbeManager {
    /// 启动并执行首次能力探测。
    pub async fn start(storage: StorageConfig, node_id: String) -> Self {
        let capability = probe_nas_capability(&storage, &node_id).await;
        tracing::info!(
            supported = capability.no_replace_supported,
            strategy = capability.strategy,
            detail = %capability.detail,
            "NAS no-replace 能力探测完成"
        );
        Self {
            storage,
            node_id,
            inner: std::sync::Arc::new(std::sync::Mutex::new(NasProbeInner {
                capability,
                mount_was_unhealthy: false,
            })),
        }
    }

    /// 当前能力结论。
    pub fn capability(&self) -> NasCapability {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.capability.clone()
    }

    /// 每次心跳后调用：检测「NAS 重新挂载」并重新探测能力。
    pub async fn maybe_reprobe(&self, health: &NasHealth) {
        let should_reprobe = {
            let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if health.mount_present && guard.mount_was_unhealthy {
                guard.mount_was_unhealthy = false;
                true
            } else {
                if !health.mount_present {
                    guard.mount_was_unhealthy = true;
                }
                false
            }
        };
        if should_reprobe {
            let capability = probe_nas_capability(&self.storage, &self.node_id).await;
            let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.capability = capability.clone();
            tracing::info!(
                supported = capability.no_replace_supported,
                strategy = capability.strategy,
                "NAS 重新挂载，已重新探测 no-replace 能力"
            );
        }
    }
}

/// 当前平台采用的 no-replace 策略名（技术标识）。
fn platform_strategy() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "renameat2(RENAME_NOREPLACE)"
    }
    #[cfg(target_os = "macos")]
    {
        "renamex_np(RENAME_EXCL)"
    }
    #[cfg(windows)]
    {
        "MoveFileExW(no-replace)"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        "hard_link(exclusive)"
    }
}

/// 将已下载的临时文件原子写入 NAS 目录并严格复核。
pub async fn ingest_file(
    storage: &StorageConfig,
    local_temp_file: &Path,
    nas_relative_path: &str,
    task_id: Uuid,
    execution_id: Uuid,
    node_id: Uuid,
    minimum_bytes: u64,
) -> Result<IngestOutcome> {
    if !local_temp_file.exists() {
        bail!("本地临时下载文件不存在：{}", local_temp_file.display());
    }

    // 1. 相对路径防逃逸检查
    validate_relative_path(nas_relative_path)?;

    let relative_p = Path::new(nas_relative_path);
    let final_file_name = relative_p
        .file_name()
        .and_then(|n| n.to_str())
        .context("无法从 NAS 相对路径提取最终文件名")?;

    validate_leaf_file_name(final_file_name)?;

    let meta = tokio::fs::metadata(local_temp_file).await?;
    let local_size_bytes = meta.len();
    if local_size_bytes < minimum_bytes {
        bail!(
            "下载文件过小（{} 字节 < 阈值 {} 字节），疑似错误页，拒绝入库",
            local_size_bytes,
            minimum_bytes
        );
    }

    // 2. 计算本机源文件 SHA-256
    let local_sha256 = calculate_sha256(local_temp_file).await?;

    // 3. 确定 NAS 目标目录并检查 Containment。
    //
    // P0（审查第 4 项）：Containment 校验**必须发生在 create_dir_all 之前**——
    // 若中间目录是符号链接，先建目录可能把目录建到 NAS 根之外，随后才报错。
    // ensure_containment 会对最近存在的父目录做 canonical 解析，
    // 因此目标目录尚未创建时也能可靠判断。
    let nas_full_target_file = storage.nas_mount.join(nas_relative_path);
    let target_dir = nas_full_target_file
        .parent()
        .context("无法确定 NAS 目标父目录")?;

    ensure_containment(&storage.nas_mount, target_dir)?;
    ensure_final_path_not_symlink(&nas_full_target_file)?;

    tokio::fs::create_dir_all(target_dir)
        .await
        .with_context(|| format!("创建 NAS 目标目录失败：{}", target_dir.display()))?;

    // 创建后再做一次 containment 复核（纵深防御：目录刚被创建，canonical 全解析
    // 可发现「创建路径本身变成符号链接」这类竞态）。
    ensure_containment(&storage.nas_mount, target_dir)?;

    // 4. 检查已存在目标文件（快速幂等路径）
    if nas_full_target_file.exists() {
        if let Ok(existing_sha) = calculate_sha256(&nas_full_target_file).await {
            if existing_sha == local_sha256 {
                tracing::info!(
                    path = %nas_full_target_file.display(),
                    "NAS 目标文件已存在且哈希一致，视为幂等成功"
                );
                let _ = tokio::fs::remove_file(local_temp_file).await;
                return Ok(IngestOutcome::AlreadyExistsSameHash(IngestResult {
                    nas_relative_path: nas_relative_path.to_string(),
                    file_name: final_file_name.to_string(),
                    size_bytes: local_size_bytes,
                    sha256: local_sha256,
                }));
            } else {
                tracing::warn!(
                    existing_sha = %existing_sha,
                    local_sha = %local_sha256,
                    path = %nas_full_target_file.display(),
                    "NAS 上已存在同名文件但哈希不一致，禁止覆盖"
                );
                return Ok(IngestOutcome::ConflictDifferentHash {
                    existing_sha256: existing_sha,
                    local_sha256,
                    final_path: nas_full_target_file.display().to_string(),
                });
            }
        }
    }

    // 5. 独占创建唯一 NAS 临时上传文件
    let rand_id: u32 = rand::random();
    let uploading_file_name =
        format!(".{final_file_name}.上传中-{task_id}-{execution_id}-{node_id}-{rand_id:08x}");
    let nas_uploading_path = target_dir.join(&uploading_file_name);

    {
        let mut reader = tokio::fs::File::open(local_temp_file).await?;
        let mut writer = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&nas_uploading_path)
            .await
            .with_context(|| format!("创建 NAS 临时文件失败: {}", nas_uploading_path.display()))?;

        tokio::io::copy(&mut reader, &mut writer)
            .await
            .with_context(|| {
                format!("拷贝文件到 NAS 暂存失败：{}", nas_uploading_path.display())
            })?;
        writer.sync_all().await?;
    }

    // 6. 复核 NAS 临时文件的哈希与大小
    let nas_staged_sha = calculate_sha256(&nas_uploading_path).await?;
    if nas_staged_sha != local_sha256 {
        let _ = tokio::fs::remove_file(&nas_uploading_path).await;
        bail!("NAS 临时文件哈希与本地源文件不匹配，数据传输损坏");
    }

    // 7. 使用不覆盖语义提交至最终路径
    let commit_outcome = commit_noreplace(&nas_uploading_path, &nas_full_target_file).await?;

    match commit_outcome {
        CommitOutcome::Unsupported => {
            // V4 方案第 9.2 节：能力不足时失败关闭。清理**本次上传**的 NAS 临时文件
            // （归属明确：task+execution+node 唯一名），保留本地源文件与已存在的
            // 最终文件供人工核验，绝不退化为 exists()+rename()。
            let _ = tokio::fs::remove_file(&nas_uploading_path).await;
            bail!(
                "当前文件系统不支持可靠的 no-replace 提交（NAS 路径 {}），节点应进入存储异常并停止申请下载会话",
                nas_full_target_file.display()
            );
        }
        CommitOutcome::Created => {
            // 复核最终文件哈希
            let final_sha = calculate_sha256(&nas_full_target_file).await?;
            if final_sha != local_sha256 {
                bail!("最终 NAS 文件哈希校验异常");
            }
            // 清理本地临时文件
            let _ = tokio::fs::remove_file(local_temp_file).await;

            Ok(IngestOutcome::Success(IngestResult {
                nas_relative_path: nas_relative_path.to_string(),
                file_name: final_file_name.to_string(),
                size_bytes: local_size_bytes,
                sha256: local_sha256,
            }))
        }
        CommitOutcome::AlreadyExists => {
            let _ = tokio::fs::remove_file(&nas_uploading_path).await;
            let existing_sha = calculate_sha256(&nas_full_target_file).await?;
            if existing_sha == local_sha256 {
                let _ = tokio::fs::remove_file(local_temp_file).await;
                Ok(IngestOutcome::AlreadyExistsSameHash(IngestResult {
                    nas_relative_path: nas_relative_path.to_string(),
                    file_name: final_file_name.to_string(),
                    size_bytes: local_size_bytes,
                    sha256: local_sha256,
                }))
            } else {
                Ok(IngestOutcome::ConflictDifferentHash {
                    existing_sha256: existing_sha,
                    local_sha256,
                    final_path: nas_full_target_file.display().to_string(),
                })
            }
        }
    }
}

/// 计算文件的 SHA-256 散列。
pub async fn calculate_sha256(path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// NAS 挂载点健康检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NasHealth {
    /// 挂载点是否存在且是目录。
    pub mount_present: bool,
    /// 探针文件写入是否成功。
    pub writable: bool,
    /// 剩余空间 GB；`None` 表示查不到。
    pub free_gb: Option<u64>,
    /// 探针往返耗时（写入 + 读回 + 删除）。
    pub latency_ms: u64,
    /// 中文说明。
    pub detail: String,
}

impl NasHealth {
    /// 是否可以承接下载任务。
    pub fn healthy(&self) -> bool {
        self.mount_present && self.writable
    }

    /// 上报给 Master 的剩余空间。
    pub fn reported_free_gb(&self) -> u64 {
        self.free_gb.unwrap_or(0)
    }
}

/// 实测 NAS 挂载点的可写性、剩余空间与往返延迟。
pub async fn check_nas_health(storage: &StorageConfig, node_id: &str) -> NasHealth {
    let mount_dir = &storage.nas_mount;
    if !(mount_dir.exists() && mount_dir.is_dir()) {
        return NasHealth {
            mount_present: false,
            writable: false,
            free_gb: None,
            latency_ms: 0,
            detail: format!("NAS 挂载点不存在或不是目录：{}", mount_dir.display()),
        };
    }

    let probe_name = format!(".nas_probe_{}_{}.tmp", node_id, rand::random::<u32>());
    let probe_path = mount_dir.join(probe_name);
    let payload = format!("nas-probe-{node_id}").into_bytes();

    let started = std::time::Instant::now();
    let (writable, mut detail) = match tokio::fs::write(&probe_path, &payload).await {
        Ok(()) => match tokio::fs::read(&probe_path).await {
            Ok(read_back) if read_back == payload => (true, "NAS 探针读写往返正常".to_string()),
            Ok(_) => (
                false,
                "NAS 探针读回内容与写入不一致，挂载点可能处于异常状态".to_string(),
            ),
            Err(err) => (false, format!("NAS 探针写入成功但读回失败：{err}")),
        },
        Err(err) => (false, format!("NAS 探针写入失败：{err}")),
    };
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let _ = tokio::fs::remove_file(&probe_path).await;

    let free_gb = free_space_gb(mount_dir);
    if free_gb.is_none() {
        detail.push_str("；系统未报告该挂载点的剩余空间，已按 0 上报");
    }

    NasHealth {
        mount_present: true,
        writable,
        free_gb,
        latency_ms,
        detail,
    }
}

/// 查询某个路径所在文件系统的剩余空间（GB）。
pub fn free_space_gb(path: &std::path::Path) -> Option<u64> {
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| target.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space() / (1024 * 1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_traversal_validation() {
        assert!(validate_relative_path("000001-000500/000123-book.pdf").is_ok());
        assert!(validate_relative_path("../secret.txt").is_err());
        assert!(validate_relative_path("sub/../../etc/passwd").is_err());
        assert!(validate_relative_path("/root/test.pdf").is_err());
        assert!(validate_relative_path("C:\\Windows\\test.pdf").is_err());
        assert!(validate_relative_path("D:/books/test.pdf").is_err());
    }

    #[test]
    fn leaf_file_name_validation() {
        assert!(validate_leaf_file_name("000123-算法导论.pdf").is_ok());
        assert!(validate_leaf_file_name("").is_err());
        assert!(validate_leaf_file_name(".").is_err());
        assert!(validate_leaf_file_name("..").is_err());
        assert!(validate_leaf_file_name("sub/book.pdf").is_err());
        assert!(validate_leaf_file_name("C:book.pdf").is_err());
        // Windows 保留设备名，即使运行在 Unix 上也要拒绝（NAS 可能被 Windows 挂载）
        assert!(validate_leaf_file_name("CON.pdf").is_err());
        assert!(validate_leaf_file_name("PRN").is_err());
        assert!(validate_leaf_file_name("com1.txt").is_err());
        assert!(validate_leaf_file_name("LPT9-书.pdf").is_ok());
        assert!(validate_leaf_file_name("NUL.pdf").is_err());
    }

    #[tokio::test]
    async fn capability_probe_passes_on_local_fs_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        tokio::fs::create_dir_all(&storage.nas_mount).await.unwrap();

        let cap = probe_nas_capability(&storage, "节点1").await;
        assert!(
            cap.no_replace_supported,
            "本地文件系统应支持 no-replace：{}",
            cap.detail
        );

        // 探测目录与文件必须清理干净
        let mut entries = tokio::fs::read_dir(&storage.nas_mount).await.unwrap();
        assert!(
            entries.next_entry().await.unwrap().is_none(),
            "能力探测后 NAS 根目录不应残留任何文件"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn symlinked_final_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        let target_dir = storage.nas_mount.join("000001-000500");
        tokio::fs::create_dir_all(&target_dir).await.unwrap();
        tokio::fs::write(target_dir.join("real.pdf"), vec![1u8; 1024])
            .await
            .unwrap();

        let local = dir.path().join("book.pdf");
        tokio::fs::write(&local, vec![7u8; 40 * 1024])
            .await
            .unwrap();

        // 最终路径是符号链接 → 拒绝（不跟随链接写入）
        let symlink = target_dir.join("000123-书.pdf");
        std::os::unix::fs::symlink(target_dir.join("real.pdf"), &symlink).unwrap();

        let err = ingest_file(
            &storage,
            &local,
            "000001-000500/000123-书.pdf",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            32 * 1024,
        )
        .await
        .expect_err("最终路径是符号链接必须被拒绝");
        assert!(err.to_string().contains("符号链接"));
        // 链接目标未被改动
        let real = tokio::fs::read(target_dir.join("real.pdf")).await.unwrap();
        assert_eq!(real, vec![1u8; 1024]);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn containment_rejects_escape_via_symlinked_parent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        tokio::fs::create_dir_all(&storage.nas_mount).await.unwrap();

        // 在 NAS 根外建一个真实目录，再在 NAS 根里放一个指向它的符号链接
        let outside = dir.path().join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        let link = storage.nas_mount.join("escape-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let target = link.join("sub");
        tokio::fs::create_dir_all(&target).await.unwrap();
        assert!(ensure_containment(&storage.nas_mount, &target).is_err());
    }

    fn storage_at(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.join("data"),
            nas_mount: dir.join("nas"),
        }
    }

    #[tokio::test]
    async fn missing_mount_is_reported_as_unhealthy_without_a_fake_size() {
        let dir = tempfile::tempdir().unwrap();
        let health = check_nas_health(&storage_at(dir.path()), "节点1").await;
        assert!(!health.mount_present);
        assert!(!health.healthy());
        assert_eq!(health.free_gb, None);
        assert_eq!(health.reported_free_gb(), 0);
        assert!(health.detail.contains("挂载点不存在"));
    }

    #[tokio::test]
    async fn writable_mount_measures_a_real_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        tokio::fs::create_dir_all(&storage.nas_mount).await.unwrap();

        let health = check_nas_health(&storage, "节点1").await;
        assert!(health.mount_present);
        assert!(health.writable, "{}", health.detail);
        assert!(health.healthy());

        let mut entries = tokio::fs::read_dir(&storage.nas_mount).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn too_small_file_is_refused_before_touching_nas() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        tokio::fs::create_dir_all(&storage.nas_mount).await.unwrap();
        let local = dir.path().join("small.pdf");
        tokio::fs::write(&local, "这不是一本书，只是站点的错误页".as_bytes())
            .await
            .unwrap();

        let err = ingest_file(
            &storage,
            &local,
            "000001-000500/000123-书.pdf",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            32 * 1024,
        )
        .await
        .expect_err("过小的文件必须被拒绝");
        assert!(err.to_string().contains("疑似错误页"));
        assert!(!storage.nas_mount.join("000001-000500").exists());
    }

    #[tokio::test]
    async fn same_hash_target_is_idempotent_and_different_hash_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        let target_dir = storage.nas_mount.join("000001-000500");
        tokio::fs::create_dir_all(&target_dir).await.unwrap();

        let body = vec![7u8; 40 * 1024];
        tokio::fs::write(target_dir.join("000123-书.pdf"), &body)
            .await
            .unwrap();

        // 同哈希：视为已经入过库，直接成功，且不留下本地文件
        let local = dir.path().join("same.pdf");
        tokio::fs::write(&local, &body).await.unwrap();
        let result = ingest_file(
            &storage,
            &local,
            "000001-000500/000123-书.pdf",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            32 * 1024,
        )
        .await
        .expect("同哈希应幂等成功");

        match result {
            IngestOutcome::AlreadyExistsSameHash(res) => {
                assert_eq!(res.size_bytes, body.len() as u64);
                assert!(!local.exists());
            }
            other => panic!("预期 AlreadyExistsSameHash，得到 {other:?}"),
        }

        // 不同哈希：绝不覆盖
        let other = dir.path().join("other.pdf");
        tokio::fs::write(&other, vec![9u8; 40 * 1024])
            .await
            .unwrap();
        let result = ingest_file(
            &storage,
            &other,
            "000001-000500/000123-书.pdf",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            32 * 1024,
        )
        .await
        .expect("不同哈希应返回冲突判定");

        match result {
            IngestOutcome::ConflictDifferentHash { .. } => {}
            other => panic!("预期 ConflictDifferentHash，得到 {other:?}"),
        }
        assert!(other.exists(), "发生冲突时不应删除本地证据文件");
    }

    #[tokio::test]
    async fn successful_ingest_commits_atomically_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_at(dir.path());
        tokio::fs::create_dir_all(&storage.nas_mount).await.unwrap();
        let local = dir.path().join("book.pdf");
        tokio::fs::write(&local, vec![3u8; 50 * 1024])
            .await
            .unwrap();

        let result = ingest_file(
            &storage,
            &local,
            "000001-000500/000123-书.pdf",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            32 * 1024,
        )
        .await
        .expect("入库应成功");

        let final_path = storage.nas_mount.join("000001-000500/000123-书.pdf");
        assert!(final_path.exists());

        match result {
            IngestOutcome::Success(res) => {
                assert_eq!(res.sha256, calculate_sha256(&final_path).await.unwrap());
            }
            other => panic!("预期 Success，得到 {other:?}"),
        }

        assert!(!local.exists(), "入库成功后应清理本地暂存文件");
    }
}
