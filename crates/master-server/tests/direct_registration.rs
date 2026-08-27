//! V5 Worker 直连注册集成测试（实施方案 v5 第 11.2 节）。
//!
//! 直接调用 gRPC 处理函数与 store/API，覆盖：
//! - 幂等注册（同安装标识+公钥不重复建节点）；
//! - 同主机名不同安装标识 → 两个节点；
//! - 同安装标识公钥突变 → 安全异常；
//! - 待审核节点不能领取证书/令牌；
//! - 批准后才签发证书；重复批准不重复签发；一次性领取；
//! - 已拒绝节点无法重新注册；
//! - 过期会话无法查询；
//! - 私钥持有证明错误被拒绝；
//! - 只有超级管理员能批准/拒绝。

mod support;

use master_server::api::auth::AuthenticatedUser;
use master_server::grpc::registration;
use master_server::security::csr;
use master_server::state::AppState;
use master_server::store;
use platform_proto::v1 as pb;
use tonic::Request;
use uuid::Uuid;

/// 测试 Worker 身份：安装标识 + ECDSA P-256 密钥 + CSR + ring 签名器。
struct TestWorker {
    installation_id: Uuid,
    csr_pem: String,
    fingerprint: String,
    signer: ring::signature::EcdsaKeyPair,
}

impl TestWorker {
    fn new(_name: &str) -> Self {
        use rcgen::{CertificateParams, DistinguishedName, KeyPair};
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name = DistinguishedName::new();
        let csr = params.serialize_request(&key).unwrap();
        let csr_pem = csr.pem().unwrap();
        let fingerprint = csr::csr_public_key_fingerprint(&csr_pem).unwrap();
        let der = key.serialize_der();
        let signer = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &der,
            &ring::rand::SystemRandom::new(),
        )
        .unwrap();
        Self {
            installation_id: Uuid::new_v4(),
            csr_pem,
            fingerprint,
            signer,
        }
    }

    fn register_request(&self, ip: &str) -> Request<pb::RegisterNodeRequest> {
        let nonce = "client-nonce-1234";
        let sig = self
            .signer
            .sign(&ring::rand::SystemRandom::new(), nonce.as_bytes())
            .unwrap();
        let mut req = Request::new(pb::RegisterNodeRequest {
            installation_id: self.installation_id.to_string(),
            node_name: "测试节点".to_string(),
            os_type: "linux".to_string(),
            os_version: "6.8".to_string(),
            agent_version: "0.1.0".to_string(),
            requested_slots: 5,
            csr_pem: self.csr_pem.clone(),
            public_key_fingerprint: self.fingerprint.clone(),
            nonce: nonce.to_string(),
            nonce_signature: hex::encode(sig),
        });
        req.metadata_mut()
            .insert("x-forwarded-for", ip.parse().unwrap());
        req
    }

    fn watch_request(
        &self,
        session: &str,
        challenge: &str,
    ) -> Request<pb::WatchRegistrationRequest> {
        let req = pb::WatchRegistrationRequest {
            node_id: String::new(), // 由调用方填充
            registration_session: session.to_string(),
            challenge: challenge.to_string(),
            challenge_signature: self.sign_challenge(challenge),
        };
        Request::new(req)
    }

    fn sign_challenge(&self, challenge: &str) -> String {
        let sig = self
            .signer
            .sign(&ring::rand::SystemRandom::new(), challenge.as_bytes())
            .unwrap();
        hex::encode(sig)
    }
}

