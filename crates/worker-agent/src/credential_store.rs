//! 本地凭据管理与原子落盘（V7 实施方案第 7 节）。
//!
//! 最终本地文件模型（第 7.1 节）：
//! ```text
//! data/
//! ├── identity.json
//! ├── client.key (0600)
//! └── client.crt
//! ```
//!
//! 不再保存 node_token、registration_session、challenge 或 node_ca.crt。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::WorkerIdentityPaths;
use crate::master_port::{ClientCredential, ConnectError};

/// 本地身份元数据（identity.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityJson {
    /// 架构版本（V7 为 2）。
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 唯一安装标识。
    pub installation_id: String,
    /// 节点编号（批准或注册后获得）。
    #[serde(default)]
    pub node_id: Option<String>,
}

fn default_schema_version() -> u32 {
    2
}

/// 本地凭据状态。
#[derive(Debug, Clone)]
pub enum LocalCredentialState {
    /// 全新环境：无私钥与身份文件。
    Uninitialized,
    /// 待审批/未签发阶段：已有私钥与安装标识，但缺少有效证书。
    PendingRegistration {
        /// 安装标识。
        installation_id: String,
        /// 节点编号（若已知）。
        node_id: Option<String>,
        /// 私钥 PEM。
        key_pem: String,
        /// CSR PEM。
        csr_pem: String,
    },
    /// 已就绪：拥有匹配有效的私钥与证书。
    Ready {
        /// 正式链路凭据。
        credential: ClientCredential,
    },
}

/// 本地凭据存储契约。
pub trait CredentialStore: Send + Sync {
    /// 读取并推导当前本地凭据状态。
    fn load_state(&self) -> Result<LocalCredentialState, ConnectError>;

    /// 生成全新私钥和安装标识并落盘。
    fn initialize_fresh(&self) -> Result<LocalCredentialState, ConnectError>;

    /// 原子保存已批准领取的证书并更新 identity.json。
    fn save_approved_certificate(&self, cert_pem: &str, node_id: &str) -> Result<(), ConnectError>;

    /// 使用本地私钥对规范摘要进行签名（私钥持有证明）。
    fn sign_proof(&self, message: &str) -> Result<String, ConnectError>;

    /// 获取 CSR PEM。
    fn csr_pem(&self) -> Result<String, ConnectError>;
}

/// 基于真实文件系统的凭据存储实现。
#[derive(Debug, Clone)]
pub struct FsCredentialStore {
    paths: WorkerIdentityPaths,
}

impl FsCredentialStore {
    /// 新建文件系统凭据存储。
    pub fn new(paths: WorkerIdentityPaths) -> Self {
        Self { paths }
    }

    fn read_key_pem(&self) -> Result<Option<String>, ConnectError> {
        if !self.paths.client_key_file.exists() {
            return Ok(None);
        }
        let pem = std::fs::read_to_string(&self.paths.client_key_file)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("读取私钥失败：{e}")))?;
        Ok(Some(pem))
    }

    fn read_cert_pem(&self) -> Result<Option<String>, ConnectError> {
        if !self.paths.client_cert_file.exists() {
            return Ok(None);
        }
        let pem = std::fs::read_to_string(&self.paths.client_cert_file)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("读取证书失败：{e}")))?;
        Ok(Some(pem))
    }

    fn read_identity_json(&self) -> Result<Option<IdentityJson>, ConnectError> {
        if !self.paths.identity_file.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&self.paths.identity_file)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("读取身份文件失败：{e}")))?;
        let json: IdentityJson = serde_json::from_str(&content)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("解析身份文件 JSON 失败：{e}")))?;
        Ok(Some(json))
    }

    fn generate_csr_from_key(key: &KeyPair) -> Result<String, ConnectError> {
        let mut params = CertificateParams::new(Vec::<String>::new())
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("构造 CSR 参数失败：{e}")))?;
        params.distinguished_name = DistinguishedName::new();
        let csr = params
            .serialize_request(key)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("序列化 CSR 失败：{e}")))?;
        csr.pem()
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("生成 CSR PEM 失败：{e}")))
    }
}

