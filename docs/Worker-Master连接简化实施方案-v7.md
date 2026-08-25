# Worker–Master 连接简化实施方案 V7

## 1. 文档目的

本文给出 Worker 与 Master 连接链路的简化实施方案。目标不是降低生产安全要求，而是把注册、审批、凭据、TLS、重连和在线状态的复杂度收进少数深模块，使 Worker 调用方、Master 业务处理和部署配置不再共同承担连接协议细节。

本方案在兼容现有 V5 Worker 的前提下分阶段实施，最终将公网 Worker 协议收敛为：

```text
一个公开地址 + 两个 RPC + 一种正式凭据 + 一个 Worker 运行状态机
```

最终 RPC：

```proto
rpc EnsureRegistration(EnsureRegistrationRequest) returns (EnsureRegistrationResponse);
rpc OpenLink(stream WorkerMessage) returns (stream MasterMessage);
```

## 2. 结论摘要

### 2.1 推荐目标

1. 保留一个 Worker 公网域名，由 Caddy 终止 TLS。
2. 用幂等 `EnsureRegistration` 合并 `RegisterNode` 与 `WatchRegistration`。
3. 使用 mTLS 客户端证书作为正式链路的唯一长期身份凭据。
4. 删除 `node_token`、注册会话令牌、一次性领取语义和客户端自报身份元数据。
5. Worker 对外只暴露 `WorkerRuntime::run(config)`，内部管理注册、审批等待、证书落盘、mTLS 连接、对账和退避重连。
6. 将注册状态、连接状态、运行状态分开，审批成功不再被误解为已经在线。
7. 先双栈兼容，再切换新 Worker，最后删除旧字段、旧 RPC 和旧表结构。

### 2.2 不能删除的一次重连

审批前 Worker 没有客户端证书，首次连接只能使用服务端 TLS。审批后客户端证书不能注入已经建立的 TLS 会话，因此 Worker 必须使用新证书重新握手并建立 mTLS `OpenLink`。

所以目标不是强行变成一条物理连接，而是只保留两个清晰阶段，并把阶段切换隐藏在 Worker 运行模块内部：

```text
服务端 TLS 注册阶段 → 审批并取得证书 → mTLS 正式链路阶段
```

如果要求从启动到运行始终只有一条物理连接，只能在以下两项中选择一项：

- 审批前预置客户端证书；
- 放弃审批后 mTLS，改用普通 TLS 上的令牌鉴权。

这两项都偏离当前“审批后才建立设备信任”的目标，本方案不采用。

## 3. 当前实现评估

### 3.1 当前时序

```text
Worker                                             Caddy                  Master
  │                                                   │                      │
  │── TLS / RegisterNode ────────────────────────────>│── h2c ─────────────>│
  │<─ node_id + session + challenge ────────────────────────────────────────│
  │                                                   │                      │
  │── 每 15 秒建立 Channel / WatchRegistration ─────>│─────────────────────>│
  │<─ 待审核 ────────────────────────────────────────────────────────────────│
  │                     管理员批准、签证书、生成 node_token                 │
  │── WatchRegistration ─────────────────────────────>│─────────────────────>│
  │<─ cert + Node CA + 一次性 node_token ───────────────────────────────────│
  │                                                   │                      │
  │── mTLS / OpenLink + node_id + token + fingerprint│─────────────────────>│
  │<════════════════════ 双向长连接、对账、心跳、任务 ═══════════════════════>│
```

### 3.2 主要复杂度来源

#### 协议表面过大

当前同时存在：

- `Enroll`：旧注册码兼容链路；
- `RegisterNode`：提交直连注册；
- `WatchRegistration`：查询审批状态；
- `OpenLink`：正式双向长连接。

其中 `WatchRegistration` 声明为服务端流，但 Master 只产生一个事件，Worker 每次查询又重新创建 gRPC Channel。它承担的是普通查询行为，却引入了流式接口与连接生命周期。

#### 身份证据重复

`OpenLink` 当前同时携带并校验：

- `x-node-id`；
- `x-node-token`；
- 客户端证书指纹；
- 节点批准、禁用、证书过期和吊销状态。

证书指纹已经可以唯一映射节点；继续要求客户端自报节点编号和第二套长期令牌，增加了存储、轮换、错误分支和部署联动，却没有形成独立的设备身份来源。

#### 一次性交付不可恢复