fn state_for(db: &support::TestDb) -> AppState {
    AppState {
        pool: db.pool.clone(),
        config: std::sync::Arc::new(master_server::config::MasterConfig {
            server: Default::default(),
            database: master_server::config::DatabaseConfig {
                url: "postgres://localhost/dummy".to_string(),
                max_connections: 5,
                auto_migrate: false,
            },
            security: master_server::config::SecurityConfig {
                jwt_secret: "12345678901234567890123456789012".to_string(),
                jwt_hours: 12,
                field_key_base64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                ca_cert_path: std::path::PathBuf::from("data/ca.crt"),
                ca_key_path: std::path::PathBuf::from("data/ca.key"),
                node_cert_days: 365,
                require_client_cert: true,
                cookie_secure: false,
            },
            scheduler: Default::default(),
            nas: Default::default(),
            webshare: Default::default(),
            opensearch: Default::default(),
        }),
        cipher: std::sync::Arc::new(
            master_server::security::FieldCipher::from_base64(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )
            .unwrap(),
        ),
        tokens: std::sync::Arc::new(master_server::security::TokenIssuer::new(
            "12345678901234567890123456789012",
            12,
        )),
        ca: std::sync::Arc::new(master_server::security::NodeCa::generate(365).unwrap()),
        events: Default::default(),
        links: Default::default(),
        search: None,
    }
}

/// 创建真实的管理员用户（approved_by/rejected_by 外键引用 users 表）。
async fn create_admin(db: &support::TestDb) -> AuthenticatedUser {
    let user = store::admin::create_user(&db.pool, "test_admin", "散列", "超级管理员")
        .await
        .unwrap();
    AuthenticatedUser {
        id: user.id,
        username: user.username,
        role: user.role,
        session_id: None,
    }
}

/// 创建只读用户（无审批权限）。
async fn create_readonly(db: &support::TestDb) -> AuthenticatedUser {
    let user = store::admin::create_user(&db.pool, "test_readonly", "散列", "只读用户")
        .await
        .unwrap();
    AuthenticatedUser {
        id: user.id,
        username: user.username,
        role: user.role,
        session_id: None,
    }
}

#[tokio::test]
async fn 注册创建待审核节点且幂等() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("w1");

    let resp = registration::register_node(&state, worker.register_request("203.0.113.10"))
        .await
        .unwrap();
    assert_eq!(resp.registration_status, "待审核");
    assert!(!resp.registration_session.is_empty());
    assert!(!resp.challenge.is_empty());
    let node_id = Uuid::parse_str(&resp.node_id).unwrap();

    // 幂等：同安装+同公钥再注册 → 同一节点，不重复创建
    let resp2 = registration::register_node(&state, worker.register_request("203.0.113.10"))
        .await
        .unwrap();
    assert_eq!(resp2.node_id, resp.node_id);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_nodes WHERE id = $1")
        .bind(node_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "重复注册不得创建重复节点");
}

#[tokio::test]
async fn 同主机名不同安装标识是两个节点() {
    let db = require_db!();
    let state = state_for(&db);
    let a = TestWorker::new("host-a");
    let b = TestWorker::new("host-b");

    let ra = registration::register_node(&state, a.register_request("203.0.113.1"))
        .await
        .unwrap();
    let rb = registration::register_node(&state, b.register_request("203.0.113.2"))
        .await
        .unwrap();
    assert_ne!(ra.node_id, rb.node_id);
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM worker_nodes ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(names.len(), 2, "主机名相同不构成唯一身份");
}

#[tokio::test]
async fn 同安装标识公钥突变触发安全异常() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("k1");
    registration::register_node(&state, worker.register_request("203.0.113.3"))
        .await
        .unwrap();

    // 新公钥 + 同安装标识
    let imposter = TestWorker::new("k2");
    let mut req = imposter.register_request("203.0.113.3");
    let inner = req.get_mut();
    inner.installation_id = worker.installation_id.to_string();
    inner.node_name = "同一台机器".to_string();
    let err = registration::register_node(&state, req).await.unwrap_err();
    assert!(
        err.to_string().contains("公钥") || err.to_string().contains("身份异常"),
        "公钥突变必须被拒绝：{err}"
    );
}

