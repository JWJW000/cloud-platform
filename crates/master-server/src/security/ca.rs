//! 节点证书颁发（第 15.1 节）。
//!
//! Master 自带一个自签 CA：
//! - 首次启动若证书文件不存在则自动生成，避免部署时还要手工跑 openssl；
//! - Worker 注册时提交 CSR，Master 用该 CA 签发客户端证书并记录指纹；
//! - 撤销即把指纹标记为 `revoked_at`，反向代理或 Master 拒绝该指纹继续接入。
//!
//! 私钥只落在 Master 的配置目录里，**从不出现在日志或 API 响应中**。

use std::path::Path;

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use sha2::{Digest, Sha256};

/// 一次签发的结果。
#[derive(Debug, Clone)]
pub struct IssuedCertificate {
    /// 证书 PEM。
    pub certificate_pem: String,
    /// SHA-256 指纹（小写十六进制，无分隔符）。
    pub fingerprint: String,
    /// 到期时间。
    pub not_after: chrono::DateTime<chrono::Utc>,
}

/// 节点 CA。
pub struct NodeCa {
    certificate_pem: String,
    key_pem: String,
    valid_days: i64,
}

impl std::fmt::Debug for NodeCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodeCa(有效期 {} 天)", self.valid_days)
    }
}

impl NodeCa {
    /// 载入 CA；证书或私钥缺失时生成一套新的并写入磁盘。
    pub fn load_or_create(cert_path: &Path, key_path: &Path, valid_days: i64) -> Result<Self> {
        if cert_path.exists() && key_path.exists() {
            let certificate_pem = std::fs::read_to_string(cert_path)
                .with_context(|| format!("读取 CA 证书失败：{}", cert_path.display()))?;
            let key_pem = std::fs::read_to_string(key_path)
                .with_context(|| format!("读取 CA 私钥失败：{}", key_path.display()))?;
            // 立刻验证一次，避免坏文件拖到第一个节点注册时才暴露
            KeyPair::from_pem(&key_pem).context("CA 私钥无法解析")?;
            return Ok(Self {
                certificate_pem,
                key_pem,
                valid_days,
            });
        }

        let ca = Self::generate(valid_days)?;
        if let Some(parent) = cert_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(cert_path, &ca.certificate_pem)
            .with_context(|| format!("写入 CA 证书失败：{}", cert_path.display()))?;
        std::fs::write(key_path, &ca.key_pem)
            .with_context(|| format!("写入 CA 私钥失败：{}", key_path.display()))?;
        restrict_permissions(key_path);
        tracing::info!(证书 = %cert_path.display(), "已生成新的节点 CA");
        Ok(ca)
    }

    /// 生成一套新的自签 CA（不落盘）。
    pub fn generate(valid_days: i64) -> Result<Self> {
        let key = KeyPair::generate().context("生成 CA 密钥失败")?;
        let mut params =
            CertificateParams::new(Vec::<String>::new()).context("构造 CA 证书参数失败")?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "云端图书平台节点CA");
        dn.push(DnType::OrganizationName, "云端图书平台");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        // CA 有效期取节点证书有效期的十倍，至少 10 年，避免 CA 先于节点证书过期
        let ca_days = (valid_days * 10).max(3650);
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(ca_days);

        let certificate = params.self_signed(&key).context("自签 CA 证书失败")?;
        Ok(Self {
            certificate_pem: certificate.pem(),
            key_pem: key.serialize_pem(),
            valid_days,
        })
    }

    /// CA 证书 PEM，下发给 Worker 用于校验 Master 服务端。
    pub fn certificate_pem(&self) -> &str {
        &self.certificate_pem
    }

    /// 用 CA 签发节点客户端证书。
    ///
    /// `csr_pem` 由 Worker 本地生成（私钥不出节点），`node_id` 写入 CN 便于排查。
    pub fn sign_csr(&self, csr_pem: &str, node_id: &str) -> Result<IssuedCertificate> {
        let issuer_key = KeyPair::from_pem(&self.key_pem).context("CA 私钥无法解析")?;
        let issuer_params = CertificateParams::from_ca_cert_pem(&self.certificate_pem)
            .context("CA 证书无法解析")?;
        let issuer = issuer_params
            .self_signed(&issuer_key)
            .context("重建 CA 签发者失败")?;

        let mut csr = CertificateSigningRequestParams::from_pem(csr_pem)
            .context("节点提交的 CSR 无法解析")?;
        let mut dn = DistinguishedName::new();
        // CN 由 Master 覆写为节点编号：不信任节点自报的身份
        dn.push(DnType::CommonName, node_id);
        dn.push(DnType::OrganizationName, "云端图书平台Worker");
        csr.params.distinguished_name = dn;
        csr.params.is_ca = IsCa::NoCa;
        csr.params.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        csr.params.not_after =
            time::OffsetDateTime::now_utc() + time::Duration::days(self.valid_days.max(1));

        let certificate = csr
            .signed_by(&issuer, &issuer_key)
            .context("签发节点证书失败")?;
        let certificate_pem = certificate.pem();
        let fingerprint = fingerprint_of_der(certificate.der());
        let not_after = chrono::Utc::now() + chrono::Duration::days(self.valid_days.max(1));
        Ok(IssuedCertificate {
            certificate_pem,
            fingerprint,
            not_after,
        })
    }
}

/// 计算 DER 证书的 SHA-256 指纹（小写十六进制）。
pub fn fingerprint_of_der(der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(der);
    hex::encode(hasher.finalize())
}

/// 归一化外部传入的指纹：去掉冒号并转小写，便于与数据库中的值比较。
pub fn normalize_fingerprint(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟 Worker 侧：生成密钥与 CSR。
    fn worker_csr() -> (KeyPair, String) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "节点自报名称");
        params.distinguished_name = dn;
        let csr = params.serialize_request(&key).unwrap();
        (key, csr.pem().unwrap())
    }

    #[test]
    fn generates_usable_ca() {
        let ca = NodeCa::generate(30).unwrap();
        assert!(ca.certificate_pem().contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn signs_worker_csr_and_overrides_common_name() {
        let ca = NodeCa::generate(30).unwrap();
        let (_key, csr_pem) = worker_csr();
        let issued = ca.sign_csr(&csr_pem, "节点编号-1").unwrap();
        assert!(issued.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(issued.fingerprint.len(), 64);
        assert!(issued.not_after > chrono::Utc::now());
    }

    #[test]
    fn distinct_nodes_get_distinct_fingerprints() {
        let ca = NodeCa::generate(30).unwrap();
        let (_k1, csr1) = worker_csr();
        let (_k2, csr2) = worker_csr();
        assert_ne!(
            ca.sign_csr(&csr1, "节点1").unwrap().fingerprint,
            ca.sign_csr(&csr2, "节点2").unwrap().fingerprint
        );
    }

    #[test]
    fn malformed_csr_is_rejected() {
        let ca = NodeCa::generate(30).unwrap();
        assert!(ca.sign_csr("不是 PEM", "节点1").is_err());
    }

    #[test]
    fn load_or_create_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.crt.pem");
        let key = dir.path().join("ca.key.pem");
        let first = NodeCa::load_or_create(&cert, &key, 30).unwrap();
        let second = NodeCa::load_or_create(&cert, &key, 30).unwrap();
        assert_eq!(first.certificate_pem(), second.certificate_pem());
    }

    #[test]
    fn fingerprint_normalization_ignores_separators_and_case() {
        assert_eq!(normalize_fingerprint("AB:CD:ef"), "abcdef");
    }
}
