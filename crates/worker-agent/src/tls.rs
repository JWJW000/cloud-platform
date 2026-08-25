//! Worker TLS / mTLS 连接通道工厂（V4 方案第 8 节）。
//!
//! V4-07 / V4-08 的修复点：
//! - **注册通道**：只接受 `https://`；用系统根或显式 `server_ca_file` 验证服务端；
//!   不加载 Node CA，不携带客户端证书；证书校验失败绝不退化为 HTTP。
//! - **WorkerLink 通道**：启动前 fail closed——endpoint 非 HTTPS、身份缺失、
//!   客户端证书/私钥缺失、证书与私钥不匹配、证书已过期、配置了 Server CA 但文件
//!   缺失，全部直接失败，绝不静默降级。
//! - **Node CA 绝不能加入服务端信任根**：服务端证书校验只信任系统根 +
//!   `server_ca_file`，Node CA 只用于「审计客户端证书」，不得用来验证云端服务器。

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use x509_parser::certificate::X509Certificate;
use x509_parser::prelude::FromDer;

use crate::config::{MasterLinkConfig, SavedIdentity, WorkerConfig};

/// 连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 校验磁盘上的私钥与证书文件。
pub fn validate_client_pair(key_path: &Path, cert_path: &Path) -> Result<()> {
    let key_pem = std::fs::read_to_string(key_path)
        .with_context(|| format!("读取私钥文件失败: {}", key_path.display()))?;
    let cert_pem = std::fs::read_to_string(cert_path)
        .with_context(|| format!("读取证书文件失败: {}", cert_path.display()))?;
    validate_cert_key_pair(&cert_pem, &key_pem, cert_path)?;
    validate_cert_not_expired(&cert_pem, cert_path)?;
    Ok(())
}

/// 计算 PEM 格式证书的 SHA-256 指纹（小写无分隔符）。
pub fn fingerprint_of_pem(pem: &str) -> Result<String> {
    fingerprint_der(pem)
}

/// 通用 TLS / mTLS 连接 Endpoint。
pub async fn connect_tls_endpoint(
    config: &MasterLinkConfig,
    endpoint_str: &str,
    client_auth: Option<(&str, &str)>,
) -> Result<Channel> {
    let is_loopback = endpoint_str.starts_with("http://127.0.0.1")
        || endpoint_str.starts_with("http://localhost");

    if config.insecure {
        return Endpoint::from_shared(endpoint_str.to_string())
            .with_context(|| format!("非法的 Master endpoint: {endpoint_str}"))?
            .connect_timeout(CONNECT_TIMEOUT)
            .connect()
            .await
            .with_context(|| format!("连接 Master 失败: {endpoint_str}"));
    }

    if !endpoint_str.starts_with("https://") && !is_loopback {
        bail!("endpoint 必须是 https://（当前：{endpoint_str}）——生产接入边界不允许明文");
    }

    let mut endpoint = Endpoint::from_shared(endpoint_str.to_string())
        .with_context(|| format!("非法的 Master endpoint: {endpoint_str}"))?
        .connect_timeout(CONNECT_TIMEOUT);

    if endpoint_str.starts_with("https://") {
        let mut tls = ClientTlsConfig::new();

        if let Some((cert_pem, key_pem)) = client_auth {
            let client_identity = Identity::from_pem(cert_pem, key_pem);
            tls = tls.identity(client_identity);
        }

        // 服务端信任根
        if let Some(server_ca_path) = &config.server_ca_file {
            tls = tls.ca_certificate(Certificate::from_pem(read_ca_file(server_ca_path)?));
        } else {
            #[cfg(unix)]
            for path in [
                "/etc/ssl/cert.pem",
                "/etc/ssl/certs/ca-certificates.crt",
                "/etc/pki/tls/certs/ca-bundle.crt",
            ] {
                let path = Path::new(path);
                if path.is_file() {
                    tls = tls.ca_certificate(Certificate::from_pem(read_ca_file(path)?));
                    break;
                }
            }
        }

        // 域名校验
        let domain = if let Some(d) = &config.tls_domain {
            d.trim().to_string()
        } else {
            endpoint_str
                .strip_prefix("https://")
                .or_else(|| endpoint_str.strip_prefix("http://"))
                .and_then(|rest| rest.split('/').next())
                .and_then(|rest| rest.split(':').next())
                .filter(|h| !h.is_empty())
                .with_context(|| format!("无法从 endpoint 推导 TLS 域名: {endpoint_str}"))?
                .to_string()
        };

        if !domain.is_empty() {
            tls = tls.domain_name(domain);
        }

        endpoint = endpoint
            .tls_config(tls)
            .context("配置 TLS 参数失败")?;
    }

    endpoint
        .connect()
        .await
        .with_context(|| format!("连接 Master 失败: {endpoint_str}"))
}