#[tokio::test]
async fn 待审核节点不能领取证书_批准后才签发且只签发一次() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("p1");

    let reg = registration::register_node(&state, worker.register_request("198.51.100.1"))
        .await
        .unwrap();
    let node_id = Uuid::parse_str(&reg.node_id).unwrap();

    // 待审核：WatchRegistration 不得返回证书/令牌
    let mut watch = worker.watch_request(&reg.registration_session, &reg.challenge);
    watch.get_mut().node_id = reg.node_id.clone();
    let event = registration::watch_registration(&state, watch.into_inner())
        .await
        .unwrap();
    assert_eq!(event.registration_status, "待审核");
    assert!(event.client_certificate_pem.is_empty());
    assert!(event.node_token.is_empty());

    // 批准：才签发证书与令牌（使用测试 CA；审批人必须是真实用户）
    let admin = create_admin(&db).await;
    let ca = std::sync::Arc::new(master_server::security::NodeCa::generate(365).unwrap());
    let ca_for_sign = std::sync::Arc::clone(&ca);
    let (approved, _session, _token) = store::registration::approve_registration(
        &db.pool,
        node_id,
        admin.id,
        5,
        Some("测试批准"),
        move |csr| {
            ca_for_sign
                .sign_csr(csr, &node_id.to_string())
                .map_err(|e| master_server::error::AppError::bad(e.to_string()))
        },
    )
    .await
    .unwrap();
    assert_eq!(approved.registration_status, "已批准");
    assert_eq!(approved.configured_slots, Some(5));

    // 领取证书与令牌（一次性）
    let mut watch2 = worker.watch_request(&reg.registration_session, &reg.challenge);
    watch2.get_mut().node_id = reg.node_id.clone();
    let event2 = registration::watch_registration(&state, watch2.into_inner())
        .await
        .unwrap();
    assert_eq!(event2.registration_status, "已批准");
    assert!(!event2.client_certificate_pem.is_empty());
    assert!(!event2.ca_certificate_pem.is_empty());
    assert!(!event2.node_token.is_empty());

    // 重复领取被拒绝
    let mut watch3 = worker.watch_request(&reg.registration_session, &reg.challenge);
    watch3.get_mut().node_id = reg.node_id.clone();
    assert!(
        registration::watch_registration(&state, watch3.into_inner())
            .await
            .is_err(),
        "证书与令牌必须一次性领取"
    );

    // 证书记录只有一条
    let certs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_certificates WHERE node_id = $1")
            .bind(node_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(certs, 1, "批准只签发一套证书");

    // 重复批准被拒绝（注册状态已非待审核）
    assert!(
        store::registration::approve_registration(
            &db.pool,
            node_id,
            admin.id,
            5,
            None,
            move |csr| {
                ca.sign_csr(csr, &node_id.to_string())
                    .map_err(|e| master_server::error::AppError::bad(e.to_string()))
            },
        )
        .await
        .is_err(),
        "重复批准不得重复签发"
    );
}

#[tokio::test]
async fn 已拒绝节点无法重新注册() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("r1");
    let reg = registration::register_node(&state, worker.register_request("198.51.100.9"))
        .await
        .unwrap();
    let node_id = Uuid::parse_str(&reg.node_id).unwrap();

    let admin = create_admin(&db).await;
    let mut tx = db.pool.begin().await.unwrap();
    store::registration::set_node_rejected(&mut tx, node_id, admin.id, "来源设备未知")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let err = registration::register_node(&state, worker.register_request("198.51.100.9"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("拒绝") || err.to_string().contains("禁用"),
        "已拒绝节点不得绕过原决定：{err}"
    );
}

#[tokio::test]
async fn 过期注册会话无法查询() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("e1");
    let reg = registration::register_node(&state, worker.register_request("198.51.100.2"))
        .await
        .unwrap();

    sqlx::query("UPDATE worker_registration_sessions SET expires_at = now() - interval '1 minute'")
        .execute(&db.pool)
        .await
        .unwrap();

    let mut watch = worker.watch_request(&reg.registration_session, &reg.challenge);
    watch.get_mut().node_id = reg.node_id.clone();
    let err = registration::watch_registration(&state, watch.into_inner())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("过期"),
        "过期会话必须被拒绝：{err}"
    );
}

