//! CSR 公钥指纹与签名校验（V5 直连注册用，第 6.5 节）。
//!
//! Worker 直连注册时提交 CSR；Master 必须：
//! 1. 从 CSR 提取公钥并计算 SHA-256 指纹（与 Worker 上报值复核，防伪造）；
//! 2. 验证 Worker 用 CSR 私钥对 nonce/challenge 的签名（私钥持有证明）。
//!
//! 密钥约定：Worker 使用 ECDSA P-256（rcgen 默认），签名编码为 ASN.1 DER。

use anyhow::Result;
use sha2::{Digest, Sha256};
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::pem::parse_x509_pem;
use x509_parser::prelude::FromDer;

/// 从 CSR PEM 提取公钥指纹（SHA-256，小写十六进制）。
pub fn csr_public_key_fingerprint(csr_pem: &str) -> Result<String> {
    let public_key = csr_public_key(csr_pem)?;
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    Ok(hex::encode(hasher.finalize()))
}

/// 从 CSR PEM 提取公钥原始字节（EC 未压缩点）。
fn csr_public_key(csr_pem: &str) -> Result<Vec<u8>> {
    let (_, pem) =
        parse_x509_pem(csr_pem.as_bytes()).map_err(|e| anyhow::anyhow!("CSR 不是合法 PEM：{e}"))?;
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents)
        .map_err(|e| anyhow::anyhow!("CSR DER 解析失败：{e}"))?;
    let spki = &csr.certification_request_info.subject_pki;
    Ok(spki.subject_public_key.data.to_vec())
}

/// 校验「CSR 公钥」对 `message` 的 ECDSA P-256 签名（十六进制编码）。
pub fn verify_public_key_signature(
    csr_pem: &str,
    message: &str,
    signature_hex: &str,
) -> Result<()> {
    use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};

    let public_key = csr_public_key(csr_pem)?;
    let signature = hex::decode(signature_hex.trim())
        .map_err(|e| anyhow::anyhow!("签名不是合法十六进制：{e}"))?;
    let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, &public_key);
    key.verify(message.as_bytes(), &signature)
        .map_err(|_| anyhow::anyhow!("私钥持有证明签名校验失败"))
}

/// 生成服务端挑战值（256 位随机熵，十六进制）。
pub fn new_challenge() -> String {
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut raw);
    hex::encode(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_csr() -> (rcgen::KeyPair, String, ring::signature::EcdsaKeyPair) {
        use rcgen::{CertificateParams, DistinguishedName, KeyPair};
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name = DistinguishedName::new();
        let csr = params.serialize_request(&key).unwrap();
        let csr_pem = csr.pem().unwrap();
        // 从私钥构造 ring 签名器（与 Worker 侧流程一致）
        let der = key.serialize_der();
        let kp = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &der,
            &ring::rand::SystemRandom::new(),
        )
        .unwrap();
        (key, csr_pem, kp)
    }

    #[test]
    fn fingerprint_is_stable_and_matches_public_key() {
        let (_key, csr_pem, _kp) = worker_csr();
        let fp = csr_public_key_fingerprint(&csr_pem).unwrap();
        assert_eq!(fp.len(), 64);
        assert_eq!(fp, csr_public_key_fingerprint(&csr_pem).unwrap());
    }

    #[test]
    fn valid_signature_is_accepted() {
        let (_key, csr_pem, kp) = worker_csr();
        let message = "challenge-value";
        let sig = kp
            .sign(&ring::rand::SystemRandom::new(), message.as_bytes())
            .unwrap();
        let sig_hex = hex::encode(sig);
        assert!(verify_public_key_signature(&csr_pem, message, &sig_hex).is_ok());
        // 篡改消息必须失败
        assert!(verify_public_key_signature(&csr_pem, "other-message", &sig_hex).is_err());
    }

    #[test]
    fn wrong_key_signature_is_rejected() {
        let (_key, csr_pem, _kp) = worker_csr();
        let (other_key, _other_csr, _) = worker_csr();
        let _ = other_key;
        let message = "challenge-value";
        let (_k2, _c2, kp2) = worker_csr();
        let sig = kp2
            .sign(&ring::rand::SystemRandom::new(), message.as_bytes())
            .unwrap();
        assert!(verify_public_key_signature(&csr_pem, message, &hex::encode(sig)).is_err());
    }

    #[test]
    fn malformed_csr_is_rejected() {
        assert!(csr_public_key_fingerprint("不是PEM").is_err());
        assert!(verify_public_key_signature("不是PEM", "m", "aa").is_err());
    }
}