批准后 Master 将明文 `node_token` 放入注册会话，Worker 首次查询后 Master 立即清空。若响应已经提交而 Worker 尚未完成本地原子落盘就发生进程崩溃、磁盘错误或网络中断，后续查询只能得到“已领取”，必须人工重置身份。

这种行为把网络投递与本地持久化绑定成了无法跨机器提交的事务，是当前最需要删除的脆弱点。

#### 本地状态重复

Worker 当前需要协调：

- `identity.json`；
- `client.key`；
- `client.crt`；
- `node_ca.crt`；
- 注册会话令牌；
- 注册挑战值；
- 节点令牌；
- 本地注册状态。

其中 Node CA 不参与 Worker 对云端服务器证书的校验；云端服务器由系统根或显式 `server_ca_file` 验证。Node CA 仅为 Master/Caddy 验证 Worker 证书所需，不必下发并保存在 Worker。

#### 状态语义混合

“已批准”表示允许建立正式链路，不表示 Worker 已经建立连接。当前审批事务会把节点运行状态写成“离线”，只有 `OpenLink` 建立后才置为已连接，因此页面出现“批准后仍离线”其实是状态语义没有清楚呈现。

## 4. 目标架构

### 4.1 总体时序

```text
Worker                                             Caddy                  Master
  │                                                   │                      │
  │── TLS / EnsureRegistration(CSR + proof) ─────────>│── h2c ─────────────>│
  │<─ PENDING + retry_after ────────────────────────────────────────────────│
  │                                                   │                      │
  │── 复用 Channel / EnsureRegistration ─────────────>│─────────────────────>│
  │<─ APPROVED + client_certificate ────────────────────────────────────────│
  │── 原子保存 client.crt                              │                      │
  │                                                   │                      │
  │── mTLS / OpenLink ────────────────────────────────>│── fingerprint ─────>│
  │<════════════════════ 双向长连接、对账、心跳、任务 ═══════════════════════>│
```

### 4.2 身份来源

正式链路只接受一个权威身份来源：

```text
Caddy 实际收到的客户端证书
        ↓ SHA-256 fingerprint
Master 查询 node_certificates
        ↓
得到 node_id，并校验批准、禁用、过期、吊销状态
```

Worker 消息中的 `node_id` 在兼容阶段可以继续存在，但 Master 不得把它作为身份来源。Master 应使用已经鉴权得到的 `node_id`，并在发现消息内节点编号不一致时拒绝消息或记录安全审计。

### 4.3 Worker 深模块

Worker 连接逻辑收口到 `WorkerRuntime` 模块。它的外部接口只有：

```rust
pub struct WorkerRuntime<P, S> {
    master: P,
    credentials: S,
}

impl<P: MasterPort, S: CredentialStore> WorkerRuntime<P, S> {
    pub async fn run(self, config: WorkerConfig) -> anyhow::Result<()>;
}
```

外部调用方不再依次调用“加载身份、注册、创建 TLS Channel、打开长连接”。这些顺序约束全部属于 `WorkerRuntime` 的实现。

远端 Master 是自有远端依赖，在内部 seam 定义 `MasterPort`：

```rust
#[async_trait]
pub trait MasterPort: Send + Sync {
    async fn ensure_registration(
        &self,
        request: EnsureRegistration,
    ) -> Result<RegistrationOutcome, ConnectError>;

    async fn open_link(
        &self,
        credential: &ClientCredential,
    ) -> Result<MasterLink, ConnectError>;
}
```

提供两个 adapter：

- `TonicMasterAdapter`：生产 gRPC/TLS 实现；
- `InMemoryMasterAdapter`：运行状态机测试实现。

凭据存储是本地可替换依赖，在内部 seam 定义 `CredentialStore`，生产使用文件系统 adapter，测试使用内存或临时目录 adapter。

### 4.4 Worker 状态机

```text
┌──────────────┐
│ LoadingLocal │
└──────┬───────┘
       │
       ├── 有匹配且有效的 key + cert ───────────────┐
       │                                             │
       └── 无 cert ──> EnsuringRegistration          │
                            │                         │
                 ┌──────────┼───────────┐             │
                 │          │           │             │
              Pending    Rejected    Approved         │
                 │          │           │             │
                 │      停止并报告       └─ 原子保存证书┘
                 │
                 └─ 按 retry_after 等待
                                                ↓
                                          ConnectingMtls
                                                ↓
                                           Reconciling
                                                ↓
                                             Online
                                                ↓
                                     Disconnected / Backoff
                                                └──> ConnectingMtls
```

