# 前端云端化与 Worker 直连注册修复实施方案 V5

> 编写日期：2026-08-23  
> 实施范围：仅限 `cloud-platform/` 目录内的云端管理平台、Master、Worker 与部署配置  
> 核心约束：不得修改或影响仓库根目录现有桌面客户端 `frontend/` 及 `frontend/src-tauri/`  
> 文档用途：交给其他智能体按阶段实施、测试和验收

## 1. 修复目标

本轮修复同时解决两个问题：

1. 云端管理前端没有继续采用 `shadcn-admin` 的产品结构，当前 `cloud-platform/web-admin` 功能过少、操作反馈不完整，导致大量后台能力无法使用或看起来“点击无反应”。
2. Worker 仍依赖管理员预先生成注册码，偏离“Worker 配置 Master 地址后直接连接并注册”的目标。

最终目标如下：

- 在 `cloud-platform/admin-web/` 内建设独立的 React 云端管理前端，设计与工程结构以 [shadcn-admin](https://github.com/satnaing/shadcn-admin) 为基础。
- 仓库根目录 `frontend/` 继续作为原有桌面客户端存在，代码、依赖、构建、路由和 Tauri 配置均不改动。
- 云端前端覆盖 Master 已有的主要管理能力，所有操作都有加载、成功、失败、权限不足和冲突反馈。
- Worker 只需配置一个公开的 Master gRPC 地址，即可自动发起注册，不再要求管理员生成或输入注册码。
- 自动注册不等于自动授权：新 Worker 首次连接后处于“待审核”，只有超级管理员批准后才签发正式客户端证书并允许领取任务。
- 所有展示给管理员的业务状态值使用中文，不再展示 `Registered`、`Pending`、`Online` 等英文枚举。
- 第一期按 5 台 Worker、每台默认约 5 个槽位设计，但每台 Worker 的槽位数可由管理员单独配置。
- 下载文件仍由 Worker 通过局域网直接写入同一台 NAS；Master 只下发相对目录，不接触 NAS 凭据和 NAS 文件流。
- Webshare 代理继续使用；一本书从开始到完成固定使用同一出口 IP，失败恢复也必须优先复用该 IP。

## 2. 不可违反的边界

### 2.1 原桌面客户端是冻结区

以下路径不得因本任务发生任何修改：

```text
frontend/
frontend/src/
frontend/src-tauri/
frontend/package.json
frontend/pnpm-lock.yaml
```

禁止行为：

- 不得把云端页面直接写进根目录 `frontend/`。
- 不得让云端前端从 `../../frontend/src` 导入组件、状态或 API。
- 不得修改桌面客户端的 Tauri 命令、路由、构建脚本或依赖版本。
- 不得为了云端构建而覆盖根目录锁文件。
- 不得把桌面客户端改成云端管理平台的壳。

交付前必须执行并确认：

```bash
git diff -- frontend
```

输出必须为空；若仓库开始实施前该目录已有用户改动，应记录基线并证明本任务未新增任何差异。

### 2.2 新云端前端必须独立

新的云端前端固定放置在：

```text
cloud-platform/admin-web/
```

它应拥有独立的：

- `package.json`
- `pnpm-lock.yaml`
- TypeScript、Vite、ESLint 配置
- 路由、查询缓存、认证状态和 API 客户端
- 组件目录、测试目录与构建产物

`cloud-platform/web-admin/` 是现有临时 Vue 前端。迁移期间可以短暂共存，但切换完成、回归通过后必须删除，不能长期维护两套云端前端。

### 2.3 注册和授权必须分离

Worker 能够直接连接并提交注册资料，但不能因此立即成为受信任节点。

必须满足：

```text
自动提交注册申请 ≠ 自动批准 ≠ 自动获得任务权限
```

只有超级管理员执行“批准”后，Master 才能签发正式 mTLS 客户端证书和节点令牌。处于“待审核”“已拒绝”“已过期”的节点均不得建立任务链路或领取任务。

## 3. 当前实现存在的问题

### 3.1 云端前端偏离目标技术栈

当前云端前端位于 `cloud-platform/web-admin/`，是一个功能非常有限的 Vue 单页应用；仓库根目录原客户端反而保留了较完整的 `shadcn-admin`、React、TanStack Router、TanStack Query、Radix 和 Tailwind 体系。

问题不是必须复用根目录客户端源码，而是云端管理前端没有独立采用同一成熟设计体系。正确做法是在 `cloud-platform/` 内单独落一份云端实现，保持部署边界清晰。

### 3.2 页面只覆盖少量后台能力

当前页面主要只有：

- 总览
- Worker
- 批次
- 注册码

而 Master 已经包含图书、任务、账号、代理、浏览器会话、日志、告警、设置、证书等管理能力。没有页面入口会让管理员误以为系统不可操作。

### 3.3 操作失败被静默吞掉

当前实现中存在捕获异常后不展示错误、只向控制台输出、没有 Toast、没有字段错误和请求编号等问题。典型结果是按钮点击后页面不变，用户只能判断为“无法执行任何操作”。

所有查询和变更必须有可见反馈，禁止空的 `catch`。

### 3.4 权限和按钮没有一致呈现

后端接口使用只读、可写、超级管理员等权限控制，但前端没有完整的权限矩阵。无权限用户仍可能看到按钮，点击后收到 403，却没有明确提示。

前端隐藏或禁用无权操作只是体验优化，后端仍必须作为最终权限边界。

### 3.5 批次恢复接口调用错误

暂停批次应调用：

```text
POST /api/batches/{id}/resume
```

不能把暂停后的恢复动作继续映射成 `/start`。前端动作必须按资源当前状态选择正确接口。

### 3.6 SSE 在线提示不可信

当前界面固定显示“SSE 实时在线”，但未根据 `onopen`、`onerror`、重连和关闭事件更新状态。页面应展示真实连接状态，而非静态文案。

### 3.7 Worker 注册流程仍依赖注册码

当前 protobuf、Master 注册服务和 Worker CLI 都围绕 `enroll_code` 设计：管理员先生成代码，Worker 再执行带 `--code` 的注册命令。这不符合 Worker 指定 gRPC 地址后自行注册的运维方式。

### 3.8 当前证书签发时机过早

若注册请求一提交就签发正式客户端证书，即使节点状态仍是待审核，也扩大了信任面。正式证书应当延迟到管理员批准之后签发。

## 4. 目标总体架构

```text
管理员浏览器
    │ HTTPS
    ▼
云端管理前端 cloud-platform/admin-web
    │ /api + /api/events
    ▼
Master HTTP API ─────────────── PostgreSQL
    │
    │ 同一个公开 gRPC 地址，例如 grpc.example.com:443
    ▼
Master gRPC 服务
    ▲                     ▲
    │ 长连接               │ 长连接
家中 Worker              办公室 Worker
    │ SMB/NFS              │ SMB/NFS
    └──────────► 同一台局域网 NAS
```

关键职责：

| 组件 | 职责 |
|---|---|
| `admin-web` | 管理员操作、审批、状态展示、配置和审计入口 |
| Master HTTP API | 用户认证、RBAC、资源管理、Worker 审批 |
| Master gRPC | Worker 自动注册、证书领取、长连接、心跳、任务下发与事件上报 |
| Worker | 浏览器自动化、Webshare 代理绑定、下载、NAS 写入、进度回报 |
| PostgreSQL | 全局图书去重、任务状态、节点身份、注册会话、审计和配置 |
| NAS | 最终图书文件唯一落盘位置；不经过云服务器中转 |

## 5. 云端前端修复方案

### 5.1 目录和技术选型

新建：

```text
cloud-platform/admin-web/
├── src/
│   ├── api/
│   ├── components/
│   ├── features/
│   ├── hooks/
│   ├── lib/
│   ├── routes/
│   ├── stores/
│   ├── test/
│   └── main.tsx
├── public/
├── package.json
├── pnpm-lock.yaml
├── tsconfig.json
├── vite.config.ts
└── LICENSES.md
```

建议技术栈：

- React + TypeScript + Vite
- shadcn/ui + Radix UI + Tailwind CSS
- TanStack Router
- TanStack Query
- TanStack Table
- React Hook Form + Zod
- Sonner 或同等 Toast 组件
- Vitest + Testing Library + MSW
- Playwright 用于关键管理流程 E2E

可以以 `shadcn-admin` 的页面布局、侧边栏、认证页、主题和组件组织为基础进行适配，但必须：

- 保留其 MIT 许可证和必要归属信息。
- 删除演示数据、演示登录和不相关页面。
- 不直接把上游仓库作为运行时依赖。
- 不从根目录桌面客户端复制带有 Tauri 依赖的业务代码。
- 将云端 API 调用统一收口到 `src/api/`。

### 5.2 页面清单

第一阶段至少实现以下路由：

| 路由 | 中文名称 | 核心能力 |
|---|---|---|
| `/login` | 登录 | 基于服务端会话登录，处理失效和退出 |
| `/` | 运行总览 | Worker、槽位、任务、下载、失败、告警摘要 |
| `/workers` | Worker 节点 | 在线状态、审核状态、槽位、版本、最后心跳、操作 |
| `/workers/pending` | 待审核节点 | 批准、拒绝、查看注册指纹和来源 IP |
| `/books` | 图书库 | 全局唯一图书、下载状态、NAS 相对路径、优先级 |
| `/batches` | 下载批次 | 新建、开始、暂停、恢复、终止、优先级 |
| `/tasks` | 下载任务 | 调度状态、Worker、槽位、固定代理、重试、错误 |
| `/accounts` | 下载账号 | 账号状态、分配、冷却和异常 |
| `/proxies` | Webshare 代理 | 代理池状态、出口 IP、占用书籍、健康度 |
| `/sessions` | 浏览器会话 | 会话归属、状态、异常和回收 |
| `/confirmations` | 待确认事项 | 需要人工确认的下载或账号事件 |
| `/alerts` | 告警中心 | 告警确认、处理、筛选 |
| `/logs` | 运行日志 | 按 Worker、任务、级别、请求编号检索 |
| `/settings` | 系统设置 | 全局并发、调度、重试、代理和保留策略 |
| `/users` | 管理员 | 用户、角色与禁用，仅超级管理员可见 |
| `/certificates` | 节点证书 | 有效期、指纹、吊销和审计，仅超级管理员可操作 |

原“注册码”菜单和页面必须删除。其位置由“待审核节点”替代。

### 5.3 Worker 页面字段

Worker 列表至少显示：

- 节点名称
- 安装标识缩写
- 注册状态
- 运行状态
- 操作系统
- Worker 版本
- 配置槽位数
- 已占用槽位数
- Master 当前分配数
- NAS 挂载检查状态
- 最后心跳时间
- 来源 IP
- 证书到期时间

注册状态必须显示中文：

```text
待审核
已批准
已拒绝
已过期
```

运行状态必须显示中文：

```text
未连接
正在连接
在线
忙碌
已暂停
离线
异常
已禁用
```

数据库或协议内部若保留稳定代码，必须通过统一字典转换后展示，禁止组件自行散落转换逻辑。

### 5.4 权限矩阵

至少保留三种角色：

| 能力 | 只读管理员 | 任务管理员 | 超级管理员 |
|---|---:|---:|---:|
| 查看运行数据 | 是 | 是 | 是 |
| 新建和调整批次 | 否 | 是 | 是 |
| 暂停、恢复、终止任务 | 否 | 是 | 是 |
| 调整图书优先级 | 否 | 是 | 是 |
| 批准或拒绝 Worker | 否 | 否 | 是 |
| 修改 Worker 槽位 | 否 | 否 | 是 |
| 吊销证书 | 否 | 否 | 是 |
| 管理用户和角色 | 否 | 否 | 是 |
| 修改系统级设置 | 否 | 否 | 是 |

前端应基于当前用户权限隐藏或禁用操作，并说明原因；后端每个写接口仍需独立校验权限。

### 5.5 API 客户端规范

统一实现 `src/api/client.ts`，要求：

- 自动携带同源会话 Cookie。
- 对所有响应先检查 HTTP 状态，再解析业务数据。
- 支持服务端返回的 `request_id`。
- 将错误转换为统一 `ApiError`，包含状态码、错误码、中文消息、字段错误和请求编号。
- 请求超时、网络断开和 JSON 解析失败均有独立中文提示。
- 禁止页面直接裸写 `fetch`。
- 禁止 `catch {}` 或只写 `console.error`。

统一错误行为：

| 状态 | 前端行为 |
|---|---|
| 401 | 清理认证缓存，跳转登录，提示“登录已失效” |
| 403 | 保留当前页，提示“权限不足”，显示请求编号 |
| 404 | 提示资源不存在并刷新相关列表 |
| 409 | 提示状态已变化或重复操作，刷新详情 |
| 422 | 把字段错误显示在表单对应位置 |
| 429 | 显示限流提示和可重试时间 |
| 5xx | 显示服务异常、请求编号和重试入口 |

### 5.6 所有写操作的交互标准

每个写操作必须具备：

- 点击后立即进入加载态并防止重复提交。
- 成功后显示中文 Toast。
- 失败后显示明确中文错误，而不是静默失败。
- 成功后精确刷新相关 Query Key。
- 危险操作弹出确认对话框。
- 对可以重复点击的动作使用后端幂等设计。

必须重点修复：

- 暂停批次调用 `/pause`。
- 恢复批次调用 `/resume`。
- 尚未开始的批次才调用 `/start`。
- 终止批次必须二次确认。
- Worker 批准和拒绝必须二次确认并记录操作人。
- 槽位数修改必须校验为允许范围内的正整数。

### 5.7 SSE 实时状态

前端连接 `/api/events` 后应维护以下状态：

```text
连接中
已连接
重连中
已断开
```

要求：

- `onopen` 后才显示“已连接”。
- `onerror` 后显示“重连中”，采用带抖动的指数退避。
- 超过最大连续失败阈值显示“已断开”和手动重连按钮。
- 收到事件后按资源类型使对应 Query Key 失效。
- 页面隐藏再恢复时重新检查连接。
- 登出时主动关闭 EventSource。

### 5.8 前端构建和部署

修改 `cloud-platform/Dockerfile` 的前端阶段，从 `cloud-platform/admin-web` 构建：

```dockerfile
FROM node:22-alpine AS admin-web-builder
WORKDIR /src/admin-web
RUN corepack enable
COPY admin-web/package.json admin-web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY admin-web/ ./
RUN pnpm build
```

为减少 Master 运行配置变更，可以仍将构建产物复制到容器内原路径：

```dockerfile
COPY --from=admin-web-builder /src/admin-web/dist /app/web-admin/dist
```

这样源代码目录是 `admin-web`，但 Master 的 `web_root=/app/web-admin/dist` 暂时保持不变。不要让 Docker 构建读取根目录 `frontend/`。

CI 至少执行：

```text
pnpm install --frozen-lockfile
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

工作目录必须是 `cloud-platform/admin-web`。

## 6. Worker 直连注册方案

### 6.1 管理员和 Worker 的最终体验

Worker 配置示例：

```toml
[master]
endpoint = "https://grpc.example.com:443"

[worker]
name = "office-pc-01"
requested_slots = 5

[storage]
nas_root = "Z:\\Books"
```

Linux/macOS 示例只需把 `nas_root` 改为本机挂载路径。该绝对路径永远保存在 Worker 本地配置中，不由 Master 统一覆盖。

启动方式：

```bash
worker-agent run --config worker.toml
```

不再要求：

```bash
worker-agent enroll --code ...
```

首次启动流程：

1. Worker 本地生成安装标识和私钥。
2. Worker 连接配置中的 gRPC 地址并提交注册申请和 CSR。
3. Master 创建“待审核”节点，返回短期注册会话。
4. Worker 保持等待或按退避策略轮询审批结果。
5. 超级管理员在“待审核节点”页面核对名称、系统、来源 IP、公钥指纹和申请槽位数。
6. 管理员批准，并可把槽位数改为实际允许值。
7. Master 此时才签发正式客户端证书和节点令牌。
8. Worker 保存身份材料，使用 mTLS 建立长期 `OpenLink`。
9. Master 标记节点在线并开始按槽位调度任务。

### 6.2 单一公开 gRPC 地址

对 Worker 只暴露一个逻辑地址，例如：

```text
grpc.example.com:443
```

由于首次注册时 Worker 尚无客户端证书，而后续任务链路必须使用 mTLS，入口代理应配置为“请求客户端证书但不在 TLS 握手阶段强制所有请求必须提供”。随后由 Master 按 RPC 方法执行严格校验：

| RPC | 客户端证书 | 额外校验 |
|---|---|---|
| `RegisterNode` | 首次可无 | TLS、限流、请求大小、安装标识、CSR 校验 |
| `WatchRegistration` | 首次可无 | 短期注册会话、CSR 私钥持有证明、有效期 |
| `OpenLink` | 必须有 | 证书指纹、节点令牌、已批准、未禁用、未吊销 |

不能因为入口代理采用可选客户端证书，就让 `OpenLink` 也变成可选认证。Master 方法级拦截器必须拒绝任何未携带有效证书的正式链路。

### 6.3 节点唯一身份

不能使用主机名作为唯一标识，因为不同地点可能存在相同主机名，主机名也可被修改。

采用双重身份：

- `installation_id`：Worker 首次启动生成的随机 UUID，持久化到本机身份目录。
- `public_key_fingerprint`：Worker CSR 公钥的 SHA-256 指纹。

幂等规则：

- 相同 `installation_id` 和相同公钥重复注册：返回原节点和原注册进度，不创建重复记录。
- 相同主机名但不同安装标识、公钥：视为两个不同 Worker。
- 相同安装标识但公钥变化：进入安全异常，不自动替换身份，要求管理员确认重新注册。
- 已拒绝或已禁用节点重复申请：不得绕过原决定创建新节点，应关联原记录并审计。

### 6.4 自动注册会话不是注册码

Master 可以返回机器自动管理的短期注册会话，但它不能成为管理员手工复制的“注册码”。

注册会话要求：

- 至少 256 位随机熵。
- 数据库只保存令牌哈希，不保存明文。
- 与节点、CSR 指纹、安装标识绑定。
- 默认 15 分钟滚动有效，注册申请最长保留 7 天。
- 每次查询审批状态都要验证令牌和私钥持有证明。
- 批准、拒绝、过期或证书领取完成后立即失效。
- 日志中不得输出完整令牌、私钥、节点令牌或证书私钥。

### 6.5 protobuf 变更

在 `cloud-platform/proto/worker.proto` 中新增直连注册 RPC，不复用旧字段语义：

```proto
rpc RegisterNode(RegisterNodeRequest) returns (RegisterNodeResponse);
rpc WatchRegistration(WatchRegistrationRequest) returns (stream RegistrationEvent);
rpc OpenLink(stream WorkerMessage) returns (stream MasterMessage);
```

消息至少包含：

```text
RegisterNodeRequest
- installation_id
- node_name
- os_type
- os_version
- agent_version
- requested_slots
- csr_pem
- public_key_fingerprint
- nonce
- nonce_signature

RegisterNodeResponse
- node_id
- registration_status
- registration_session
- expires_at
- retry_after_seconds

RegistrationEvent
- registration_status
- approved_slots
- client_certificate_pem（仅批准后一次性返回）
- ca_certificate_pem（仅批准后返回）
- node_token（仅批准后一次性返回）
- rejection_reason
```

要求：

- 旧 `Enroll` RPC 先标记弃用，至少保留一个兼容发布周期。
- protobuf 已使用的字段编号永久保留，禁止复用。
- 新状态在协议中可以使用稳定枚举代码，但前端、日志和管理员 API 必须输出中文标签。
- 证书和节点令牌领取需要做到幂等且防止被其他注册会话领取。

### 6.6 数据库迁移

新增迁移建议命名：

```text
cloud-platform/crates/master-server/migrations/0004_direct_registration.sql
```

对 `worker_nodes` 补充或规范以下字段：

| 字段 | 用途 |
|---|---|
| `installation_id` | Worker 安装实例 UUID |
| `public_key_fingerprint` | CSR 公钥 SHA-256 指纹 |
| `registration_status` | 待审核、已批准、已拒绝、已过期 |
| `requested_slots` | Worker 申请值 |
| `configured_slots` | 管理员批准的实际槽位数 |
| `registration_expires_at` | 注册申请到期时间 |
| `first_seen_ip` | 首次申请来源 IP |
| `last_registration_at` | 最近注册请求时间 |
| `approved_at/by` | 审批时间和操作人 |
| `rejected_at/by/reason` | 拒绝审计信息 |

新增 `worker_registration_sessions`：

| 字段 | 要求 |
|---|---|
| `id` | UUID 主键 |
| `node_id` | 外键关联 Worker |
| `token_hash` | 唯一，只保存哈希 |
| `csr_pem` | 仅保存公钥 CSR，不包含私钥 |
| `csr_fingerprint` | 唯一或与节点组成唯一约束 |
| `challenge` | 服务端挑战值 |
| `status` | 待审核、已批准、已拒绝、已过期、已领取 |
| `expires_at` | 会话过期时间 |
| `attempt_count` | 防暴力请求计数 |
| `created_at/last_seen_at` | 审计时间 |

索引与约束：

- `installation_id` 非空后建立唯一索引。
- `public_key_fingerprint` 建立唯一或受状态约束的唯一索引。
- 槽位数必须在系统允许范围内，例如 `1..50`。
- 所有审批人字段外键关联管理员用户。
- 状态字段设置数据库约束，禁止任意字符串。
- 审批更新使用事务和行锁，防止重复批准并重复签证书。

首个迁移版本不得立即删除 `enroll_codes` 表，以保证滚动升级。所有 Worker 升级完成并观察一个版本周期后，再用独立迁移删除旧表、旧 API 和旧 RPC。

### 6.7 Master 注册状态机

```text
未注册
  │ RegisterNode
  ▼
待审核 ──管理员拒绝──► 已拒绝
  │
  ├──超过期限────────► 已过期
  │
  └──管理员批准──────► 已批准
                          │ 领取证书并 OpenLink
                          ▼
                        在线/忙碌/离线
```

规则：

- 待审核节点不能领取任务。
- 待审核阶段不签发正式客户端证书。
- 批准动作只有超级管理员可执行。
- 批准时管理员可以调整 `configured_slots`，默认值取申请槽位但受全局上限限制。
- 同一次批准事务只能生成一套有效身份。
- 拒绝必须支持填写中文原因。
- 证书吊销后现有长连接应被关闭，后续连接拒绝。
- 节点被禁用后即使证书仍在有效期内也不能连接。

### 6.8 注册接口防护

公开注册 RPC 至少实现：

- 按来源 IP 限流。
- 按安装标识和公钥指纹限流。
- 全局待审核节点数量上限。
- 节点名、版本、系统信息和 CSR 大小限制。
- CSR 格式、签名算法和密钥长度校验。
- nonce 重放保护。
- 注册会话失败次数限制。
- 来源 IP、User-Agent/Agent 版本、指纹和失败原因审计。
- 对过旧 Worker 版本返回明确升级要求。
- 定时清理过期注册会话，但保留必要审计摘要。

第一阶段不建议对公网节点自动批准。若未来需要无人值守自动扩容，应另行设计预置设备身份、云实例证明或企业 CA，不得把“无需注册码”解释为“任何互联网主机自动获得任务权限”。

### 6.9 Worker 本地身份存储

Worker 首次运行生成并持久化：

- `installation_id`
- 私钥
- CSR 或其必要元数据
- 批准后取得的客户端证书
- CA 证书
- 节点令牌

要求：

- Windows 使用受限 ACL，Linux/macOS 使用仅当前用户可读写权限。
- 私钥不得写入 TOML 配置、日志或崩溃报告。
- 写入使用临时文件加原子替换，防止断电造成半文件。
- Worker 重启必须复用已有身份，不能重复产生节点。
- 身份损坏时进入明确的“身份异常”，不能静默创建新节点绕过审核。
- 提供显式 `reset-identity` 维护命令，并要求危险操作确认；该命令不能自动让服务器忘记旧身份。

### 6.10 连接和重连

Worker `run` 统一处理：

1. 无本地身份：自动注册并等待审核。
2. 有待审核会话：恢复等待，不重复创建节点。
3. 已批准且有证书：直接建立 `OpenLink`。
4. 证书接近到期：走受认证的续期流程。
5. 网络中断：指数退避并带随机抖动重连。
6. Master 返回已禁用或已吊销：停止高频重试，显示明确原因。

建议退避：1 秒起步，逐步到 60 秒上限；成功稳定连接后重置。网络短暂抖动不能导致重复注册、重复任务或丢失已绑定代理。

## 7. 下载、NAS 和代理约束保持不变

### 7.1 NAS 路径

Master 中只保存全局相对路径，例如：

```text
books/作者/书名/文件名.pdf
```

Worker 本地配置保存绝对挂载根目录：

```text
Windows: Z:\Books
Linux:   /mnt/nas/books
macOS:   /Volumes/Books
```

实际路径由 Worker 安全拼接。必须拒绝 `..`、绝对路径注入、非法分隔符和符号链接越界。Worker 启动和心跳应上报 NAS 可写性、剩余空间和挂载状态。

### 7.2 全局图书唯一

一本书全局只保存一份。任务创建必须基于稳定书籍标识或规范化来源标识建立数据库唯一约束，不能仅靠前端去重。

并发调度时使用事务或原子占用，确保两个 Worker 不会同时把同一本书当作全新任务下载。

### 7.3 一本书固定一个代理 IP

一本书进入下载执行后，Master 必须记录代理租约：

```text
book/task -> proxy_id -> observed_exit_ip
```

要求：

- 同一本书的所有请求、重试和断点恢复优先使用同一 `proxy_id`。
- Worker 实际探测并上报出口 IP，Master 校验是否发生变化。
- 代理失效时任务进入“等待代理”或明确的恢复流程，不能静默切换 IP 后继续。
- 若业务允许人工强制换 IP，必须在后台二次确认、记录审计，并从安全检查点重新开始。
- 槽位是每台电脑允许同时执行的书籍任务数，不是浏览器可开启数量。

## 8. 后端 API 调整

新增或规范以下管理员 API，具体 REST 命名可按现有风格微调，但语义必须一致：

```text
GET  /api/workers?registration_status=待审核
GET  /api/workers/{id}
POST /api/workers/{id}/approve
POST /api/workers/{id}/reject
PATCH /api/workers/{id}/slots
POST /api/workers/{id}/disable
POST /api/workers/{id}/enable
POST /api/workers/{id}/certificates/revoke
```

批准请求示例：

```json
{
  "configured_slots": 5,
  "remark": "办公室下载节点"
}
```

拒绝请求示例：

```json
{
  "reason": "来源设备未知，请联系管理员确认"
}
```

统一响应要求：

- 业务状态和提示文案返回中文。
- 每个错误响应包含稳定 `code`、中文 `message` 和 `request_id`。
- 资源状态冲突返回 409，不伪装成 500。
- 参数错误返回 422，并包含字段错误。
- 审批和证书操作写入审计日志。

旧注册码 API 在兼容阶段标记为弃用，不再出现在新前端。兼容期结束后删除路由、服务、存储代码和数据库表。

## 9. 文件级修改清单

### 9.1 新增

```text
cloud-platform/admin-web/**
cloud-platform/crates/master-server/migrations/0004_direct_registration.sql
cloud-platform/crates/master-server/src/grpc/registration.rs
cloud-platform/crates/master-server/src/store/registration.rs
cloud-platform/crates/master-server/src/api/worker_registrations.rs
cloud-platform/crates/master-server/tests/direct_registration.rs
```

实际 Rust 模块名应遵循现有工程规范，禁止为了照抄此清单重复创建与已有职责相同的模块。

### 9.2 修改

```text
cloud-platform/proto/worker.proto
cloud-platform/crates/master-server/src/grpc/mod.rs
cloud-platform/crates/master-server/src/api/mod.rs
cloud-platform/crates/master-server/src/store/worker*.rs
cloud-platform/crates/worker-agent/src/main.rs
cloud-platform/crates/worker-agent/src/config*.rs
cloud-platform/crates/worker-agent/src/tls*.rs
cloud-platform/worker.example.toml
cloud-platform/Caddyfile
cloud-platform/Dockerfile
cloud-platform/docker-compose*.yml
cloud-platform/.github/workflows/**（若工作流位于仓库级则只改云端相关 job）
```

### 9.3 兼容期后删除

```text
cloud-platform/web-admin/
旧 enroll code HTTP API
旧 enroll code store/service
旧 Enroll RPC 及其客户端命令
enroll_codes 数据表（通过后续独立迁移）
```

### 9.4 永不修改

```text
frontend/**
```

## 10. 分阶段实施顺序

### 阶段 0：建立基线

- 记录 `git status` 和 `git diff -- frontend`。
- 运行当前 Rust、前端和部署测试，记录已有失败。
- 盘点现有 HTTP API 与页面映射，禁止凭想象新增重复接口。

完成条件：原客户端差异基线明确，当前可用能力有清单。

### 阶段 1：创建独立 `admin-web`

- 在 `cloud-platform/admin-web` 引入并适配 shadcn-admin 结构。
- 完成登录、布局、主题、菜单、路由保护、API 客户端和全局错误反馈。
- 先用真实 API 完成总览、Worker、批次三个核心页面。

完成条件：独立安装和构建，不读取根目录 `frontend`，核心页面可操作。

### 阶段 2：补齐管理功能

- 实现图书、任务、账号、代理、会话、告警、日志、设置等页面。
- 完成权限矩阵、写操作反馈和 SSE 状态。
- 修复批次开始、暂停、恢复、终止接口映射。

完成条件：Master 已有管理能力均有合理入口，不能操作时有明确原因。

### 阶段 3：增加直连注册协议和存储

- 添加数据库迁移。
- 新增 protobuf RPC，不立即删除旧 RPC。
- 实现幂等注册、会话验证、状态机、限流和审计。
- 把证书签发移动到批准动作之后。

完成条件：后端集成测试覆盖注册、审批、拒绝、过期和攻击路径。

### 阶段 4：改造 Worker 启动流程

- `run` 在无身份时自动注册。
- 本地安全保存安装标识、私钥、会话和证书。
- 支持等待审核、断线恢复和身份异常。
- 保留旧 `enroll` 命令一个兼容周期并打印弃用提示。

完成条件：新机器只配置 endpoint 和 NAS 路径即可出现在待审核列表。

### 阶段 5：审批页面与单地址入口

- 完成待审核节点页面和批准/拒绝动作。
- 配置入口代理的可选客户端证书请求。
- Master 对正式 RPC 强制 mTLS 和节点状态校验。
- 验证来源 IP 和审计信息获取正确。

完成条件：未批准节点无法打开任务链路，批准后无需人工复制任何密钥即可上线。

### 阶段 6：切换部署

- Docker 改为构建 `admin-web`。
- 在测试环境执行数据库迁移和滚动升级。
- 完成浏览器回归后删除 `cloud-platform/web-admin`。
- 保留旧注册码后端兼容能力直至旧 Worker 全部升级。

完成条件：生产镜像只包含新管理前端，桌面客户端完全未受影响。

### 阶段 7：移除旧注册码机制

- 统计旧 `Enroll` 使用量为零。
- 删除旧页面、API、RPC、CLI 和存储逻辑。
- 单独迁移删除 `enroll_codes` 表。
- 更新运维手册和示例配置。

完成条件：代码、数据库和文档均不再要求注册码，protobuf 字段编号仍保留不复用。

## 11. 测试要求

### 11.1 云端前端单元和集成测试

必须覆盖：

- 登录成功、失败和会话失效。
- 不同角色看到的菜单和按钮不同。
- 403 显示“权限不足”，不能静默。
- 409 状态冲突后刷新资源。
- 422 显示字段错误。
- 暂停批次使用 `/pause`。
- 恢复批次使用 `/resume`，不能使用 `/start`。
- Worker 批准、拒绝和槽位调整。
- SSE 的连接中、已连接、重连中和已断开状态。
- 菜单中不存在“注册码”，存在“待审核节点”。
- 所有英文内部状态正确转换为中文显示。

### 11.2 Master 单元和集成测试

必须覆盖：

- 相同安装标识和公钥重复注册不产生重复节点。
- 相同主机名、不同安装标识可以注册为两个节点。
- 相同安装标识、公钥突变触发安全异常。
- 待审核节点不能获取证书、节点令牌或任务。
- 只有超级管理员能批准节点。
- 批准后才签发证书，重复批准不重复签发。
- 已拒绝节点无法继续完成注册。
- 过期注册会话无法查询或领取身份。
- 错误、伪造或泄漏后的会话令牌不能越权。
- CSR 私钥持有证明错误时拒绝。
- 注册限流和待审核总量上限生效。
- 被禁用节点和吊销证书不能建立 `OpenLink`。
- 数据库唯一约束和并发审批事务正确。

### 11.3 Worker 测试

必须覆盖：

- 空身份目录首次运行会自动注册。
- 重启后复用注册会话，不创建重复节点。
- 批准后原进程能自动取得证书并上线。
- 重启后复用证书直接上线。
- 网络中断使用退避重连。
- 身份文件部分损坏时进入身份异常。
- NAS 不可写时不上报为可接任务。
- 日志不会泄露私钥、注册令牌或节点令牌。

### 11.4 端到端验收

使用至少两种系统的 Worker 进行测试，建议 Windows + Linux 或 Windows + macOS：

1. 新 Worker 配置唯一 gRPC 地址、名称、申请槽位和本地 NAS 路径。
2. 执行 `worker-agent run`，不输入注册码。
3. 10 秒内在云端“待审核节点”页面看到申请。
4. 管理员看到真实系统、来源 IP、公钥指纹和申请槽位。
5. 管理员批准并把槽位设置为 5。
6. Worker 自动取得身份并显示“在线”。
7. 重启 Worker，不能产生重复节点。
8. 创建下载批次，任务只分配到批准节点。
9. 文件通过局域网写入 NAS，不上传到云服务器。
10. 同一本书重试时保持同一 Webshare 出口 IP。
11. 暂停和恢复批次均有成功反馈并调用正确接口。
12. 只读管理员无法执行写操作，并看到明确权限提示。

## 12. 部署与回滚

### 12.1 上线前备份

- 备份 PostgreSQL。
- 保存当前云端镜像标签。
- 保存当前 Caddy 和 Master 配置。
- 记录已在线 Worker 的版本、节点 ID 和证书指纹。

### 12.2 推荐上线顺序

1. 先部署兼容新旧协议的 Master。
2. 部署新 `admin-web`。
3. 小范围升级 1 台 Worker 验证直连注册。
4. 验证审批、证书、长连接、NAS 和下载。
5. 分批升级其余约 4 台 Worker。
6. 观察一个完整任务周期。
7. 后续版本再移除旧注册码机制。

### 12.3 回滚原则

- 数据库迁移优先采用向前兼容，不在首次上线删除旧字段或表。
- 新前端失败可将镜像切回旧前端，但不要回滚已创建的节点身份数据。
- 新 Worker 协议失败时，兼容期内可以临时使用旧注册流程。
- 不得通过删除 Worker 表或清空证书表来“快速回滚”。

## 13. 实现智能体禁止事项

- 禁止修改根目录 `frontend/**`。
- 禁止把 Vue 和 React 两套云端前端长期并存。
- 禁止保留一个不可操作的纯展示后台冒充完成。
- 禁止吞掉任何 API 错误。
- 禁止写死“SSE 在线”。
- 禁止把暂停后的批次调用 `/start`。
- 禁止使用主机名作为 Worker 唯一身份。
- 禁止首次注册时立即签发正式证书。
- 禁止让待审核节点建立正式任务链路。
- 禁止在数据库或日志中保存明文注册会话令牌。
- 禁止把“无需注册码”实现成公网自动批准。
- 禁止复用 protobuf 旧字段编号。
- 禁止让 Master 中转下载文件或保存 NAS 账号密码。
- 禁止任务失败后静默更换一本书使用的代理 IP。
- 禁止将管理员可见的状态值展示为英文。

## 14. 最终验收清单

只有全部满足才可宣布完成：

- [ ] 新云端前端位于 `cloud-platform/admin-web`。
- [ ] 前端采用 shadcn-admin 的 React 管理后台结构和视觉体系。
- [ ] `git diff -- frontend` 证明原桌面客户端未受影响。
- [ ] Docker 和 CI 只构建 `cloud-platform/admin-web`。
- [ ] 现有 Master 主要管理能力都有页面入口。
- [ ] 所有按钮有加载、成功和失败反馈。
- [ ] 角色权限在前后端均生效。
- [ ] 批次暂停、恢复接口调用正确。
- [ ] SSE 状态反映真实连接生命周期。
- [ ] 前端不再出现注册码页面。
- [ ] Worker 只配置一个 gRPC 地址即可提交注册。
- [ ] 新节点默认显示“待审核”。
- [ ] 只有超级管理员能批准或拒绝节点。
- [ ] 正式证书仅在批准后签发。
- [ ] 未批准、已拒绝、已禁用和证书吊销节点不能领取任务。
- [ ] Worker 重启不会重复注册。
- [ ] 每台 Worker 可单独配置槽位数，第一阶段默认可设为 5。
- [ ] NAS 绝对路径按 Worker 本地配置，文件不经过云端中转。
- [ ] 全局一本书只有一条有效下载成果。
- [ ] 一本书的下载过程固定使用同一个 Webshare 出口 IP。
- [ ] 管理员可见状态、错误和提示均为中文。
- [ ] 安全、单元、集成和端到端测试全部通过。

## 15. 交付报告模板

实现智能体完成后必须提交以下内容，不能只回复“已完成”：

```text
1. 修改文件清单
2. 数据库迁移说明
3. protobuf 兼容说明
4. 云端前端路由和 API 映射表
5. Worker 首次注册完整时序
6. 权限与安全控制说明
7. 已执行的命令和测试结果
8. 未通过测试及原因
9. 部署和回滚步骤
10. git diff -- frontend 的结果
11. 仍保留的兼容代码及计划删除版本
```

任何未实现项、临时 mock、跳过测试或兼容性风险都必须明确列出，不能隐藏在“后续优化”中。
