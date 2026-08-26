//! Worker 运行时状态机测试（实施方案 v7 第 12.1 节）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use platform_proto::v1 as pb;
use worker_agent::credential_store::{
    CredentialStore, InMemoryCredentialStore, LocalCredentialState,
};
use worker_agent::master_port::{
    ClientCredential, ConnectError, EnsureRegistrationRequestDto, MasterLinkSession, MasterPort,
    RegistrationOutcome,
};
use worker_agent::runtime::WorkerRuntime;
use worker_agent::WorkerConfig;

/// 测试专用的内存 MasterPort 适配器。
#[derive(Clone)]
struct InMemoryMasterAdapter {
    registration_calls: Arc<AtomicUsize>,
    outcome_sequence: Arc<tokio::sync::Mutex<Vec<Result<RegistrationOutcome, ConnectError>>>>,
    open_link_called: Arc<AtomicBool>,
}

struct RecordingSession {
    sent: Arc<AtomicUsize>,
}

impl MasterLinkSession for RecordingSession {
    fn send(&mut self, _msg: pb::WorkerMessage) -> Result<(), ConnectError> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn inbound_stream(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<pb::MasterMessage, ConnectError>> + Send + 'static>,
    > {
        Box::pin(stream::empty())
    }
}

#[derive(Clone)]
struct OneSessionThenStopAdapter {
    open_calls: Arc<AtomicUsize>,
    sent: Arc<AtomicUsize>,
}

#[async_trait]
impl MasterPort for OneSessionThenStopAdapter {
    async fn ensure_registration(
        &self,
        _request: EnsureRegistrationRequestDto,
    ) -> Result<RegistrationOutcome, ConnectError> {
        unreachable!("ready credential must not register again")
    }

    async fn open_link(
        &self,
        _credential: &ClientCredential,
    ) -> Result<Box<dyn MasterLinkSession>, ConnectError> {
        if self.open_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::new(RecordingSession {
                sent: self.sent.clone(),
            }))
        } else {
            Err(ConnectError::Fatal(anyhow::anyhow!(
                "stop after first session"
            )))
        }
    }
}

impl InMemoryMasterAdapter {
    fn new(outcomes: Vec<Result<RegistrationOutcome, ConnectError>>) -> Self {
        Self {
            registration_calls: Arc::new(AtomicUsize::new(0)),
            outcome_sequence: Arc::new(tokio::sync::Mutex::new(outcomes)),
            open_link_called: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl MasterPort for InMemoryMasterAdapter {
    async fn ensure_registration(
        &self,
        _req: EnsureRegistrationRequestDto,
    ) -> Result<RegistrationOutcome, ConnectError> {
        self.registration_calls.fetch_add(1, Ordering::SeqCst);
        let mut list = self.outcome_sequence.lock().await;
        if !list.is_empty() {
            list.remove(0)
        } else {
            Ok(RegistrationOutcome::Pending {
                node_id: "test-node".to_string(),
                retry_after: Duration::from_millis(50),
            })
        }
    }

    async fn open_link(
        &self,
        _credential: &ClientCredential,
    ) -> Result<Box<dyn MasterLinkSession>, ConnectError> {
        self.open_link_called.store(true, Ordering::SeqCst);
        Err(ConnectError::Fatal(anyhow::anyhow!("测试断开终止")))
    }
}

fn dummy_config() -> WorkerConfig {
    let toml_str = r#"
        [master]
        endpoint = "http://127.0.0.1:9443"
        insecure = true

        [storage]
        data_dir = "target/test_data_dir"
        nas_mount = "target/test_nas_dir"

        [execution]
        requested_slots = 3
        simulated = true
    "#;
    toml::from_str(toml_str).unwrap()
}

#[tokio::test]
async fn uninitialized_starts_fresh_and_requests_registration() {
    let cred_store = InMemoryCredentialStore::new();
    assert!(matches!(
        cred_store.load_state().unwrap(),
        LocalCredentialState::Uninitialized
    ));

    let master = InMemoryMasterAdapter::new(vec![
        Ok(RegistrationOutcome::Pending {
            node_id: "node-1".to_string(),
            retry_after: Duration::from_millis(10),
        }),
        Ok(RegistrationOutcome::Approved {
            node_id: "node-1".to_string(),
            approved_slots: 3,
            client_certificate_pem: "fake-cert-pem".to_string(),
        }),
    ]);

    let runtime = WorkerRuntime::new(master.clone(), cred_store.clone());
    let _ = runtime.run(dummy_config()).await;

    // 验证调用了两次注册
    assert_eq!(master.registration_calls.load(Ordering::SeqCst), 2);
    // 验证最终凭据保存为就绪状态
    match cred_store.load_state().unwrap() {
        LocalCredentialState::Ready { credential } => {
            assert_eq!(credential.node_id, "node-1");
            assert_eq!(credential.client_cert_pem, "fake-cert-pem");
        }
        _ => panic!("Expected Ready state"),
    }
}

#[tokio::test]
async fn rejected_registration_stops_immediately() {
    let cred_store = InMemoryCredentialStore::new();
    let master = InMemoryMasterAdapter::new(vec![Ok(RegistrationOutcome::Rejected {
        reason: "测试拒绝".to_string(),
    })]);

    let runtime = WorkerRuntime::new(master.clone(), cred_store.clone());
    let res = runtime.run(dummy_config()).await;

    assert!(res.is_err());
    let err_msg = res.unwrap_err().to_string();
    assert!(err_msg.contains("测试拒绝"));
    assert_eq!(master.registration_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ready_credentials_directly_connects_mtls() {
    let cred_store = InMemoryCredentialStore::with_ready(
        "test-installation-id",
        "node-ready",
        &rcgen::KeyPair::generate().unwrap().serialize_pem(),
        "ready-cert-pem",
    );

    let master = InMemoryMasterAdapter::new(vec![]);
    let runtime = WorkerRuntime::new(master.clone(), cred_store.clone());
    let _ = runtime.run(dummy_config()).await;

    // 无需调用 EnsureRegistration
    assert_eq!(master.registration_calls.load(Ordering::SeqCst), 0);
    // 直接尝试 open_link
    assert!(master.open_link_called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn approved_runtime_uses_the_opened_master_session_for_business_messages() {
    let cred_store = InMemoryCredentialStore::with_ready(
        "test-installation-id",
        "node-ready",
        &rcgen::KeyPair::generate().unwrap().serialize_pem(),
        "ready-cert-pem",
    );
    let master = OneSessionThenStopAdapter {
        open_calls: Arc::new(AtomicUsize::new(0)),
        sent: Arc::new(AtomicUsize::new(0)),
    };
    let mut config = dummy_config();
    let suffix = uuid::Uuid::new_v4();
    config.storage.data_dir = format!("target/runtime-session-{suffix}").into();
    config.storage.nas_mount = format!("target/runtime-session-nas-{suffix}").into();

    let runtime = WorkerRuntime::new(master.clone(), cred_store);
    let result = runtime.run(config).await;

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("stop after first session"));
    assert_eq!(master.open_calls.load(Ordering::SeqCst), 2);
    assert!(
        master.sent.load(Ordering::SeqCst) > 0,
        "NodeOnline must flow through the exact MasterPort session that passed mTLS"
    );
}