状态机必须满足：

- 正常待审批不计入故障退避；
- 网络故障使用指数退避并带随机抖动；
- 稳定在线一段时间后重置退避；
- 证书被吊销、过期或不属于节点时停止盲目重试并输出可操作原因；
- 断线后始终先对账，再恢复申请新任务；
- 不因身份文件部分损坏而自动生成新身份绕过审批。

## 5. 新协议设计

### 5.1 WorkerLink

```proto
service WorkerLink {
  rpc EnsureRegistration(EnsureRegistrationRequest)
      returns (EnsureRegistrationResponse);

  rpc OpenLink(stream WorkerMessage)
      returns (stream MasterMessage);
}
```

兼容期间保留旧 RPC，但标记弃用：

```proto
rpc Enroll(EnrollRequest) returns (EnrollResponse) {
  option deprecated = true;
}
rpc RegisterNode(RegisterNodeRequest) returns (RegisterNodeResponse) {
  option deprecated = true;
}
rpc WatchRegistration(WatchRegistrationRequest)
    returns (stream RegistrationEvent) {
  option deprecated = true;
}
```

### 5.2 EnsureRegistration 请求

```proto
message EnsureRegistrationRequest {
  uint32 protocol_version = 1;
  string installation_id = 2;
  NodeProfile profile = 3;
  string csr_pem = 4;
  string request_nonce = 5;
  string requested_at = 6;
  string proof_signature = 7;
  uint32 wait_seconds = 8;
}

message NodeProfile {
  string node_name = 1;
  string os_type = 2;
  string os_version = 3;
  string agent_version = 4;
  uint32 requested_slots = 5;
}
```

约束：

- `protocol_version` 首版为 `1`；
- `installation_id` 为首次安装生成并持久化的 UUID；
- Master 从 CSR 计算公钥指纹，不再接受客户端重复声明的指纹字段；
- `proof_signature` 使用 CSR 对应私钥对规范化请求摘要签名；
- 签名摘要必须包含协议版本、安装标识、CSR SHA-256、请求随机数和请求时间；
- `requested_at` 与服务器时间偏差默认不得超过 5 分钟；
- `request_nonce` 在短时间窗口内去重；
- `wait_seconds` 限制在 `0..30`，用于可选长轮询；
- 同一安装标识和同一公钥的重复请求必须幂等；
- 同一安装标识出现不同公钥仍视为身份异常，不能自动替换。

### 5.3 EnsureRegistration 响应

```proto
enum RegistrationState {
  REGISTRATION_STATE_UNSPECIFIED = 0;
  REGISTRATION_STATE_PENDING = 1;
  REGISTRATION_STATE_APPROVED = 2;
  REGISTRATION_STATE_REJECTED = 3;
  REGISTRATION_STATE_EXPIRED = 4;
}

message EnsureRegistrationResponse {
  string node_id = 1;
  RegistrationState state = 2;
  uint32 approved_slots = 3;
  string client_certificate_pem = 4;
  string rejection_reason = 5;
  uint32 retry_after_seconds = 6;
  string registration_expires_at = 7;
}
```

不再返回：

- `registration_session`；
- `challenge`；
- `node_token`；
- `ca_certificate_pem`。

### 5.4 幂等证书领取

客户端证书是公开材料，只有持有本地私钥的 Worker 才能用它完成 mTLS。因此，对相同安装标识、相同 CSR 公钥、已批准节点重复返回同一张有效证书，不会泄露可用的设备身份。

Master 必须：

1. 验证请求签名确由 CSR 私钥持有者产生；
2. 校验安装标识与已绑定公钥一致；
3. 已批准且存在有效证书时重复返回证书；
4. 不在响应提交时改变“已领取”状态；
5. 证书接近过期时，可对同一公钥签发新证书并返回；
6. 保留旧证书到短暂重叠期结束，再吊销或自然过期。

这样即使 Worker 在收到响应后、写盘前崩溃，下次调用也能恢复，不再需要跨网络的一次性提交语义。

### 5.5 OpenLink 鉴权

目标请求元数据只保留非身份信息，例如协议版本。Agent 版本、系统信息和现场信息统一放入首条 `NodeOnline` 消息。

Master 鉴权流程：

```text
读取 Caddy 注入的证书指纹
  → 查询未吊销且未过期的 node_certificates
  → 取得 node_id
  → 查询 worker_nodes
  → 必须已批准且未禁用
  → 建立 LinkIdentity
  → 后续所有消息使用 LinkIdentity.node_id
```

