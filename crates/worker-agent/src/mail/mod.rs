//! 邮件验证码 Provider 实现与路由模块。

pub mod manual;
pub mod mock;
pub mod outlook_http;
pub mod router;

pub use manual::ManualMailCodeAdapter;
pub use mock::MockMailCodeAdapter;
pub use outlook_http::{OutlookConfig, OutlookHttpMailCodeAdapter};
pub use router::{MailCodeProviderConfig, MailCodeRouter};