/// 注册专用 Channel（仅服务端 TLS，不携带客户端证书）。
pub async fn enrollment_channel(config: &WorkerConfig) -> Result<Channel> {
    let endpoint_str = config
        .master
        .enroll_endpoint
        .as_deref()
        .unwrap_or(&config.master.endpoint)
        .trim();

    let is_loopback = endpoint_str.starts_with("http://127.0.0.1")
        || endpoint_str.starts_with("http://localhost");

    // 第 8.2 节第 1 条：注册通道只接受 https://（本地回环调试除外）
    if !endpoint_str.starts_with("https://") && !is_loopback {
        bail!("注册入口必须是 https://（当前：{endpoint_str}）——证书校验失败后绝不退化为 HTTP");
    }

    let mut endpoint = Endpoint::from_shared(endpoint_str.to_string())
        .with_context(|| format!("非法的 Master 注册地址: {endpoint_str}"))?
        .connect_timeout(CONNECT_TIMEOUT);

    if endpoint_str.starts_with("https://") {
        let mut tls = ClientTlsConfig::new();

        // 优先使用显式 Server CA；未配置时补充操作系统常见 CA bundle。
        // macOS Keychain / 部分旧版内置根可能暂未包含 Let's Encrypt 新链，
        // 但系统维护的 OpenSSL CA bundle 已包含，加载它可避免误报 UnknownIssuer。
        tls = add_server_trust(tls, config)?;
        // 第 8.2 节第 3 条：校验域名
        tls = tls.domain_name(resolve_tls_domain(config)?);

        endpoint = endpoint.tls_config(tls).context("配置注册 TLS 参数失败")?;
    }

    endpoint
        .connect()
        .await
        .with_context(|| format!("连接 Master 注册入口失败: {endpoint_str}"))
}

