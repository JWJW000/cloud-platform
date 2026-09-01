//! 模拟引擎：不启动浏览器，但产生真实文件与真实事件流。
//!
//! 用途是验证 **平台自身**——调度租约、断线补报、NAS 原子入库、哈希校验、
//! 全局去重、限额归因——而不依赖目标站点可用性。第 20 节的故障验收项
//! 几乎都可以用它在本机跑通。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use platform_domain::FailureClass;

use crate::cancel::CancelToken;
use crate::engine::{AutomationEngine, EventSink};
use crate::types::{
    AutomationError, DownloadOutcome, DownloadSpec, RegistrationOutcome, RegistrationSpec,
    SessionHandle, SessionSpec,
};

/// 模拟剧本：让测试可以精确复现每一种失败路径。
#[derive(Debug, Clone)]
pub struct SimulationScript {
    /// 这些书名会返回「站点未收录」（任务应进入 `已跳过`）。
    pub not_found_titles: Vec<String>,
    /// 这些书名会返回可重试失败。
    pub retryable_titles: Vec<String>,
    /// 这些书名会返回「结果不确定」（任务应进入 `待确认`）。
    pub uncertain_titles: Vec<String>,
    /// 这些书名会触发代理故障。
    pub proxy_failure_titles: Vec<String>,
    /// 单个会话内允许成功的下载次数，超过后返回额度耗尽。
    pub quota_limit: u32,
    /// 生成文件的字节数。
    pub file_size_bytes: u64,
    /// 每个进度步骤之间的等待，用于观察进度节流。
    pub step_delay: Duration,
}

impl Default for SimulationScript {
    fn default() -> Self {
        Self {
            not_found_titles: Vec::new(),
            retryable_titles: Vec::new(),
            uncertain_titles: Vec::new(),
            proxy_failure_titles: Vec::new(),
            quota_limit: 10,
            file_size_bytes: 512 * 1024,
            step_delay: Duration::from_millis(0),
        }
    }
}

/// 模拟自动化引擎。
pub struct SimulatedEngine {
    script: SimulationScript,
    /// 会话编号 → 该会话已成功下载数量。
    used: Mutex<HashMap<String, u32>>,
    /// 会话编号 → 当前生效的「浏览器下载目录」。
    ///
    /// 模拟引擎也维护这个状态，是为了让第 8.2 节的顺序错误在本机测试里就暴露：
    /// 没有先设目录就下载、或者设的目录与任务扫描目录不一致，都会直接失败，
    /// 而不是像真实浏览器那样把文件默默丢进公共 staging 根目录。
    download_dirs: Mutex<HashMap<String, PathBuf>>,
}

impl SimulatedEngine {
    /// 使用给定剧本创建引擎。
    pub fn new(script: SimulationScript) -> Self {
        Self {
            script,
            used: Mutex::new(HashMap::new()),
            download_dirs: Mutex::new(HashMap::new()),
        }
    }

    /// 使用默认剧本（全部成功，单会话 10 本）。
    pub fn with_defaults() -> Self {
        Self::new(SimulationScript::default())
    }

    fn matches(list: &[String], title: &str) -> bool {
        list.iter().any(|item| item == title)
    }

    fn quota(&self, session_id: &str) -> (u32, u32) {
        let used = self
            .used
            .lock()
            .expect("模拟引擎计数锁被污染")
            .get(session_id)
            .copied()
            .unwrap_or(0);
        (used, self.script.quota_limit)
    }
}

/// 生成能通过 [`crate::verify::check_signature`] 的最小文件头。
fn file_signature(format: &str) -> Vec<u8> {
    match format {
        "epub" => {
            let mut bytes = Vec::with_capacity(64);
            bytes.extend_from_slice(b"PK\x03\x04");
            bytes.extend_from_slice(&[0u8; 26]);
            bytes.extend_from_slice(b"mimetype");
            bytes.extend_from_slice(b"application/epub+zip");
            bytes
        }
        // 默认按 PDF 生成：`download_format` 只允许 pdf/epub（第 7.1 节）
        _ => b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec(),
    }
}