目标删除：

- `METADATA_NODE_ID`；
- `METADATA_NODE_TOKEN`；
- Worker 自报 `METADATA_CLIENT_CERT_FINGERPRINT`；
- `authenticate_node(node_id, token)`；
- `node_token_hash`；
- `pending_node_token`。

本地 `insecure=true` 联调不得伪装成生产 mTLS。测试应通过 `InMemoryMasterAdapter` 或显式开发身份 adapter 完成，避免生产鉴权函数保留“客户端自报指纹”的旁路。

## 6. Caddy 与网络部署

### 6.1 保持单域名

继续使用：

```text
https://worker.<domain>
```

Caddy 保持 `client_auth mode request`，因为同一地址的注册请求没有客户端证书，而 `OpenLink` 必须携带证书。

目标路由：

```caddyfile
@grpc_worker {
    path /platform.worker.v1.WorkerLink/EnsureRegistration* /platform.worker.v1.WorkerLink/OpenLink*
}

handle @grpc_worker {
    reverse_proxy h2c://master:9443 {
        header_up x-client-cert-fingerprint {http.request.tls.client.fingerprint}
    }
}
```

兼容期间继续路由旧 RPC。

### 6.2 信任约束

Master 接收代理指纹头的前提是：

- Master gRPC 端口只在容器私网监听；
- 安全组和 Docker 端口不把 `9443` 暴露到公网；
- 只有 Caddy 能连接 Master gRPC 端口；
- Caddy 始终覆盖外部请求携带的同名头；
- Master 启动时生产配置必须声明可信代理接入模式；
- 部署验收必须验证公网无法绕过 Caddy 直连 Master。

### 6.3 暂不把 TLS 迁入 Master

直接在 Master 终止公网 TLS 可以消除代理指纹头，但会把 ACME、证书热更新、可选客户端证书和 gRPC TLS 配置都移入 Rust 进程，部署实现反而更复杂。本轮先保留 Caddy 作为 TLS adapter，并用自动化验收锁住其行为。

## 7. 本地凭据模型

### 7.1 最终文件

```text
data/
├── identity.json
├── client.key
└── client.crt
```

`identity.json` 最终只保存：

```json
{
  "schema_version": 2,
  "installation_id": "...",
  "node_id": "..."
}
```

删除：

- `node_token`；
- `certificate_fingerprint`，启动时可从证书计算；
- `registration_session`；
- `registration_challenge`；
- 本地 `status`；
- `client_certificate_pem` / `ca_certificate_pem` 进程内过渡字段；
- `node_ca_file` 配置与 `node_ca.crt`。

### 7.2 状态推导

- `client.key` 不存在：生成私钥和安装标识；
- `client.key` 存在、`client.crt` 不存在：调用 `EnsureRegistration`；
- key 与 cert 存在且匹配：直接尝试 mTLS；
- cert 过期或被吊销：停止正式连接，调用受控的续签/恢复逻辑，不生成新私钥；
- identity 与 key 不一致：报告身份异常，禁止静默覆盖。

本地不再持久化“待审核/已批准”，该状态由 Master 响应和证书是否存在共同确定。

### 7.3 原子持久化

证书保存流程：

1. 校验证书公钥与本地私钥匹配；
2. 校验证书有效期；
3. 校验证书由预期 Node CA 签发；
4. 写入同目录临时文件；
5. `fsync` 文件；
6. 原子重命名为 `client.crt`；
7. `fsync` 目录；
8. 重新从最终路径读取并校验；
9. 进入 `ConnectingMtls`。

即使第 4–8 步失败，下一次 `EnsureRegistration` 仍可重新取得证书。

## 8. Master 数据模型调整

### 8.1 保留表

- `worker_nodes`：节点、注册决定、运行配置；
- `node_certificates`：证书指纹、节点归属、有效期和吊销记录；
- 审计日志表；
- 节点配置版本表。

### 8.2 注册请求表

将短期“注册会话”改成当前注册请求记录：

```sql
CREATE TABLE worker_registration_requests (
    node_id UUID PRIMARY KEY REFERENCES worker_nodes(id),
    installation_id UUID NOT NULL,
    csr_pem TEXT NOT NULL,
    public_key_fingerprint TEXT NOT NULL,
    source_ip TEXT,
    requested_slots INTEGER NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
```

