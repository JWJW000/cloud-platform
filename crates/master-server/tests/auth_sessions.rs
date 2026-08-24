//! 管理端认证、会话撤销与加固的真实数据库测试（第 16.4 节、V3 方案第 11 节）。
//!
//! 验证场景：
//! - 用户被禁用后，原有 JWT 令牌立即失效；
//! - 改密后 token_version 自增，旧会话立即失效；
//! - 主动退出后当前 jti 会话撤销；
//! - 数据库当前角色动态生效。

mod support;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::Request;
use master_server::api::auth::AuthenticatedUser;
use master_server::security::{hash_password, TokenIssuer};
use master_server::state::AppState;
use master_server::store;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 与实现一致的 token SHA-256 散列。
fn token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[tokio::test]
async fn 管理员会话失效与动态查库校验() {
    let db = require_db!();

    let tokens = std::sync::Arc::new(TokenIssuer::new("1234567890123456", 12));
    let state = AppState {
        pool: db.pool.clone(),
        config: std::sync::Arc::new(master_server::config::MasterConfig {
            server: Default::default(),
            database: master_server::config::DatabaseConfig {
                url: "postgres://localhost/dummy".to_string(),
                max_connections: 5,
                auto_migrate: false,
            },
            security: master_server::config::SecurityConfig {
                jwt_secret: "1234567890123456".to_string(),
                jwt_hours: 12,
                field_key_base64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                ca_cert_path: std::path::PathBuf::from("data/ca.crt"),
                ca_key_path: std::path::PathBuf::from("data/ca.key"),
                node_cert_days: 365,
                require_client_cert: false,

                cookie_secure: true,
            },
            scheduler: Default::default(),
            nas: Default::default(),
            webshare: Default::default(),
        }),
        cipher: std::sync::Arc::new(
            master_server::security::FieldCipher::from_base64(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )
            .unwrap(),
        ),
        tokens: tokens.clone(),
        ca: std::sync::Arc::new(master_server::security::NodeCa::generate(365).unwrap()),
        events: Default::default(),
        links: Default::default(),
    };

    // 1. 创建测试用户
    let pwd_hash = hash_password("init_password").unwrap();
    let user = store::admin::create_user(&db.pool, "test_admin", &pwd_hash, "超级管理员")
        .await
        .unwrap();

    // 签发会话 1
    let session1_id = Uuid::new_v4();
    let token1 = tokens
        .issue(
            &user.id.to_string(),
            &session1_id.to_string(),
            &user.username,
            &user.role,
            user.token_version,
        )
        .unwrap();
    store::admin::create_admin_session(
        &db.pool,
        session1_id,
        user.id,
        &token_hash(&token1),
        chrono::Utc::now() + chrono::Duration::hours(12),
        None,
        None,
    )
    .await
    .unwrap();

    // 2. 正常鉴权
    let req1 = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token1}"))
        .body(())
        .unwrap();
    let (mut parts1, _) = req1.into_parts();
    let auth1 = AuthenticatedUser::from_request_parts(&mut parts1, &state)
        .await
        .expect("正常令牌应鉴权通过");
    assert_eq!(auth1.username, "test_admin");
    assert_eq!(auth1.role, "超级管理员");

    // 3. 场景 A：主动登出会话 1
    store::admin::revoke_admin_session(&db.pool, session1_id, "登出")
        .await
        .unwrap();
    let req_logout = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token1}"))
        .body(())
        .unwrap();
    let (mut parts_l, _) = req_logout.into_parts();
    let auth_l = AuthenticatedUser::from_request_parts(&mut parts_l, &state).await;
    assert!(auth_l.is_err(), "已登出撤销的会话必须鉴权失败");

    // 4. 场景 B：签发会话 2，然后修改用户密码（应自增 token_version 并撤销所有旧会话）
    let session2_id = Uuid::new_v4();
    let token2 = tokens
        .issue(
            &user.id.to_string(),
            &session2_id.to_string(),
            &user.username,
            &user.role,
            user.token_version,
        )
        .unwrap();
    store::admin::create_admin_session(
        &db.pool,
        session2_id,
        user.id,
        &token_hash(&token2),
        chrono::Utc::now() + chrono::Duration::hours(12),
        None,
        None,
    )
    .await
    .unwrap();

    let new_pwd_hash = hash_password("new_password").unwrap();
    store::admin::set_user_password(&db.pool, user.id, &new_pwd_hash)
        .await
        .unwrap();

    let req_after_pwd = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token2}"))
        .body(())
        .unwrap();
    let (mut parts_p, _) = req_after_pwd.into_parts();
    let auth_p = AuthenticatedUser::from_request_parts(&mut parts_p, &state).await;
    assert!(auth_p.is_err(), "改密后旧会话与旧版本令牌必须失效");

    // 5. 场景 C：禁用用户
    let updated_user = store::admin::get_user_by_id(&db.pool, user.id)
        .await
        .unwrap()
        .unwrap();
    let session3_id = Uuid::new_v4();
    let token3 = tokens
        .issue(
            &user.id.to_string(),
            &session3_id.to_string(),
            &user.username,
            &user.role,
            updated_user.token_version,
        )
        .unwrap();
    store::admin::create_admin_session(
        &db.pool,
        session3_id,
        user.id,
        &token_hash(&token3),
        chrono::Utc::now() + chrono::Duration::hours(12),
        None,
        None,
    )
    .await
    .unwrap();

    store::admin::set_user_status(&db.pool, user.id, "已禁用")
        .await
        .unwrap();

    let req_disabled = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token3}"))
        .body(())
        .unwrap();
    let (mut parts_d, _) = req_disabled.into_parts();
    let auth_d = AuthenticatedUser::from_request_parts(&mut parts_d, &state).await;
    assert!(auth_d.is_err(), "被禁用的用户持有的令牌必须立即失效");

    db.teardown().await;
}

