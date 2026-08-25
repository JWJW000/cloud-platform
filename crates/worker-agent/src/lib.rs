//! `worker-agent` 库模块声明。

pub mod bus;
pub mod client;
pub mod config;
pub mod credential_store;
pub mod dynamic;
pub mod inventory;
pub mod mail;
pub mod master_port;
pub mod outbox;
pub mod proxy_forward;
pub mod registration;
pub mod runtime;
pub mod slot;
pub mod storage;
pub mod tls;
pub mod transport;

pub use config::{SavedIdentity, WorkerConfig};
pub use credential_store::{CredentialStore, FsCredentialStore, InMemoryCredentialStore};
pub use master_port::{MasterPort, RegistrationOutcome};
pub use runtime::WorkerRuntime;
pub use transport::TonicMasterAdapter;