不再保存：

- token hash；
- challenge；
- attempt count；
- pending 明文节点令牌；
- “已领取”状态。

限流改由来源 IP、安装标识和全局待审核数量控制。请求 nonce 使用短期缓存或带过期时间的小表去重，不进入长期节点身份模型。

### 8.3 节点表兼容迁移

阶段性增加：

```sql
ALTER TABLE worker_nodes
    ADD COLUMN credential_mode TEXT NOT NULL DEFAULT 'token_and_certificate';
```

允许值：

- `token_and_certificate`：旧 Worker；
- `certificate_only`：新 Worker。

完成全量切换后：

1. 所有已批准且有有效证书的节点改为 `certificate_only`；
2. `node_token_hash` 改为可空；
3. 观察一个发布周期；
4. 删除 `node_token_hash` 和 `credential_mode`。

### 8.4 三类状态分离

Master 对外返回三个正交字段：

```json
{
  "registration_status": "已批准",
  "connection_status": "离线",
  "runtime_status": "未知"
}
```

定义：

- `registration_status`：待审核、已批准、已拒绝、已过期；
- `connection_status`：在线、离线，由活动 `OpenLink`、`connected` 和心跳超时决定；
- `runtime_status`：可用、忙碌、暂停、存储异常、未知，由已连接 Worker 上报。

审批只修改 `registration_status`，不得制造“已经在线”的假象。页面显示文案：

```text
已批准 · 等待 Worker 建立连接
```

## 9. 代码模块调整

### 9.1 worker-agent

新增：

```text
crates/worker-agent/src/runtime.rs
crates/worker-agent/src/master_port.rs
crates/worker-agent/src/credential_store.rs
crates/worker-agent/src/transport/tonic.rs
```

职责：

| 文件 | 职责 |
|---|---|
| `runtime.rs` | 唯一运行入口和连接状态机 |
| `master_port.rs` | Worker 内部 Master 端口及领域结果 |
| `credential_store.rs` | 本地身份加载、校验和原子保存 |
| `transport/tonic.rs` | gRPC、TLS、元数据和消息流 adapter |

改造：

| 当前文件 | 目标 |
|---|---|
| `main.rs` | `run` 只构造依赖并调用 `WorkerRuntime::run` |
| `registration.rs` | 逻辑迁入 runtime/credential store，兼容代码阶段后删除 |
| `tls.rs` | 变成 tonic adapter 的内部实现，不再由 main/client 直接调用 |
| `client.rs` | 保留链路消息循环，改为 runtime 内部实现；不再拥有外层重连决策 |
| `config.rs` | 删除 enroll endpoint、node token、Node CA 和注册会话字段 |

`slot`、`outbox`、`storage` 和任务执行逻辑不进入本次深化；它们只通过现有消息总线与在线链路交互。

### 9.2 master-server

新增或调整：

```text
crates/master-server/src/grpc/ensure_registration.rs
crates/master-server/src/grpc/link_identity.rs
crates/master-server/src/store/registration_request.rs
```

| 模块 | 职责 |
|---|---|
| `ensure_registration` | 幂等注册、签名验证、审批状态、证书返回 |
| `link_identity` | 由可信代理证书指纹解析唯一节点身份 |
| `registration_request` | 当前注册请求的持久化与幂等约束 |

旧 `grpc/registration.rs` 在兼容阶段作为旧协议 adapter，内部调用新的注册领域逻辑，禁止复制两套审批规则。

### 9.3 platform-proto

- 新增 `EnsureRegistration` 及消息；
- 新增 `RegistrationState` 枚举；
- 旧字段编号永久保留，不复用；
- 旧 RPC 标记弃用；
- 后续版本删除旧 RPC 声明时，保留消息字段编号注释和兼容说明。

### 9.4 admin-web

- Worker 列表拆分显示注册、连接、运行三类状态；
- 审批成功提示改为“已批准，等待 Worker 建立连接”；
- 详情页展示最近连接时间、最近心跳时间和证书有效期；
- “在线”只取连接状态，不从注册状态推断；
- SSE 节点变更事件应在审批、连接建立、连接断开和运行状态变化时发布。

## 10. 错误模型

Worker 不再依赖中文错误消息内容决定重试策略。内部错误类型：

