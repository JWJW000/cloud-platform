//! 字段级加密（第 15.3 节）。
//!
//! 账号密码与代理密码在写入 PostgreSQL 之前用 AES-256-GCM 加密，
//! 密钥来自配置或环境变量，**不与数据库备份存放在同一处**。
//! 因此拿到数据库转储的人无法直接得到可登录的凭据。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::RngCore;

/// GCM 随机数长度（字节）。
const NONCE_LEN: usize = 12;

/// 字段加密器。
#[derive(Clone)]
pub struct FieldCipher {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for FieldCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 绝不打印密钥内容
        f.write_str("FieldCipher(已加载)")
    }
}

impl FieldCipher {
    /// 从 base64 编码的 32 字节密钥构造。
    pub fn from_base64(encoded: &str) -> Result<Self> {
        let raw = BASE64
            .decode(encoded.trim())
            .context("字段加密密钥不是合法的 base64")?;
        if raw.len() != 32 {
            bail!("字段加密密钥必须是 32 字节（当前 {} 字节）", raw.len());
        }
        let key = Key::<Aes256Gcm>::from_slice(&raw);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// 生成一个新的 base64 密钥，供 `master-server keygen` 输出。
    pub fn generate_key_base64() -> String {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        BASE64.encode(raw)
    }

    /// 加密明文，返回 `base64(nonce || 密文)`。
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| anyhow::anyhow!("字段加密失败"))?;
        let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(BASE64.encode(combined))
    }

    /// 解密 `encrypt` 的输出。
    pub fn decrypt(&self, encoded: &str) -> Result<String> {
        let combined = BASE64
            .decode(encoded.trim())
            .context("密文不是合法的 base64")?;
        if combined.len() <= NONCE_LEN {
            bail!("密文长度不足，数据可能已损坏");
        }
        let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("字段解密失败：密钥不匹配或数据被篡改"))?;
        String::from_utf8(plaintext).context("解密结果不是合法的 UTF-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> FieldCipher {
        FieldCipher::from_base64(&FieldCipher::generate_key_base64()).unwrap()
    }

    #[test]
    fn round_trip() {
        let cipher = cipher();
        let secret = "对应站点账号的密码 P@ssw0rd";
        let encrypted = cipher.encrypt(secret).unwrap();
        assert_ne!(encrypted, secret);
        assert_eq!(cipher.decrypt(&encrypted).unwrap(), secret);
    }

    #[test]
    fn same_plaintext_yields_different_ciphertext() {
        // 随机 nonce：相同密码不会产生相同密文，避免从库里看出「哪些账号共用密码」
        let cipher = cipher();
        assert_ne!(
            cipher.encrypt("同一个密码").unwrap(),
            cipher.encrypt("同一个密码").unwrap()
        );
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let encrypted = cipher().encrypt("秘密").unwrap();
        assert!(cipher().decrypt(&encrypted).is_err());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let cipher = cipher();
        let encrypted = cipher.encrypt("秘密").unwrap();
        let mut raw = BASE64.decode(&encrypted).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        assert!(cipher.decrypt(&BASE64.encode(raw)).is_err());
    }

    #[test]
    fn rejects_wrong_key_length() {
        assert!(FieldCipher::from_base64(&BASE64.encode([0u8; 16])).is_err());
    }
}