#[async_trait]
impl AutomationEngine for SimulatedEngine {
    fn name(&self) -> &'static str {
        "模拟引擎"
    }

    async fn open_session(&self, spec: &SessionSpec) -> Result<SessionHandle, AutomationError> {
        tokio::fs::create_dir_all(&spec.profile_dir)
            .await
            .map_err(|err| {
                AutomationError::new(FailureClass::Fatal, format!("创建 Profile 目录失败：{err}"))
            })?;
        self.used
            .lock()
            .expect("模拟引擎计数锁被污染")
            .insert(spec.session_id.clone(), 0);
        Ok(SessionHandle {
            session_id: spec.session_id.clone(),
            browser_path: PathBuf::from("<模拟浏览器>"),
            profile_dir: spec.profile_dir.clone(),
        })
    }

    async fn set_task_download_dir(
        &self,
        session: &SessionHandle,
        dir: &Path,
    ) -> Result<(), AutomationError> {
        tokio::fs::create_dir_all(dir).await.map_err(|err| {
            AutomationError::new(
                FailureClass::Fatal,
                format!("创建任务下载目录 {} 失败：{err}", dir.display()),
            )
        })?;
        self.download_dirs
            .lock()
            .expect("模拟引擎目录锁被污染")
            .insert(session.session_id.clone(), dir.to_path_buf());
        Ok(())
    }

    async fn download_book(
        &self,
        session: &SessionHandle,
        spec: &DownloadSpec,
        events: &EventSink,
        cancel: &CancelToken,
    ) -> Result<DownloadOutcome, AutomationError> {
        cancel.check()?;
        let title = spec.book.title.as_str();

        // 第 8.2 节：下载目录必须在点击下载之前就切到本任务独占目录。
        // 目录没设或设错了，此刻就必须失败——否则文件会落到别的任务的目录里，
        // 而那时候已经没有任何办法判断这个文件属于谁。
        let browser_dir = self
            .download_dirs
            .lock()
            .expect("模拟引擎目录锁被污染")
            .get(&session.session_id)
            .cloned();
        match browser_dir {
            None => {
                return Err(AutomationError::new(
                    FailureClass::Fatal,
                    "尚未为本任务设置浏览器下载目录，拒绝开始下载（第 8.2 节）",
                ));
            }
            Some(dir) if dir != spec.staging_dir => {
                return Err(AutomationError::new(
                    FailureClass::Fatal,
                    format!(
                        "浏览器下载目录 {} 与任务暂存目录 {} 不一致，拒绝开始下载（第 8.2 节）",
                        dir.display(),
                        spec.staging_dir.display()
                    ),
                ));
            }
            Some(_) => {}
        }

        events.stage("搜索中");
        if !cancel.sleep(self.script.step_delay).await {
            return Err(cancel.check().unwrap_err());
        }

        if Self::matches(&self.script.proxy_failure_titles, title) {
            return Err(AutomationError::new(
                FailureClass::ProxyFailure,
                "proxy connect failed: 模拟上游代理不可用",
            ));
        }
        if Self::matches(&self.script.not_found_titles, title) {
            return Err(AutomationError::new(
                FailureClass::BookNotFound,
                format!("book not found: 站点未收录《{title}》"),
            ));
        }

        let (used, limit) = self.quota(&session.session_id);
        events.emit(crate::types::AutomationEvent::Quota { used, total: limit });
        if used >= limit {
            return Err(AutomationError::with_quota(
                FailureClass::AccountQuotaExhausted,
                format!("daily download quota exhausted: {used}/{limit}"),
                Some((used, limit)),
            ));
        }

        if Self::matches(&self.script.retryable_titles, title) {
            return Err(AutomationError::new(
                FailureClass::Retryable,
                "模拟可重试失败：搜索结果加载超时",
            ));
        }
        if Self::matches(&self.script.uncertain_titles, title) {
            return Err(AutomationError::new(
                FailureClass::Uncertain,
                "download stalled: 模拟停滞超时，结果不确定",
            ));
        }

        cancel.check()?;
        events.stage("下载中");
        tokio::fs::create_dir_all(&spec.staging_dir)
            .await
            .map_err(|err| {
                AutomationError::new(FailureClass::Retryable, format!("创建暂存目录失败：{err}"))
            })?;

        let format = spec.book.format.to_ascii_lowercase();
        let file_name = format!("{}.{}", platform_domain::sanitize_filename(title), format);
        let staged_file = spec.staging_dir.join(file_name);
        let total = self.script.file_size_bytes;
        // 文件内容必须能通过 `verify::check_signature`：模拟引擎的作用是验证平台，
        // 而平台的入库闸门包含签名校验。写一堆填充字节就等于绕开了要验证的东西。
        let mut buffer = file_signature(&format);
        buffer.resize(total as usize, b'K');
        // 分四段上报，模拟真实的进度节奏
        let chunk = (total / 4).max(1);
        let mut written = 0u64;
        while written < total {
            written = (written + chunk).min(total);
            events.progress(written, total);
            if !cancel.sleep(self.script.step_delay).await {
                // 取消发生在文件写完之前：不留下半个文件，上层无需猜测
                return Err(cancel.check().unwrap_err());
            }
        }
        tokio::fs::write(&staged_file, &buffer)
            .await
            .map_err(|err| {
                AutomationError::new(FailureClass::Retryable, format!("写入暂存文件失败：{err}"))
            })?;

        let used = {
            let mut guard = self.used.lock().expect("模拟引擎计数锁被污染");
            let counter = guard.entry(session.session_id.clone()).or_insert(0);
            *counter += 1;
            *counter
        };

        events.stage("入库中");
        // 与真实引擎走同一条闸门：模拟引擎若跳过校验，用它做的端到端验收
        // 就验不到「校验会不会误杀正常文件」这件事。
        let evidence = crate::verify::verify_and_collect(
            &staged_file,
            title,
            &format,
            spec.minimum_size_bytes,
        )
        .map_err(|err| AutomationError::new(FailureClass::Fatal, err.to_string()))?;
        Ok(DownloadOutcome {
            staged_file,
            size_bytes: total,
            quota_indicator: Some((used, limit)),
            evidence: Some(evidence),
            match_record: Some(crate::matching::MatchRecord {
                search_term: title.to_string(),
                candidate_count: 1,
                chosen_title: title.to_string(),
                chosen_author: spec.book.author.clone().unwrap_or_default(),
                chosen_isbn: spec.book.isbn.clone().unwrap_or_default(),
                basis: "模拟引擎直接命中".to_string(),
            }),
        })
    }

    async fn register_account(
        &self,
        _session: &SessionHandle,
        spec: &RegistrationSpec,
        events: &EventSink,
    ) -> Result<RegistrationOutcome, AutomationError> {
        events.stage("注册中");
        tokio::time::sleep(self.script.step_delay).await;
        // 约定：邮箱含 "exists" 的账号模拟「站点已存在该邮箱」
        let already_exists = spec.account.email.contains("exists");
        let awaiting_verification = if spec.needs_mail_code && !already_exists {
            if let Some(provider) = &spec.mail_provider {
                match provider
                    .prepare(&spec.account.email, Duration::from_secs(5))
                    .await
                {
                    Ok(cursor) => provider.await_code(&cursor, &spec.cancel).await.is_err(),
                    Err(_) => true,
                }
            } else {
                true
            }
        } else {
            false
        };
        Ok(RegistrationOutcome {
            already_exists,
            awaiting_verification,
        })
    }

    async fn submit_verification_code(
        &self,
        _session: &SessionHandle,
        code: &str,
        events: &EventSink,
        _cancel: &CancelToken,
    ) -> Result<RegistrationOutcome, AutomationError> {
        events.stage("提交验证码中");
        if code.trim().is_empty() {
            return Ok(RegistrationOutcome {
                already_exists: false,
                awaiting_verification: true,
            });
        }
        Ok(RegistrationOutcome {
            already_exists: false,
            awaiting_verification: false,
        })
    }

    async fn read_quota_indicator(
        &self,
        session: &SessionHandle,
    ) -> Result<Option<(u32, u32)>, AutomationError> {
        Ok(Some(self.quota(&session.session_id)))
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), AutomationError> {
        self.used
            .lock()
            .expect("模拟引擎计数锁被污染")
            .remove(&session.session_id);
        // Profile 按会话创建，正常结束即删除（第 6.3 节）
        let _ = tokio::fs::remove_dir_all(&session.profile_dir).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail_code::{MailCodeCursor, MailCodeError, MailCodeProvider, MailCodeResult};
    use crate::types::{AccountCredential, BookTarget};
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::SystemTime;

    fn session_spec(root: &std::path::Path) -> SessionSpec {
        SessionSpec {
            session_id: "会话1".to_string(),
            site_base: "https://example.invalid".to_string(),
            browser_path: None,
            headless: true,
            browser_debug_port: 19220,
            profile_dir: root.join("profiles/session-会话1"),
            staging_root: root.join("staging"),
            proxy_endpoint: Some("127.0.0.1:19001".to_string()),
            account: AccountCredential {
                account_id: "账号1".to_string(),
                email: "a@example.invalid".to_string(),
                password: "secret".to_string(),
                nickname: "a".to_string(),
                daily_used: 0,
                daily_limit: 10,
            },
            download_format: "pdf".to_string(),
            auto_login: true,
            max_duration: Duration::from_secs(3600),
        }
    }

    fn download_spec(root: &std::path::Path, title: &str) -> DownloadSpec {
        DownloadSpec {
            execution_id: "执行1".to_string(),
            task_id: "任务1".to_string(),
            book: BookTarget {
                book_id: "图书1".to_string(),
                book_seq: 1,
                title: title.to_string(),
                author: None,
                publisher: None,
                isbn: None,
                format: "pdf".to_string(),
            },
            staging_dir: root.join("staging/task-任务1"),
            stall_timeout: Duration::from_secs(120),
            minimum_size_bytes: 32 * 1024,
            search_order: "bestmatch".to_string(),
            search_extensions: Vec::new(),
            attempt: 1,
        }
    }

    /// 走完「先设目录、再下载」的正常顺序，这是所有调用方必须遵守的次序。
    async fn download(
        engine: &SimulatedEngine,
        session: &SessionHandle,
        spec: &DownloadSpec,
    ) -> Result<DownloadOutcome, AutomationError> {
        engine
            .set_task_download_dir(session, &spec.staging_dir)
            .await?;
        engine
            .download_book(session, spec, &EventSink::discarding(), &CancelToken::new())
            .await
    }

    #[tokio::test]
    async fn produces_verifiable_file() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::with_defaults();
        let session = engine
            .open_session(&session_spec(dir.path()))
            .await
            .unwrap();
        let spec = download_spec(dir.path(), "算法导论");
        let outcome = download(&engine, &session, &spec).await.unwrap();

        assert!(outcome.staged_file.exists());
        assert_eq!(outcome.size_bytes, 512 * 1024);
        // 模拟文件必须通过与真实文件同一套闸门，签名校验也不例外
        let evidence =
            crate::verify::verify_and_collect(&outcome.staged_file, "算法导论", "pdf", 32 * 1024)
                .unwrap();
        assert_eq!(evidence.size_bytes, outcome.size_bytes);
        assert_eq!(evidence.sha256.len(), 64);
    }

    #[tokio::test]
    async fn refuses_to_download_before_the_task_dir_is_set() {
        // 第 8.2 节：没有先切换下载目录就点下载，文件会落进公共 staging 根目录
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::with_defaults();
        let session = engine
            .open_session(&session_spec(dir.path()))
            .await
            .unwrap();
        let err = engine
            .download_book(
                &session,
                &download_spec(dir.path(), "算法导论"),
                &EventSink::discarding(),
                &CancelToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.class, FailureClass::Fatal);
        assert!(err.reason.contains("下载目录"), "{}", err.reason);
    }

    #[tokio::test]
    async fn refuses_when_browser_dir_is_not_the_task_dir() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::with_defaults();
        let session = engine
            .open_session(&session_spec(dir.path()))
            .await
            .unwrap();
        // 设成公共 staging 根目录：多槽位并发时归属就只能靠猜
        engine
            .set_task_download_dir(&session, &dir.path().join("staging"))
            .await
            .unwrap();
        let err = engine
            .download_book(
                &session,
                &download_spec(dir.path(), "算法导论"),
                &EventSink::discarding(),
                &CancelToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.reason.contains("不一致"), "{}", err.reason);
    }

    #[tokio::test]
    async fn cancel_stops_the_download_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::new(SimulationScript {
            step_delay: Duration::from_secs(30),
            ..SimulationScript::default()
        });
        let session = engine
            .open_session(&session_spec(dir.path()))
            .await
            .unwrap();
        let spec = download_spec(dir.path(), "算法导论");
        engine
            .set_task_download_dir(&session, &spec.staging_dir)
            .await
            .unwrap();

        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            trigger.cancel("管理员取消任务");
        });

        let started = std::time::Instant::now();
        let err = engine
            .download_book(&session, &spec, &EventSink::discarding(), &cancel)
            .await
            .unwrap_err();
        // 取消必须立刻生效，而不是等这一觉睡满
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(err.reason.contains("管理员取消任务"), "{}", err.reason);
        assert!(
            !spec.staging_dir.join("算法导论.pdf").exists(),
            "取消后不应留下半个文件"
        );
    }

    #[tokio::test]
    async fn scripted_not_found_maps_to_skip() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::new(SimulationScript {
            not_found_titles: vec!["冷门书".to_string()],
            ..SimulationScript::default()
        });
        let session = engine
            .open_session(&session_spec(dir.path()))
            .await
            .unwrap();
        let err = download(&engine, &session, &download_spec(dir.path(), "冷门书"))
            .await
            .unwrap_err();
        assert_eq!(err.class, FailureClass::BookNotFound);
        assert_eq!(
            err.class.attribution().task_status,
            Some(platform_domain::TaskStatus::Skipped)
        );
    }

    #[tokio::test]
    async fn session_quota_exhausts_after_limit() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::new(SimulationScript {
            quota_limit: 2,
            file_size_bytes: 64 * 1024,
            ..SimulationScript::default()
        });
        let session = engine
            .open_session(&session_spec(dir.path()))
            .await
            .unwrap();
        for index in 0..2 {
            download(
                &engine,
                &session,
                &download_spec(dir.path(), &format!("书{index}")),
            )
            .await
            .unwrap();
        }
        let err = download(&engine, &session, &download_spec(dir.path(), "书3"))
            .await
            .unwrap_err();
        assert_eq!(err.class, FailureClass::AccountQuotaExhausted);
        assert_eq!(err.quota_indicator, Some((2, 2)));
    }

    #[tokio::test]
    async fn closing_session_removes_profile() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::with_defaults();
        let spec = session_spec(dir.path());
        let session = engine.open_session(&spec).await.unwrap();
        assert!(spec.profile_dir.exists());
        engine.close_session(&session).await.unwrap();
        assert!(!spec.profile_dir.exists());
    }

    struct SuccessfulMailProvider;

    #[async_trait]
    impl MailCodeProvider for SuccessfulMailProvider {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn prepare(
            &self,
            email: &str,
            timeout: Duration,
        ) -> Result<MailCodeCursor, MailCodeError> {
            let now = std::time::Instant::now();
            Ok(MailCodeCursor {
                email: email.to_string(),
                start_time: now,
                started_at: SystemTime::now(),
                deadline: now + timeout,
                provider_version: 1,
                prepared_by: self.name(),
                baseline_codes: HashSet::new(),
            })
        }

        async fn await_code(
            &self,
            _cursor: &MailCodeCursor,
            _cancel: &CancelToken,
        ) -> Result<MailCodeResult, MailCodeError> {
            Ok(MailCodeResult {
                code: "123456".to_string(),
            })
        }

        async fn health(&self) -> Result<(), MailCodeError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn registration_calls_mail_provider_and_finishes_verification() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SimulatedEngine::with_defaults();
        let session_specification = session_spec(dir.path());
        let session = engine.open_session(&session_specification).await.unwrap();
        let outcome = engine
            .register_account(
                &session,
                &RegistrationSpec {
                    execution_id: "exec-registration".to_string(),
                    account: session_specification.account,
                    needs_mail_code: true,
                    mail_provider: Some(Arc::new(SuccessfulMailProvider)),
                    cancel: CancelToken::new(),
                },
                &EventSink::discarding(),
            )
            .await
            .unwrap();
        assert!(!outcome.awaiting_verification);
        assert!(!outcome.already_exists);
    }
}