```rust
pub enum ConnectError {
    Network { retry_after: Option<Duration> },
    RateLimited { retry_after: Duration },
    PendingApproval { retry_after: Duration },
    Rejected { reason: String },
    IdentityConflict,
    CertificateExpired,
    CertificateRevoked,
    Unauthorized,
    ProtocolMismatch,
    LocalCredentialCorrupt,
    Fatal(anyhow::Error),
}
```

gRPC 状态码与领域错误集中映射：

| gRPC 状态 | 领域错误 | 行为 |
|---|---|---|
| `UNAVAILABLE` | Network | 指数退避 |
| `RESOURCE_EXHAUSTED` | RateLimited | 按 retry-after 等待 |
| 正常响应 PENDING | PendingApproval | 正常等待，不计故障 |
| `PERMISSION_DENIED` + 拒绝详情 | Rejected | 停止注册循环 |
| `ALREADY_EXISTS` / `FAILED_PRECONDITION` | IdentityConflict | 停止并提示人工处理 |
| `UNAUTHENTICATED` | Unauthorized | 检查证书，不盲目新建身份 |
| `UNIMPLEMENTED` | ProtocolMismatch | 兼容期回退旧协议 |

日志可以包含节点编号、阶段、错误类别和重试时间，但不得包含私钥、完整 CSR、令牌或证书正文。

## 11. 分阶段实施

### 阶段 0：建立基线

工作：

- 固化当前生产注册、审批、上线、重连和吊销端到端测试；
- 记录当前协议与数据库基线；
- 确认所有生产 Worker 版本和在线数量；
- 为 Worker 连接、注册结果、鉴权失败和重连增加指标；
- 备份 Caddy 配置、Master 配置、数据库和 CA 材料。

完成条件：当前 V5 链路能够由自动化测试稳定复现，失败原因可观测。

### 阶段 1：先修正状态语义

工作：

- Master 返回注册、连接、运行三类状态；
- 管理页面按三类状态显示；
- 审批提示改为“等待 Worker 建立连接”；
- `OpenLink` 建立/断开成为连接状态唯一实时依据；
- 心跳超时只影响连接/运行状态，不回写注册决定。

兼容：保留原 `status` 字段一个发布周期供旧前端使用。

完成条件：批准后离线不再被页面表现为矛盾或失败。

### 阶段 2：引入 WorkerRuntime，不改变线上协议

工作：

- 新建 `WorkerRuntime`、`MasterPort` 和 `CredentialStore`；
- 将现有注册、TLS、OpenLink 和重连逻辑移入其实现；
- tonic adapter 暂时仍调用旧 V5 RPC；
- 新增 in-memory adapter 测试完整状态机；
- `main.rs` 收敛为单一调用。

完成条件：线上协议行为不变，但调用方不再了解连接顺序；状态机测试覆盖断网、重启、审批和对账。

### 阶段 3：增加 EnsureRegistration 双栈协议

工作：

- protobuf 新增 `EnsureRegistration`；
- Master 实现幂等注册请求表和签名验证；
- 已批准节点可以重复领取同一张有效证书；
- 新 Worker 优先使用新 RPC；
- 收到 `UNIMPLEMENTED` 时兼容回退旧 V5 注册链路；
- Caddy 增加新 RPC 路由；
- 保留旧 Enroll/Register/Watch。

完成条件：全新 Worker、待审批 Worker、审批瞬间断网 Worker、写盘失败后重启 Worker 都能自动恢复。

### 阶段 4：证书唯一身份双栈

工作：

- Master 新增按可信证书指纹解析 `LinkIdentity`；
- `credential_mode=certificate_only` 节点不再要求 node token；
- 新 Worker 不发送 node id、node token 或自报指纹；
- 旧 Worker 继续走 token + certificate；
- Master 对消息内 node id 与链路身份不一致进行拒绝和审计；
- 本地新身份不再保存 node token 与 Node CA。

完成条件：新 Worker 只凭有效 mTLS 证书建立正式链路；吊销、错节点证书、缺证书全部拒绝。

### 阶段 5：全量切换与观察

工作：

- 先升级一台 canary Worker；
- 验证注册、审批、上线、任务、断线重连、Master 重启和证书吊销；
- 分批升级全部 Worker；
- 观察至少一个完整业务周期；
- 统计旧 RPC 调用量和旧 credential mode 节点数；
- 调用量归零后关闭兼容回退。

完成条件：生产不存在旧 Worker，旧 RPC 指标连续一个观察周期为零。

### 阶段 6：删除旧链路

删除：