impl CredentialStore for FsCredentialStore {
    fn load_state(&self) -> Result<LocalCredentialState, ConnectError> {
        let key_pem = self.read_key_pem()?;
        let cert_pem = self.read_cert_pem()?;
        let identity = self.read_identity_json()?;

        match (key_pem, cert_pem, identity) {
            (None, None, None) => Ok(LocalCredentialState::Uninitialized),
            (Some(key_pem), None, Some(id)) => {
                let key_pair = KeyPair::from_pem(&key_pem).map_err(|e| {
                    ConnectError::LocalCredentialCorrupt(format!("解析本地私钥失败：{e}"))
                })?;
                let csr_pem = Self::generate_csr_from_key(&key_pair)?;
                Ok(LocalCredentialState::PendingRegistration {
                    installation_id: id.installation_id,
                    node_id: id.node_id,
                    key_pem,
                    csr_pem,
                })
            }
            (Some(key_pem), Some(cert_pem), Some(id)) => {
                // 校验 key 与 cert 是否匹配且未过期
                let _key_pair = KeyPair::from_pem(&key_pem).map_err(|e| {
                    ConnectError::LocalCredentialCorrupt(format!("私钥损坏：{e}"))
                })?;
                if let Err(e) = crate::tls::validate_client_pair(&self.paths.client_key_file, &self.paths.client_cert_file) {
                    return Err(ConnectError::LocalCredentialCorrupt(format!("本地证书与私钥不匹配：{e}")));
                }
                let node_id = id.node_id.unwrap_or_default();
                Ok(LocalCredentialState::Ready {
                    credential: ClientCredential {
                        node_id,
                        installation_id: id.installation_id,
                        client_key_pem: key_pem,
                        client_cert_pem: cert_pem,
                    },
                })
            }
            (None, Some(_), _) => Err(ConnectError::LocalCredentialCorrupt(
                "发现证书文件但私钥缺失，禁止使用不完整身份".to_string(),
            )),
            (Some(_), _, None) => Err(ConnectError::LocalCredentialCorrupt(
                "发现本地私钥但缺少 identity.json，无法确认节点身份".to_string(),
            )),
            (None, None, Some(_)) => Err(ConnectError::LocalCredentialCorrupt(
                "发现 identity.json 但私钥缺失".to_string(),
            )),
        }
    }