#[tokio::test]
async fn 私钥持有证明错误被拒绝() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("s1");
    let reg = registration::register_node(&state, worker.register_request("198.51.100.4"))
        .await
        .unwrap();

    // 错误的挑战签名（伪造者用别的密钥签）
    let imposter = TestWorker::new("s2");
    let mut watch = Request::new(pb::WatchRegistrationRequest {
        node_id: reg.node_id.clone(),
        registration_session: reg.registration_session.clone(),
        challenge: reg.challenge.clone(),
        challenge_signature: imposter.sign_challenge(&reg.challenge),
    });
    watch.get_mut().node_id = reg.node_id.clone();
    assert!(
        registration::watch_registration(&state, watch.into_inner())
            .await
            .is_err(),
        "私钥持有证明错误必须拒绝"
    );
}

#[tokio::test]
async fn 只有超级管理员能批准和拒绝() {
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("perm1");
    let reg = registration::register_node(&state, worker.register_request("198.51.100.5"))
        .await
        .unwrap();
    let node_id = Uuid::parse_str(&reg.node_id).unwrap();

    // 只读用户调用 approve_worker → Forbidden
    let readonly = create_readonly(&db).await;
    let err = master_server::api::workers::approve_worker(
        axum::extract::State(state.clone()),
        readonly.clone(),
        axum::extract::Path(node_id),
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("超级管理员"), "{err}");

    let err = master_server::api::workers::reject_worker(
        axum::extract::State(state),
        readonly,
        axum::extract::Path(node_id),
        axum::Json(master_server::api::workers::RejectRequest {
            reason: "测试".to_string(),
        }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("超级管理员"), "{err}");
}

#[tokio::test]
async fn 待审核节点无法建立正式任务链路() {
    // V5 第 6.2 节：注册 ≠ 授权。待审核/已拒绝/已过期节点即使拿到证书凭据，
    // 方法级鉴权也必须拒绝 OpenLink（未批准节点不得领取任务）。
    let db = require_db!();
    let state = state_for(&db);
    let worker = TestWorker::new("link1");
    let reg = registration::register_node(&state, worker.register_request("198.51.100.7"))
        .await
        .unwrap();
    let node_id = Uuid::parse_str(&reg.node_id).unwrap();

    // 待审核节点：即使伪造节点令牌（这里用空令牌，因为批准前根本没有令牌），
    // 也必须被拒绝——不能领到任何任务。
    let mut meta = tonic::metadata::MetadataMap::new();
    meta.insert("x-node-id", reg.node_id.clone().parse().unwrap());
    meta.insert("x-node-token", "伪造令牌".parse().unwrap());
    let err = master_server::grpc::auth::authenticate(&state, &meta, true)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("未获批准") || err.to_string().contains("凭据"),
        "{err}"
    );
    let _ = node_id;

    // 批准后：没有证书指纹也必须被拒绝（OpenLink 强制 mTLS）
    let admin = create_admin(&db).await;
    let ca = std::sync::Arc::new(master_server::security::NodeCa::generate(365).unwrap());
    let ca_for_sign = std::sync::Arc::clone(&ca);
    let (_approved, _session, token) = store::registration::approve_registration(
        &db.pool,
        node_id,
        admin.id,
        2,
        None,
        move |csr| {
            ca_for_sign
                .sign_csr(csr, &node_id.to_string())
                .map_err(|e| master_server::error::AppError::bad(e.to_string()))
        },
    )
    .await
    .unwrap();

    let mut meta = tonic::metadata::MetadataMap::new();
    meta.insert("x-node-id", reg.node_id.clone().parse().unwrap());
    meta.insert("x-node-token", token.parse().unwrap());
    // 没有证书指纹 → 强制拒绝（即使 require_client_cert=false 的部署）
    let err = master_server::grpc::auth::authenticate(&state, &meta, true)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("客户端证书"), "{err}");

    // 有证书指纹但不匹配 → 拒绝
    let mut meta = tonic::metadata::MetadataMap::new();
    meta.insert("x-node-id", reg.node_id.clone().parse().unwrap());
    meta.insert("x-node-token", token.parse().unwrap());
    meta.insert(
        "x-client-cert-fingerprint",
        "0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap(),
    );
    let err = master_server::grpc::auth::authenticate(&state, &meta, true)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("证书"), "{err}");
}