- `Enroll`、`RegisterNode`、`WatchRegistration`；
- 旧注册码命令与后台存储；
- `node_token_hash`；
- `pending_node_token`；
- 注册会话 token/challenge/attempt 字段；
- Worker `enroll_endpoint`、`node_ca_file`；
- Worker 身份文件中的旧字段；
- Caddy 旧 RPC 路由；
- 只服务旧浅模块的测试。

完成条件：协议只剩两个 RPC，生产正式身份只剩客户端证书。

## 12. 测试方案

### 12.1 WorkerRuntime 接口测试

使用 `InMemoryMasterAdapter` 和内存凭据 adapter，通过 `WorkerRuntime::run` 的可观察结果测试：

- 无身份首次启动提交一次注册；
- 待审批按服务器建议间隔查询；
- 正常待审批不触发指数故障退避；
- 批准后保存证书并切换 mTLS；
- 证书响应后模拟崩溃，重启可再次领取；
- 证书写盘失败不破坏现有身份；
- 已有有效证书直接连接；
- 网络错误指数退避并带抖动；
- 连接稳定后重置退避；
- 重连先对账后取任务；
- 拒绝、身份冲突、吊销不会高频重试；
- 本地 key/cert 不匹配时 fail closed。

新接口测试建立后，删除穿透内部实现、依赖具体函数调用顺序的旧测试。

### 12.2 Master 注册集成测试

- 相同安装标识 + 相同公钥重复请求不创建重复节点；
- 相同安装标识 + 不同公钥拒绝；
- Master 自行计算 CSR 指纹；
- 签名摘要字段被篡改时拒绝；
- 请求时间超窗拒绝；
- nonce 重放不产生副作用；
- 待审批不签发证书；
- 批准后重复请求返回相同有效证书；
- 拒绝、过期、禁用状态不能取得证书；
- 来源 IP、安装标识和全局待审核限流生效；
- 并发批准只签发一次有效证书；
- 证书续签保持节点身份不变。

### 12.3 OpenLink 安全测试

- 无客户端证书拒绝；
- 有效证书映射到正确节点；
- 证书属于其他节点时不能伪造消息内 node id；
- 已吊销证书拒绝；
- 已过期证书拒绝；
- 未批准或已禁用节点拒绝；
- 公网伪造 `x-client-cert-fingerprint` 被 Caddy 覆盖；
- 绕过 Caddy 直连 Master 不可达；
- 本地 insecure 模式不能进入生产配置。

### 12.4 端到端测试

1. 清空 Worker 测试身份目录；
2. 启动 Worker；
3. 确认管理端出现待审批节点；
4. 管理员批准；
5. 确认 Worker 自动取得证书；
6. 确认发生一次预期的 mTLS 重连；
7. 确认连接状态在线；
8. 确认对账完成后才申请任务；
9. 重启 Worker，确认不重复注册；
10. 断开网络再恢复，确认自动重连；
11. 吊销证书，确认现有/后续链路被拒绝；
12. 检查日志无敏感材料。

## 13. 可观测性

### 13.1 指标

建议增加：

```text
worker_registration_requests_total{result}
worker_registration_pending
worker_registration_latency_seconds
worker_link_connections{state}
worker_link_auth_failures_total{reason}
worker_link_reconnects_total{reason}
worker_link_reconcile_seconds
worker_certificate_days_until_expiry
worker_legacy_rpc_calls_total{method}
```

`reason` 只能使用有限枚举，不得把节点编号、错误正文或证书指纹作为指标标签。

### 13.2 日志阶段

Worker 每次状态变化输出一次结构化日志：

```text
loading_local
registration_pending
credential_saved
connecting_mtls
reconciling
online
disconnected
backing_off
stopped_identity_error
```

避免每个心跳周期重复输出相同存储告警；同类持续告警应限频并在恢复时输出一次恢复日志。

## 14. 发布顺序

```text
数据库向前兼容迁移
  → Master 双栈版本
  → Caddy 双栈路由
  → Admin Web 状态拆分
  → 一台新 Worker canary
  → 分批升级 Worker
  → 观察旧协议调用归零
  → Master 关闭旧协议
  → 数据库清理迁移
  → Caddy 删除旧路由
```

禁止先升级 Worker 再部署支持新 RPC 的 Master，除非 Worker 已实现 `UNIMPLEMENTED` 回退。

## 15. 回滚方案

### 15.1 功能开关

兼容期 Master 提供：