    fn initialize_fresh(&self) -> Result<LocalCredentialState, ConnectError> {
        let key_pair = KeyPair::generate()
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("生成私钥失败：{e}")))?;
        let key_pem = key_pair.serialize_pem();
        let csr_pem = Self::generate_csr_from_key(&key_pair)?;
        let installation_id = Uuid::new_v4().to_string();

        if let Some(parent) = self.paths.client_key_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("创建数据目录失败：{e}")))?;
        }

        // 写入私钥文件（0600）
        write_private_key_file(&self.paths.client_key_file, &key_pem)?;

        // 写入 identity.json
        let id_json = IdentityJson {
            schema_version: 2,
            installation_id: installation_id.clone(),
            node_id: None,
        };
        let id_content = serde_json::to_string_pretty(&id_json)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("序列化 identity.json 失败：{e}")))?;
        std::fs::write(&self.paths.identity_file, id_content)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("写入 identity.json 失败：{e}")))?;

        Ok(LocalCredentialState::PendingRegistration {
            installation_id,
            node_id: None,
            key_pem,
            csr_pem,
        })
    }

    fn save_approved_certificate(&self, cert_pem: &str, node_id: &str) -> Result<(), ConnectError> {
        let key_pem = self.read_key_pem()?.ok_or_else(|| {
            ConnectError::LocalCredentialCorrupt("无法保存证书：本地私钥不存在".to_string())
        })?;

        // 1. 验证 key 与 cert 匹配
        let _key_pair = KeyPair::from_pem(&key_pem).map_err(|e| {
            ConnectError::LocalCredentialCorrupt(format!("私钥解析失败：{e}"))
        })?;

        // 2. 原子落盘证书：写入临时文件 -> fsync -> rename -> fsync 目录
        let cert_path = &self.paths.client_cert_file;
        let parent_dir = cert_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent_dir)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("创建证书目录失败：{e}")))?;

        let tmp_path = parent_dir.join(format!(
            ".client.crt.{}.tmp",
            Uuid::new_v4().simple()
        ));

        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp_path)
                .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("创建临时证书文件失败：{e}")))?;
            file.write_all(cert_pem.as_bytes())
                .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("写入临时证书失败：{e}")))?;
            file.sync_all()
                .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("fsync 证书临时文件失败：{e}")))?;
        }

        std::fs::rename(&tmp_path, cert_path)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("原子重命名证书文件失败：{e}")))?;

        // fsync 目录
        if let Ok(dir_file) = File::open(parent_dir) {
            dir_file.sync_all().ok();
        }

        // 3. 更新 identity.json
        let mut id_json = self.read_identity_json()?.unwrap_or(IdentityJson {
            schema_version: 2,
            installation_id: Uuid::new_v4().to_string(),
            node_id: Some(node_id.to_string()),
        });
        id_json.node_id = Some(node_id.to_string());
        id_json.schema_version = 2;

        let content = serde_json::to_string_pretty(&id_json)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("序列化 identity.json 失败：{e}")))?;
        std::fs::write(&self.paths.identity_file, content)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("写入 identity.json 失败：{e}")))?;

        tracing::info!(
            node_id = %node_id,
            cert_file = %cert_path.display(),
            "证书已成功原子落盘"
        );

        Ok(())
    }

    fn sign_proof(&self, message: &str) -> Result<String, ConnectError> {
        let key_pem = self.read_key_pem()?.ok_or_else(|| {
            ConnectError::LocalCredentialCorrupt("本地私钥缺失，无法签名证明".to_string())
        })?;
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("私钥解析失败：{e}")))?;
        let der = key.serialize_der();
        let rng = ring::rand::SystemRandom::new();
        let signing_key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &der, &rng)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("构造签名器失败：{e:?}")))?;
        let signature = signing_key
            .sign(&rng, message.as_bytes())
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("签名失败：{e:?}")))?;
        Ok(hex::encode(signature.as_ref()))
    }

    fn csr_pem(&self) -> Result<String, ConnectError> {
        let key_pem = self.read_key_pem()?.ok_or_else(|| {
            ConnectError::LocalCredentialCorrupt("本地私钥缺失，无法生成 CSR".to_string())
        })?;
        let key_pair = KeyPair::from_pem(&key_pem)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("私钥解析失败：{e}")))?;
        Self::generate_csr_from_key(&key_pair)
    }
}

/// 写入私钥文件（Unix 设置 0600 权限）。
fn write_private_key_file(path: &Path, content: &str) -> Result<(), ConnectError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("打开私钥文件失败：{e}")))?;
        file.write_all(content.as_bytes())
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("写入私钥文件失败：{e}")))?;
        file.sync_all()
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("fsync 私钥文件失败：{e}")))?;
    }

    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("打开私钥文件失败：{e}")))?;
        file.write_all(content.as_bytes())
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("写入私钥文件失败：{e}")))?;
        file.sync_all()
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("fsync 私钥文件失败：{e}")))?;
    }

    Ok(())
}

/// 内存凭据存储实现（用于状态机测试与模拟运行）。
#[derive(Debug, Clone)]
pub struct InMemoryCredentialStore {
    state: Arc<Mutex<Option<InMemoryState>>>,
}

#[derive(Debug, Clone)]
struct InMemoryState {
    installation_id: String,
    node_id: Option<String>,
    key_pem: String,
    csr_pem: String,
    cert_pem: Option<String>,
}

