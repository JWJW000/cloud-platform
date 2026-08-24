//! 安全相关：密码散列、会话令牌、字段加密、节点证书。

pub mod ca;
pub mod crypto;
pub mod csr;
pub mod jwt;
pub mod password;

pub use ca::{normalize_fingerprint, IssuedCertificate, NodeCa};
pub use crypto::FieldCipher;
pub use jwt::{Claims, TokenIssuer};
pub use password::{hash_password, verify_password};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// 生成一次性节点注册码（第 15.1 节）。
///
/// 使用 URL 安全字符集，管理员可以直接粘进命令行。
pub fn new_enroll_code() -> String {
    let mut raw = [0u8; 18];
    rand::thread_rng().fill_bytes(&mut raw);
    BASE64_URL.encode(raw)
}

/// 生成节点长期凭据。数据库只保存其散列，明文仅在注册响应中返回一次。
pub fn new_node_token() -> String {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    BASE64_URL.encode(raw)
}

/// 对节点凭据取散列（SHA-256 十六进制）。
///
/// 凭据是高熵随机串，无需 Argon2 那样的慢哈希，但仍不能明文入库。
pub fn hash_node_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// 通用令牌哈希（SHA-256 十六进制）。
pub fn hash_token(token: &str) -> String {
    hash_node_token(token)
}

/// 常量时间比较，避免凭据校验被计时攻击区分前缀。
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let a = new_node_token();
        let b = new_node_token();
        assert_ne!(a, b);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn token_hash_is_stable_and_hides_plaintext() {
        let token = new_node_token();
        let hash = hash_node_token(&token);
        assert_eq!(hash, hash_node_token(&token));
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains(&token));
    }

    #[test]
    fn constant_time_eq_matches_semantics() {
        assert!(constant_time_eq("同一个值", "同一个值"));
        assert!(!constant_time_eq("值A", "值B"));
        assert!(!constant_time_eq("短", "长一些"));
    }

    #[test]
    fn enroll_codes_are_unique() {
        assert_ne!(new_enroll_code(), new_enroll_code());
    }
}