```toml
[worker_protocol]
registration_mode = "dual"       # dual / ensure_only
authentication_mode = "dual"     # dual / certificate_only
```

回滚只允许从新模式切回 `dual`，不允许切到绕过证书校验的模式。

### 15.2 回滚原则

- 数据库删除列和删除表只能在观察期结束后执行；
- 清理迁移执行前保留可恢复备份；
- 新 Worker 在兼容期可回退旧注册 RPC；
- 已经签发的新证书继续有效，不因应用版本回滚而吊销；
- Caddy 始终保留证书指纹覆盖，不能为回滚临时信任客户端自报头；
- 回滚不能恢复一次性交付的脆弱语义作为长期方案。

## 16. 文件级修改清单

### 16.1 新增

```text
crates/worker-agent/src/runtime.rs
crates/worker-agent/src/master_port.rs
crates/worker-agent/src/credential_store.rs
crates/worker-agent/src/transport/mod.rs
crates/worker-agent/src/transport/tonic.rs
crates/master-server/src/grpc/ensure_registration.rs
crates/master-server/src/grpc/link_identity.rs
crates/master-server/src/store/registration_request.rs
crates/master-server/migrations/00xx_worker_link_simplification.sql
crates/master-server/tests/ensure_registration.rs
crates/master-server/tests/certificate_only_link.rs
```

### 16.2 修改

```text
crates/platform-proto/proto/worker.proto
crates/worker-agent/src/main.rs
crates/worker-agent/src/lib.rs
crates/worker-agent/src/client.rs
crates/worker-agent/src/config.rs
crates/worker-agent/src/tls.rs
crates/master-server/src/grpc/mod.rs
crates/master-server/src/grpc/auth.rs
crates/master-server/src/store/node.rs
crates/master-server/src/api/workers.rs
crates/master-server/src/models.rs
admin-web/src/**
deploy/Caddyfile
deploy/docker-compose.yml
deploy/worker-package-templates/worker.toml
deploy/worker-package-templates/README.txt
config/worker.example.toml
docs/部署与运维指南.md
```

### 16.3 兼容期后删除

```text
crates/master-server/src/grpc/enroll.rs
crates/master-server/src/grpc/registration.rs
crates/master-server/src/store/registration.rs
旧注册码管理接口与页面
旧注册会话迁移及专用测试
Worker enroll 子命令
```

删除前必须确认这些文件未承载新注册领域逻辑；旧协议 adapter 应在阶段 3 起调用新模块，避免清理时误删核心实现。

## 17. 验收标准

- [ ] Worker 生产配置只要求一个 Master 地址；
- [ ] 公网 Worker 协议最终只剩 `EnsureRegistration` 与 `OpenLink`；
- [ ] 无证书 Worker 只能注册，不能建立正式链路；
- [ ] 审批前不签发客户端证书；
- [ ] 审批后 Worker 自动领取证书并进行一次预期 mTLS 重连；
- [ ] 证书领取可幂等恢复，不存在“已领取但本地未保存”的死状态；
- [ ] 正式链路只使用实际客户端证书确定节点身份；
- [ ] Worker 不再保存或发送 node token；
- [ ] Worker 不再保存 Node CA；
- [ ] Master 不信任 Worker 自报节点编号或证书指纹；
- [ ] 注册、连接、运行三类状态独立显示；
- [ ] 批准后离线显示为“等待 Worker 建立连接”；
- [ ] 重连完成对账前不申请任务；
- [ ] 吊销证书后链路被拒绝；
- [ ] Master gRPC 端口不能绕过 Caddy 从公网访问；
- [ ] 旧 Worker 在兼容期仍能连接；
- [ ] 旧协议调用归零后才执行破坏性数据库清理；
- [ ] 日志、指标和错误响应不泄露私钥或凭据。

## 18. 实施优先级

建议按以下优先级排期：

1. **P0：状态拆分与 WorkerRuntime 收口**——直接降低排障成本，不改变生产协议；
2. **P0：EnsureRegistration 幂等领取**——消除审批后一次性交付导致的不可恢复状态；
3. **P1：证书唯一身份**——删除 node token 和重复身份元数据；
4. **P1：全量升级与旧协议观测**；
5. **P2：删除旧 RPC、旧字段和旧表结构**；
6. **P2：证书自动续签与轮换增强**。

其中阶段 1、2 可以单独发布；阶段 3、4 应在同一里程碑内完成，但仍需分别部署和验收。
