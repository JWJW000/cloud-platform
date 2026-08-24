//! 取消令牌（第 10.1 节）。
//!
//! 为什么不复用命令通道来传递取消：槽位在等真实下载时会长时间停在
//! `download_book` 里面，此刻没有任何人在读命令通道。V2 第 3.6 节记录的
//! 「取消命令无法打断任务」就是这个结构造成的。
//!
//! 令牌把「要停」这个事实变成一个**可以被随处轮询、也可以被 `select!` 等待**
//! 的共享状态，于是浏览器自动化在每个等待点都能自己发现该退出了，
//! 不需要回到命令循环。
//!
//! 令牌一旦触发就不可撤销。这是刻意的：取消的语义是「这次执行到此为止」，
//! 而一个能被重置的标志会让「我到底该不该继续」在并发下没有确定答案。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

#[derive(Debug)]
struct Inner {
    cancelled: AtomicBool,
    reason: Mutex<String>,
    notify: Notify,
}

/// 可克隆的取消令牌。所有克隆共享同一个状态。
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// 新建一个未取消的令牌。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                reason: Mutex::new(String::new()),
                notify: Notify::new(),
            }),
        }
    }

    /// 触发取消。重复调用时保留**第一个**原因。
    ///
    /// 保留第一个而不是最后一个：先到的那个才是真正让执行停下来的原因，
    /// 后续的取消（例如会话超时紧跟在管理员取消之后）只是连带效果。
    pub fn cancel(&self, reason: impl Into<String>) {
        let reason = reason.into();
        // 先写原因再置位：任何看到 `is_cancelled() == true` 的读者都能读到原因。
        {
            let mut guard = match self.inner.reason.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if guard.is_empty() {
                *guard = reason;
            }
        }
        let first = !self.inner.cancelled.swap(true, Ordering::SeqCst);
        if first {
            self.inner.notify.notify_waiters();
        }
    }

    /// 是否已被取消。
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// 取消原因；未取消时为 `None`。
    pub fn reason(&self) -> Option<String> {
        if !self.is_cancelled() {
            return None;
        }
        let guard = match self.inner.reason.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        Some(if guard.is_empty() {
            "已取消".to_string()
        } else {
            guard.clone()
        })
    }

    /// 等待取消发生。已经取消时立即返回。
    ///
    /// `notified()` 必须在检查标志**之后**再等待才有竞态安全性，
    /// 因此这里先注册等待、再复查一次标志。
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let waiter = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        waiter.await;
    }

    /// 已取消时返回一个中文原因错误，便于在 `?` 链上直接短路。
    pub fn check(&self) -> Result<(), crate::types::AutomationError> {
        match self.reason() {
            Some(reason) => Err(crate::types::AutomationError::new(
                platform_domain::FailureClass::Uncertain,
                format!("执行已取消：{reason}"),
            )),
            None => Ok(()),
        }
    }

    /// 可被取消打断的休眠。返回 `false` 表示是被取消唤醒的。
    ///
    /// 真实自动化里到处是「等页面」「等文件稳定」这类休眠。把它们统一换成
    /// 这个方法，取消延迟就从「一次休眠时长」降到「立即」。
    pub async fn sleep(&self, duration: std::time::Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(duration) => true,
            _ = self.cancelled() => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        assert_eq!(token.reason(), None);
        assert!(token.check().is_ok());
    }

    #[test]
    fn cancel_records_the_first_reason() {
        let token = CancelToken::new();
        token.cancel("管理员取消任务");
        token.cancel("会话已达最大时长");
        assert_eq!(token.reason().as_deref(), Some("管理员取消任务"));
    }

    #[test]
    fn clones_share_state() {
        let token = CancelToken::new();
        let clone = token.clone();
        token.cancel("结束会话且不完成当前任务");
        assert!(clone.is_cancelled());
        assert!(clone.check().is_err());
    }

    #[tokio::test]
    async fn cancelled_returns_immediately_when_already_cancelled() {
        let token = CancelToken::new();
        token.cancel("已取消");
        // 没有超时包裹也应当立即返回；卡住就是测试挂起，正是我们想暴露的
        token.cancelled().await;
    }

    #[tokio::test]
    async fn waiter_is_woken_by_cancel() {
        let token = CancelToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
            waiter.reason()
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        token.cancel("代理连接已确认失效");
        assert_eq!(handle.await.unwrap().as_deref(), Some("代理连接已确认失效"));
    }

    #[tokio::test]
    async fn sleep_is_interrupted_by_cancel() {
        let token = CancelToken::new();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger.cancel("取消");
        });
        let started = std::time::Instant::now();
        let completed = token.sleep(Duration::from_secs(30)).await;
        assert!(!completed, "应当是被取消唤醒而不是睡满");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn sleep_completes_normally_without_cancel() {
        let token = CancelToken::new();
        assert!(token.sleep(Duration::from_millis(5)).await);
    }
}
