//! 管理员密码散列（第 15.2 节）：Argon2id + 随机盐。

use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;

/// 计算密码散列（PHC 字符串，自带盐与参数）。
pub fn hash_password(plaintext: &str) -> Result<String> {
    // 直接用 rand 生成盐字节，避免依赖 argon2 的可选随机数特性
    let mut salt_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt_bytes);
    let salt =
        SaltString::encode_b64(&salt_bytes).map_err(|err| anyhow::anyhow!("生成盐失败：{err}"))?;
    let hash = Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map_err(|err| anyhow::anyhow!("密码散列失败：{err}"))?;
    Ok(hash.to_string())
}

/// 校验密码。散列串损坏时返回错误，密码不匹配时返回 `Ok(false)`。
pub fn verify_password(plaintext: &str, stored: &str) -> Result<bool> {
    let parsed = PasswordHash::new(stored)
        .map_err(|err| anyhow::anyhow!("已存储的密码散列无法解析：{err}"))
        .context("密码校验中断")?;
    Ok(Argon2::default()
        .verify_password(plaintext.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_correct_password() {
        let hash = hash_password("管理员密码-2026").unwrap();
        assert!(verify_password("管理员密码-2026", &hash).unwrap());
    }

    #[test]
    fn rejects_wrong_password() {
        let hash = hash_password("正确密码").unwrap();
        assert!(!verify_password("错误密码", &hash).unwrap());
    }

    #[test]
    fn hashes_are_salted() {
        assert_ne!(
            hash_password("同一个密码").unwrap(),
            hash_password("同一个密码").unwrap()
        );
    }

    #[test]
    fn corrupted_hash_is_an_error_not_a_pass() {
        assert!(verify_password("任何密码", "这不是 PHC 字符串").is_err());
    }
}