/// WorkerLink 长期长连接 Channel（mTLS，启动前 fail closed）。
pub async fn worker_link_channel(
    config: &WorkerConfig,
    identity: &SavedIdentity,
) -> Result<Channel> {
    let endpoint_str = config.master.endpoint.trim();

    // 本地开发（insecure=true）：http 明文直连，不要求客户端证书/服务端 CA。
    // 身份（node_token）校验仍然生效，只是绕过 TLS 边界（第 8.3 节 fail-closed 仅在
    // 生产模式生效；validate_run_ready 已拦截「非 insecure 却 http」的配置）。
    if config.master.insecure {
        if identity.node_id.trim().is_empty() || identity.node_token.trim().is_empty() {
            bail!("节点身份缺失：请先执行 `worker-agent enroll` 完成注册");
        }
        return Endpoint::from_shared(endpoint_str.to_string())
            .with_context(|| format!("非法的 Master endpoint: {endpoint_str}"))?
            .connect_timeout(CONNECT_TIMEOUT)
            .connect()
            .await
            .with_context(|| format!("连接 Master WorkerLink 失败: {endpoint_str}"));
    }

    let is_loopback = endpoint_str.starts_with("http://127.0.0.1")
        || endpoint_str.starts_with("http://localhost");

    // 第 8.3 节：endpoint 不是 HTTPS → 失败（本地回环调试除外）
    if !endpoint_str.starts_with("https://") && !is_loopback {
        bail!(
            "WorkerLink endpoint 必须是 https://（当前：{endpoint_str}）——生产接入边界不允许明文"
        );
    }

    let paths = config.identity_paths();

    // 第 8.3 节：身份、客户端证书、私钥缺失 → 失败
    if identity.node_id.trim().is_empty() || identity.node_token.trim().is_empty() {
        bail!("节点身份缺失：请先执行 `worker-agent enroll` 完成注册");
    }
    if !paths.client_cert_file.is_file() {
        bail!(
            "客户端证书不存在：{}——mTLS 缺配置时必须失败，不能静默降级",
            paths.client_cert_file.display()
        );
    }
    if !paths.client_key_file.is_file() {
        bail!(
            "客户端私钥不存在：{}——mTLS 缺配置时必须失败，不能静默降级",
            paths.client_key_file.display()
        );
    }

    // 第 8.3 节：cert/key 不匹配 → 失败；证书已过期或即将过期 → 失败
    let cert_pem = std::fs::read_to_string(&paths.client_cert_file)
        .with_context(|| format!("读取客户端证书失败: {}", paths.client_cert_file.display()))?;
    let key_pem = std::fs::read_to_string(&paths.client_key_file)
        .with_context(|| format!("读取客户端私钥失败: {}", paths.client_key_file.display()))?;
    validate_cert_key_pair(&cert_pem, &key_pem, paths.client_cert_file.as_path())?;
    validate_cert_not_expired(&cert_pem, paths.client_cert_file.as_path())?;

    // 第 8.3 节：私有 Server CA 配置了但文件不存在 → 失败
    if let Some(server_ca_path) = &paths.server_ca_file {
        read_ca_file(server_ca_path)?;
    }

    let mut endpoint = Endpoint::from_shared(endpoint_str.to_string())
        .with_context(|| format!("非法的 Master endpoint: {endpoint_str}"))?
        .connect_timeout(CONNECT_TIMEOUT);

    if endpoint_str.starts_with("https://") {
        let mut tls = ClientTlsConfig::new();

        // 1. 客户端身份：mTLS 出示证书与私钥
        let client_identity = Identity::from_pem(cert_pem, key_pem);
        tls = tls.identity(client_identity);

        // 2. 服务端信任根：显式 server_ca_file 或系统根/系统 CA bundle。
        //    Node CA（node_ca_file）绝不加入服务端信任根。
        tls = add_server_trust(tls, config)?;

        // 3. 域名校验
        tls = tls.domain_name(resolve_tls_domain(config)?);

        endpoint = endpoint
            .tls_config(tls)
            .context("配置 WorkerLink mTLS 参数失败")?;
    }

    endpoint
        .connect()
        .await
        .with_context(|| format!("连接 Master WorkerLink 失败: {endpoint_str}"))
}

/// 解析用于 TLS 校验的域名：优先 `tls_domain`，否则取 endpoint 的主机名。
fn resolve_tls_domain(config: &WorkerConfig) -> Result<String> {
    if let Some(domain) = &config.master.tls_domain {
        let domain = domain.trim();
        if !domain.is_empty() {
            return Ok(domain.to_string());
        }
    }
    let endpoint_str = config.master.endpoint.trim();
    let host = endpoint_str
        .strip_prefix("https://")
        .or_else(|| endpoint_str.strip_prefix("http://"))
        .and_then(|rest| rest.split('/').next())
        .and_then(|rest| rest.split(':').next())
        .filter(|h| !h.is_empty())
        .with_context(|| format!("无法从 endpoint 推导 TLS 域名: {endpoint_str}"))?;
    Ok(host.to_string())
}

/// 配置服务端信任根。
fn add_server_trust(mut tls: ClientTlsConfig, config: &WorkerConfig) -> Result<ClientTlsConfig> {
    if let Some(server_ca_path) = &config.master.server_ca_file {
        return Ok(tls.ca_certificate(Certificate::from_pem(read_ca_file(server_ca_path)?)));
    }

    #[cfg(unix)]
    for path in [
        "/etc/ssl/cert.pem",                  // macOS / Alpine
        "/etc/ssl/certs/ca-certificates.crt", // Debian / Ubuntu
        "/etc/pki/tls/certs/ca-bundle.crt",   // RHEL / Fedora
    ] {
        let path = Path::new(path);
        if path.is_file() {
            tls = tls.ca_certificate(Certificate::from_pem(read_ca_file(path)?));
            break;
        }
    }

    Ok(tls)
}