/// V4-11 / V4-12：空 jti、非法 jti、未知 jti、token_hash 不匹配、session.user 与 sub 不一致
/// 必须全部失败——不允许任何一条路径绕过会话表验证。
#[tokio::test]
async fn 会话表验证失败开放路径被堵死() {
    let db = require_db!();

    let tokens = std::sync::Arc::new(TokenIssuer::new("1234567890123456", 12));
    let state = AppState {
        pool: db.pool.clone(),
        config: std::sync::Arc::new(master_server::config::MasterConfig {
            server: Default::default(),
            database: master_server::config::DatabaseConfig {
                url: "postgres://localhost/dummy".to_string(),
                max_connections: 5,
                auto_migrate: false,
            },
            security: master_server::config::SecurityConfig {
                jwt_secret: "1234567890123456".to_string(),
                jwt_hours: 12,
                field_key_base64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                ca_cert_path: std::path::PathBuf::from("data/ca.crt"),
                ca_key_path: std::path::PathBuf::from("data/ca.key"),
                node_cert_days: 365,
                require_client_cert: false,

                cookie_secure: true,
            },
            scheduler: Default::default(),
            nas: Default::default(),
            webshare: Default::default(),
        }),
        cipher: std::sync::Arc::new(
            master_server::security::FieldCipher::from_base64(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )
            .unwrap(),
        ),
        tokens: tokens.clone(),
        ca: std::sync::Arc::new(master_server::security::NodeCa::generate(365).unwrap()),
        events: Default::default(),
        links: Default::default(),
    };

    let pwd_hash = hash_password("init_password").unwrap();
    let user = store::admin::create_user(&db.pool, "test_hardening", &pwd_hash, "超级管理员")
        .await
        .unwrap();

    // 1. 空 jti：签发一个 jti 为空的令牌（模拟伪造），必须失败
    let session_real = Uuid::new_v4();
    let token_no_jti = tokens
        .issue(&user.id.to_string(), "", &user.username, &user.role, 1)
        .unwrap();
    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token_no_jti}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    assert!(
        AuthenticatedUser::from_request_parts(&mut parts, &state)
            .await
            .is_err(),
        "空 jti 必须失败"
    );
    let _ = session_real;

    // 2. 非法 jti（非 UUID）：必须失败
    let bad_token = jwt_mint(&state, &user, "not-a-uuid");
    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {bad_token}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    assert!(
        AuthenticatedUser::from_request_parts(&mut parts, &state)
            .await
            .is_err(),
        "非法 jti 必须失败"
    );

    // 3. 未知 jti（会话不存在）：必须失败
    let ghost_session = Uuid::new_v4();
    let ghost_token = jwt_mint(&state, &user, &ghost_session.to_string());
    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {ghost_token}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    assert!(
        AuthenticatedUser::from_request_parts(&mut parts, &state)
            .await
            .is_err(),
        "未知 jti 必须失败"
    );

    // 4. token_hash 不匹配：会话存在但库里存的是另一个哈希，必须失败
    let session_x = Uuid::new_v4();
    let token_x = jwt_mint(&state, &user, &session_x.to_string());
    store::admin::create_admin_session(
        &db.pool,
        session_x,
        user.id,
        "wrong-hash-value",
        chrono::Utc::now() + chrono::Duration::hours(12),
        None,
        None,
    )
    .await
    .unwrap();
    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token_x}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    assert!(
        AuthenticatedUser::from_request_parts(&mut parts, &state)
            .await
            .is_err(),
        "token_hash 不匹配必须失败"
    );

    // 5. session.user_id 与 sub 不一致：必须失败
    let other = store::admin::create_user(&db.pool, "other_user", &pwd_hash, "只读用户")
        .await
        .unwrap();
    let session_y = Uuid::new_v4();
    let token_y = tokens
        .issue(
            &user.id.to_string(), // sub = user
            &session_y.to_string(),
            &user.username,
            &user.role,
            1,
        )
        .unwrap();
    store::admin::create_admin_session(
        &db.pool,
        session_y,
        other.id, // session 属于 other
        &token_hash(&token_y),
        chrono::Utc::now() + chrono::Duration::hours(12),
        None,
        None,
    )
    .await
    .unwrap();
    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token_y}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    assert!(
        AuthenticatedUser::from_request_parts(&mut parts, &state)
            .await
            .is_err(),
        "session.user_id 与 sub 不一致必须失败"
    );

    // 6. 一切正确时通过（回归）
    let session_ok = Uuid::new_v4();
    let token_ok = tokens
        .issue(
            &user.id.to_string(),
            &session_ok.to_string(),
            &user.username,
            &user.role,
            1,
        )
        .unwrap();
    store::admin::create_admin_session(
        &db.pool,
        session_ok,
        user.id,
        &token_hash(&token_ok),
        chrono::Utc::now() + chrono::Duration::hours(12),
        None,
        None,
    )
    .await
    .unwrap();
    let req = Request::builder()
        .header(AUTHORIZATION, format!("Bearer {token_ok}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    assert!(
        AuthenticatedUser::from_request_parts(&mut parts, &state)
            .await
            .is_ok(),
        "正确令牌应通过"
    );

    db.teardown().await;
}

/// 用给定 jti 签发令牌。
fn jwt_mint(state: &AppState, user: &master_server::models::User, jti: &str) -> String {
    state
        .tokens
        .issue(&user.id.to_string(), jti, &user.username, &user.role, 1)
        .unwrap()
}