impl InMemoryCredentialStore {
    /// 新建空的内存凭据存储。
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// 预置有效就绪状态。
    pub fn with_ready(installation_id: &str, node_id: &str, key_pem: &str, cert_pem: &str) -> Self {
        let key_pair = KeyPair::from_pem(key_pem).unwrap();
        let csr_pem = FsCredentialStore::generate_csr_from_key(&key_pair).unwrap();
        Self {
            state: Arc::new(Mutex::new(Some(InMemoryState {
                installation_id: installation_id.to_string(),
                node_id: Some(node_id.to_string()),
                key_pem: key_pem.to_string(),
                csr_pem,
                cert_pem: Some(cert_pem.to_string()),
            }))),
        }
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn load_state(&self) -> Result<LocalCredentialState, ConnectError> {
        let guard = self.state.lock().unwrap();
        match &*guard {
            None => Ok(LocalCredentialState::Uninitialized),
            Some(s) => match &s.cert_pem {
                Some(cert_pem) => Ok(LocalCredentialState::Ready {
                    credential: ClientCredential {
                        node_id: s.node_id.clone().unwrap_or_default(),
                        installation_id: s.installation_id.clone(),
                        client_key_pem: s.key_pem.clone(),
                        client_cert_pem: cert_pem.clone(),
                    },
                }),
                None => Ok(LocalCredentialState::PendingRegistration {
                    installation_id: s.installation_id.clone(),
                    node_id: s.node_id.clone(),
                    key_pem: s.key_pem.clone(),
                    csr_pem: s.csr_pem.clone(),
                }),
            },
        }
    }

    fn initialize_fresh(&self) -> Result<LocalCredentialState, ConnectError> {
        let key_pair = KeyPair::generate()
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("生成私钥失败：{e}")))?;
        let key_pem = key_pair.serialize_pem();
        let csr_pem = FsCredentialStore::generate_csr_from_key(&key_pair)?;
        let installation_id = Uuid::new_v4().to_string();

        let mut guard = self.state.lock().unwrap();
        *guard = Some(InMemoryState {
            installation_id: installation_id.clone(),
            node_id: None,
            key_pem: key_pem.clone(),
            csr_pem: csr_pem.clone(),
            cert_pem: None,
        });

        Ok(LocalCredentialState::PendingRegistration {
            installation_id,
            node_id: None,
            key_pem,
            csr_pem,
        })
    }

    fn save_approved_certificate(&self, cert_pem: &str, node_id: &str) -> Result<(), ConnectError> {
        let mut guard = self.state.lock().unwrap();
        if let Some(ref mut s) = *guard {
            s.cert_pem = Some(cert_pem.to_string());
            s.node_id = Some(node_id.to_string());
            Ok(())
        } else {
            Err(ConnectError::LocalCredentialCorrupt("无本地私钥记录".to_string()))
        }
    }

    fn sign_proof(&self, message: &str) -> Result<String, ConnectError> {
        let guard = self.state.lock().unwrap();
        let s = guard.as_ref().ok_or_else(|| {
            ConnectError::LocalCredentialCorrupt("本地私钥缺失，无法签名".to_string())
        })?;
        let key = KeyPair::from_pem(&s.key_pem)
            .map_err(|e| ConnectError::LocalCredentialCorrupt(format!("私钥解析失败：{e}")))?;
        let der = key.serialize_der();
        let rng = ring::rand::SystemRandom::new();
        let signing_key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &der, &rng)
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("构造签名器失败：{e:?}")))?;
        let signature = signing_key
            .sign(&rng, message.as_bytes())
            .map_err(|e| ConnectError::Fatal(anyhow::anyhow!("签名失败：{e:?}")))?;
        Ok(hex::encode(signature.as_ref()))
    }

    fn csr_pem(&self) -> Result<String, ConnectError> {
        let guard = self.state.lock().unwrap();
        let s = guard.as_ref().ok_or_else(|| {
            ConnectError::LocalCredentialCorrupt("本地私钥缺失，无法获取 CSR".to_string())
        })?;
        Ok(s.csr_pem.clone())
    }
}