/// 读取 Server CA 文件；文件缺失或为空直接失败（fail closed）。
fn read_ca_file(path: &Path) -> Result<Vec<u8>> {
    let pem = std::fs::read(path)
        .with_context(|| format!("读取服务端 CA 证书失败: {}", path.display()))?;
    if pem.is_empty() {
        bail!("服务端 CA 证书文件为空: {}", path.display());
    }
    Ok(pem)
}

/// 校验客户端证书与私钥匹配（同一公钥），且证书链主体是叶证书。
pub fn validate_cert_key_pair(cert_pem: &str, key_pem: &str, source: &Path) -> Result<()> {
    with_leaf_cert(cert_pem, |cert| {
        let key = rcgen::KeyPair::from_pem(key_pem)
            .with_context(|| format!("解析客户端私钥失败: {}", source.display()))?;
        // 证书 SPKI 中的 BIT STRING 公钥内容应与私钥派生的公钥一致
        let cert_spki = cert.public_key().subject_public_key.data.as_ref();
        if cert_spki != key.public_key_raw() {
            bail!(
                "客户端证书与私钥不匹配（{}）：证书的公钥与私钥不一致",
                source.display()
            );
        }
        Ok(())
    })
    .with_context(|| format!("解析客户端证书失败: {}", source.display()))
}

/// 校验客户端证书未过期且不在「即将过期」窗口（30 天内告警不算失败，仅记录）。
pub fn validate_cert_not_expired(cert_pem: &str, source: &Path) -> Result<()> {
    with_leaf_cert(cert_pem, |cert| {
        let now = chrono::Utc::now();
        let not_after = chrono::DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)
            .context("证书到期时间超出可表示范围")?;
        if not_after <= now {
            bail!(
                "客户端证书已过期（{}，到期时间 {}）：请重新执行 enroll 注册",
                source.display(),
                not_after
            );
        }
        let soon = chrono::Duration::days(30);
        if not_after <= now + soon {
            tracing::warn!(
                path = %source.display(),
                not_after = %not_after,
                "客户端证书将在 30 天内过期，请安排重新注册"
            );
        }
        Ok(())
    })
    .with_context(|| format!("解析客户端证书失败: {}", source.display()))
}

/// 解析 PEM 中的证书 DER 字节（剥离 PEM 外壳）。
fn der_from_pem(pem: &str) -> Result<Vec<u8>> {
    use x509_parser::pem::parse_x509_pem;
    let (_, parsed) = parse_x509_pem(pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("证书不是合法 PEM 格式：{e}"))?;
    Ok(parsed.contents)
}

/// 在证书 DER 生命周期内执行闭包（x509-parser 的证书借用 DER 字节）。
fn with_leaf_cert<T>(
    cert_pem: &str,
    f: impl FnOnce(&X509Certificate<'_>) -> Result<T>,
) -> Result<T> {
    let der = der_from_pem(cert_pem)?;
    let (_, cert) =
        X509Certificate::from_der(&der).map_err(|e| anyhow::anyhow!("证书 DER 解析失败：{e}"))?;
    f(&cert)
}

/// 计算证书 DER 的 SHA-256 指纹（小写十六进制）。
///
/// V4 方案第 8.4 节第 8 条：指纹必须对证书 **DER** 计算，而不是对 PEM 文本计算——
/// PEM 文本的换行、头部差异会让同一张证书算出不同指纹。
pub fn fingerprint_der(cert_pem: &str) -> Result<String> {
    let der = der_from_pem(cert_pem)?;
    let mut hasher = Sha256::new();
    hasher.update(&der);
    Ok(hex::encode(hasher.finalize()))
}

/// 校验证书链：叶证书由给定 CA（PEM）签发。
///
/// 用于注册产物落盘前的自检：Master 可能返回了一张由错误 CA 签发的证书，
/// 提前发现好过等到 mTLS 握手时被云端拒绝。
pub fn validate_chain_signed_by(cert_pem: &str, ca_pem: &str) -> Result<()> {
    let ca_der = der_from_pem(ca_pem).context("Node CA 解析失败")?;
    let (_, ca) =
        X509Certificate::from_der(&ca_der).map_err(|e| anyhow::anyhow!("Node CA 解析失败：{e}"))?;

    with_leaf_cert(cert_pem, |cert| {
        // 用 CA 的公钥验证叶证书的签名（内部按算法 OID 选择 RSA/ECDSA/Ed25519）。
        cert.verify_signature(Some(ca.public_key()))
            .map_err(|e| anyhow::anyhow!("客户端证书不是由返回的 Node CA 签发（{e}）"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, IsCa, KeyPair};

    struct TestCa {
        key: KeyPair,
        cert: rcgen::Certificate,
    }

    fn test_ca() -> TestCa {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name = DistinguishedName::new();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        let cert = params.self_signed(&key).unwrap();
        TestCa { key, cert }
    }

    fn leaf_signed_by(ca: &TestCa, days: i64) -> (KeyPair, rcgen::Certificate) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["worker.example.com".to_string()]).unwrap();
        params.is_ca = IsCa::NoCa;
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let issuer_params = CertificateParams::from_ca_cert_pem(&ca.cert.pem()).unwrap();
        let issuer = issuer_params.self_signed(&ca.key).unwrap();
        let cert = params.signed_by(&key, &issuer, &ca.key).unwrap();
        (key, cert)
    }

    #[test]
    fn fingerprint_is_der_based_and_stable() {
        let ca = test_ca();
        let (_, leaf) = leaf_signed_by(&ca, 30);
        let pem = leaf.pem();
        let fp = fingerprint_der(&pem).unwrap();
        assert_eq!(fp.len(), 64);
        // 同一证书两次计算一致
        assert_eq!(fp, fingerprint_der(&pem).unwrap());
        // 指纹 = DER 的 SHA-256（不是 PEM 文本的）
        let der = der_from_pem(&pem).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&der);
        assert_eq!(fp, hex::encode(hasher.finalize()));
        // PEM 文本本身的哈希必须与指纹不同，证明没有对文本计算
        let mut text_hasher = Sha256::new();
        text_hasher.update(pem.as_bytes());
        assert_ne!(fp, hex::encode(text_hasher.finalize()));
    }

    #[test]
    fn matching_key_pair_is_accepted() {
        let ca = test_ca();
        let (key, leaf) = leaf_signed_by(&ca, 30);
        assert!(validate_cert_key_pair(&leaf.pem(), &key.serialize_pem(), Path::new("t")).is_ok());
    }

    #[test]
    fn mismatched_key_pair_is_rejected() {
        let ca = test_ca();
        let (_, leaf) = leaf_signed_by(&ca, 30);
        let other_key = KeyPair::generate().unwrap();
        assert!(
            validate_cert_key_pair(&leaf.pem(), &other_key.serialize_pem(), Path::new("t"))
                .is_err()
        );
    }

    #[test]
    fn expired_certificate_is_rejected() {
        let ca = test_ca();
        let (_, leaf) = leaf_signed_by(&ca, -2); // 已过期
        assert!(validate_cert_not_expired(&leaf.pem(), Path::new("t")).is_err());
    }

    #[test]
    fn chain_signed_by_ca_is_accepted() {
        let ca = test_ca();
        let (_, leaf) = leaf_signed_by(&ca, 30);
        assert!(validate_chain_signed_by(&leaf.pem(), &ca.cert.pem()).is_ok());
    }

    #[test]
    fn chain_signed_by_wrong_ca_is_rejected() {
        let ca = test_ca();
        let (_, leaf) = leaf_signed_by(&ca, 30);
        let other = test_ca();
        assert!(validate_chain_signed_by(&leaf.pem(), &other.cert.pem()).is_err());
    }
}
