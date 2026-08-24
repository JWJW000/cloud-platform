# CSV 建批与业务任务统一下发实现方案 V6

> 编写日期：2026-08-23  
> 实施范围：`cloud-platform/` 目录内的管理前端、Master、Worker、协议、数据库与测试  
> 前置基础：V5 云端前端和 Worker 直连注册已经实现  
> 核心目标：管理员通过网页上传 CSV 创建下载批次和账号注册任务；批次启动后由 Master 自动下发给在线 Worker 执行  
> 强制约束：不得修改或影响仓库根目录原桌面客户端 `frontend/` 和 `frontend/src-tauri/`

## 1. 需求确认

本方案以以下业务含义为唯一实现口径。

### 1.1 下载任务

1. 管理员在云端网页上传图书 CSV 文件，新建下载批次。
2. 文件格式和原客户端兼容，不能要求管理员改成网页文本逐行粘贴。
3. Master 解析、校验、全局去重并持久化批次和图书任务。
4. 批次处于“待开始”时不执行；管理员点击“开始”后进入“执行中”。
5. Master 自动选择在线、已批准、有空闲槽位、NAS 正常且能力匹配的 Worker。
6. Worker 通过现有 gRPC 长连接接收会话与任务，在本机执行浏览器自动化。
7. 文件由 Worker 经局域网直接写入已配置的同一台 NAS，不经过云服务器中转。

### 1.2 账号和账号注册任务

必须区分资源与任务：

- “账号”是可被调度和租赁的业务资源。
- “账号注册任务”是 Worker 需要执行的浏览器自动化工作。
- “Worker 节点注册”是节点接入 Master 的安全流程，和账号注册任务完全不同。

管理员可在网页：

1. 单个新增账号。
2. 上传账号文件批量导入“待注册账号”。
3. 上传账号文件批量导入“已注册账号”。
4. 为待注册账号创建账号注册批次。
5. 启动、暂停、恢复、取消账号注册批次。
6. 查看每个注册任务的 Worker、进度、结果、重试和人工确认状态。

账号注册批次启动后，同样由 Master 自动选择 Worker 下发，不由管理员手工指定每个账号去哪台电脑。

### 1.3 网页和 Worker 的职责边界

```text
网页：创建、配置、启动、暂停、恢复、取消、查看、人工确认
Master：校验、持久化、排队、优先级、资源租赁、调度、权限、审计、结果裁决
Worker：浏览器自动化、下载、账号注册、代理连接、NAS 写入、进度和证据上报
```

网页不能直接连接 Worker，也不能把密码、代理或任务参数通过浏览器转发给 Worker。所有业务动作必须先写入 Master 数据库，再通过已认证的 gRPC 长连接可靠下发。

## 2. 当前实现评估

### 2.1 已有能力，应保留

当前代码已经具备以下可复用基础：

- `cloud-platform/admin-web/` 独立云端前端。
- Worker 直连注册、管理员审批和 mTLS 长连接。
- 中文领域状态枚举。
- 图书全局去重和批次状态。
- `book_tasks` 图书任务租约、执行编号和阶段版本。
- `execution_sessions` 会话级槽位、账号和代理联合租赁。
- Worker 本地 Outbox、Master 事件幂等处理和重连对账。
- NAS 临时文件、哈希校验和非覆盖式原子提交。
- Webshare 代理资源与出口 IP 字段。
- `automation-core` 中图书下载和账号注册的真实执行引擎。

这些能力不得因本轮改造被重写成另一套并行系统。

### 2.2 当前下载批次并不是真正的文件上传

当前 `admin-web/src/pages/BatchesPage.tsx` 使用文本框，让管理员粘贴：

```text
书名[,作者[,出版社[,ISBN]]]
```

然后浏览器自行用逗号切分并提交 JSON `rows`。这与原客户端“选择 CSV 文件”不一致，也不能正确处理 CSV 引号、字段内逗号、BOM 和编码错误。

当前 `POST /api/batches/import` 接收 JSON `rows` 或 `csv_text`，没有真正的 multipart 文件上传、预检和提交阶段。

### 2.3 调度方向仍由 Worker 声明任务类型

当前 Worker 发送 `SessionRequest.task_type`，Master 按 Worker 给出的类型调用 `allocate_session`。这会导致：

- Worker 而不是 Master 决定下一项工作是什么。
- 网页启动的账号注册任务没有可靠的队列唤醒关系。
- 老版本或异常 Worker 可以请求不应执行的任务类型。
- Master 难以在下载、注册、核验和代理检测之间统一安排优先级。

正确方向应是 Worker 只上报“某槽位空闲及其能力”，由 Master 根据数据库队列选择任务类型。

### 2.4 账号注册引擎存在，但 Worker 编排没有调用

`automation-core` 已提供 `register_account`，但当前 Worker 的 `execute_session_loop` 固定执行：

```text
打开会话并登录 → 循环 NextTaskRequest → 执行图书下载
```

它没有根据 `CreateSession.task_type` 分支调用账号注册引擎。当前账号注册会话即使被分配，也会进入下载会话路径，无法形成正确的注册结果闭环。

### 2.5 账号注册缺少独立任务记录

账号表只有账号状态和会话占用信息，缺少：

- 注册批次
- 注册任务编号
- 任务优先级
- 尝试次数
- 下一次重试时间
- 租约执行编号
- 明确的开始、暂停、取消和完成统计
- 人工验证码确认记录

不能用账号状态代替任务状态。资源状态和执行状态必须分开。

### 2.6 当前任务执行记录只绑定图书任务

`task_executions.task_id` 当前指向 `book_tasks`。账号注册需要独立执行记录或兼容扩展，否则无法统一审计 Worker 接收、执行、重试和结果归因。

### 2.7 一本书固定 IP 仍需加强

当前代理主要固定在执行会话上，一个会话可以连续下载多本书。但一本书失败后若在另一会话重试，仍可能绑定另一条代理或另一出口 IP。

必须把图书和代理的绑定持久化到任务级，而不仅是会话内存或会话记录。

## 3. 目标架构

```text
管理员上传 CSV
      │
      ▼
Master 文件预检 ──► 返回预览、错误和导入令牌
      │ 管理员确认
      ▼
事务创建批次、资源和任务
      │ 点击开始
      ▼
统一调度选择器
  ├── 图书下载队列
  ├── 账号注册队列
  ├── NAS 核验队列
  └── 代理检测队列
      │
      ▼
在线 Worker 上报空闲槽位
      │
      ▼
Master 选择工作类型并创建租约
      │ gRPC
      ▼
Worker 按任务类型执行
      │ 可靠事件 + 证据
      ▼
Master 幂等提交结果、释放资源、SSE 更新网页
```

核心原则：

- 任务先持久化，后下发。
- Worker 报能力，Master 做选择。
- 每次下发都有执行编号和租约世代。
- “已发送”不等于“已接收”，“已接收”不等于“已完成”。
- Worker 结果是建议和证据，Master 负责最终状态裁决。

## 4. CSV 文件上传设计

### 4.1 图书 CSV 兼容格式

必须兼容原客户端的两种格式。

单列：

```csv
书名
第一本书
第二本书
```

四列：

```csv
书名,作者,出版社,ISBN
天上有个大薯片,夏忠波著,电子工业出版社有限公司,9787121110627
```

规则：

- 第一列“书名”必填。
- 作者、出版社、ISBN 可空。
- 可有或没有表头。
- 接受 UTF-8 和 UTF-8 BOM。
- CSV 必须使用标准解析器，支持带引号字段和字段内逗号。
- 不能使用 `line.split(',')` 或同时按中文逗号切分。
- 空白行忽略，但保留原始行号用于错误报告。
- ISBN 统一去除空格和连字符后校验，不满足 ISBN-10/13 时列为错误或警告，不得静默改值。

### 4.2 账号文件兼容格式

原客户端账号导入使用：

```text
email@example.com----password
```

云端应兼容该格式，同时建议支持标准 CSV：

```csv
邮箱,密码,昵称
email@example.com,StrongPassword,example
```

账号导入页面必须让管理员明确选择：

```text
导入为待注册账号
导入为已注册账号
```

安全要求：

- 预览只显示脱敏邮箱和“密码已提供”，不能回显密码。
- 账号原始文件不能长期保存在普通文件目录。
- 导入完成后立即删除临时明文文件。
- 密码在数据库中使用现有应用层加密，不得写入操作日志、错误详情、SSE 或浏览器缓存。
- 待注册账号可按当前兼容规则规范化邮箱；已注册账号是否保留邮箱大小写应与原客户端保持一致并写测试固定。
- 不建议继续“超长密码静默截断”；云端应在预检中明确报错或要求管理员确认兼容处理。

### 4.3 两阶段上传接口

采用“预检”和“提交”两阶段，避免上传即创建无法撤回的批次。

#### 图书文件预检

```text
POST /api/imports/books/preview
Content-Type: multipart/form-data
```

字段：

```text
file
batch_name
download_format
priority
max_attempts
```

响应：

```json
{
  "import_token": "一次性短期令牌",
  "file_name": "books.csv",
  "file_sha256": "...",
  "total_rows": 1200,
  "valid_rows": 1188,
  "duplicate_in_file": 6,
  "duplicate_in_library": 4,
  "already_ingested": 2,
  "error_rows": 0,
  "warnings": [],
  "preview": []
}
```

#### 图书文件提交

```text
POST /api/imports/books/commit
```

```json
{
  "import_token": "...",
  "start_immediately": false
}
```

提交接口必须是幂等的。相同 `import_token` 重复提交返回同一个批次，不能重复建批。

#### 账号文件预检和提交

```text
POST /api/imports/accounts/preview
POST /api/imports/accounts/commit
```

提交请求应包含导入模式：

```json
{
  "import_token": "...",
  "mode": "待注册",
  "create_registration_batch": true,
  "start_immediately": false,
  "priority": 10
}
```

### 4.4 上传安全限制

默认限制建议：

| 项目 | 限制 |
|---|---:|
| 图书 CSV | 10 MiB |
| 账号文件 | 5 MiB |
| 单次图书行数 | 50,000 |
| 单次账号行数 | 10,000 |
| 单字段长度 | 1,000 字符 |
| 文件名长度 | 255 字符 |
| 预检令牌有效期 | 30 分钟 |

必须做到：

- 只使用服务器生成的临时文件名，不直接使用上传文件名作为路径。
- 校验实际内容可以被 CSV/文本解析，不能只信扩展名或 MIME。
- 在流式读取时同时计算 SHA-256 和限制总大小。
- 超限立即终止，不把整个文件读入无限内存。
- 预检临时文件放在不可被静态服务访问的位置。
- 预检令牌只保存哈希，绑定管理员、文件哈希和导入类型。
- 预检提交后、过期后或取消后删除临时文件。
- 错误 CSV 导出时防止公式注入；以 `= + - @` 开头的单元格必须转义。
- 所有接口执行登录校验、写权限校验、CSRF 防护和速率限制。

### 4.5 数据库存档

新增 `import_jobs`，建议迁移文件为下一个顺序迁移，例如：

```text
cloud-platform/crates/master-server/migrations/0006_business_tasks.sql
```

字段：

```text
id UUID
import_type 中文：图书、账号
status 中文：预检中、待确认、已提交、已过期、失败
original_file_name
file_sha256
temp_path（可为空，提交后清除）
token_hash
created_by
expires_at
committed_at
committed_resource_id
summary JSONB（不得包含账号密码）
created_at / updated_at
```

图书 CSV 可按配置保留原文件用于审计，例如 30 天；账号原始文件默认不保留，只保存哈希、行数和导入摘要。

## 5. 批次和任务数据模型

### 5.1 保留图书现有模型

继续使用：

- `download_batches`
- `batch_books`
- `books`
- `book_tasks`
- `book_files`

不要为了统一外观复制出第二套图书任务表。

图书任务状态继续使用现有中文状态：

```text
待处理
已分配
执行中
等待入库
待确认
已完成
失败
已跳过
已取消
```

### 5.2 新增账号注册批次

新增 `account_registration_batches`：

```text
id UUID PRIMARY KEY
name TEXT NOT NULL
source_file TEXT
status TEXT：待开始、执行中、已暂停、已完成、已取消
priority INT NOT NULL
created_by UUID
created_at / updated_at
```

### 5.3 新增账号注册任务

新增 `account_registration_tasks`：

```text
id UUID PRIMARY KEY
batch_id UUID NOT NULL
account_id UUID NOT NULL
status TEXT NOT NULL
priority INT NOT NULL
attempts INT NOT NULL DEFAULT 0
max_attempts INT NOT NULL DEFAULT 3
next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now()
lease_node_id UUID
lease_session_id UUID
lease_execution_id UUID
lease_expires_at TIMESTAMPTZ
stage TEXT
stage_version INT NOT NULL DEFAULT 0
last_error TEXT
cancel_requested BOOLEAN NOT NULL DEFAULT FALSE
created_at / updated_at
UNIQUE(batch_id, account_id)
```

状态建议：

```text
待处理
已分配
执行中
等待人工确认
正在重试
已完成
失败
已取消
```

账号资源状态仍为：

```text
待注册
已注册
待验证
登录失败
今日额度耗尽
已禁用
```

任务结果和账号资源状态必须在同一个事务中更新。例如注册成功时：

```text
账号注册任务 → 已完成
账号资源 → 已注册
registered_at → 当前时间
```

### 5.4 执行记录兼容扩展

扩展 `task_executions`：

- 将现有 `task_id` 语义明确为图书任务，可保持字段名兼容现有代码。
- 新增 `account_registration_task_id UUID NULL`。
- 新增 `task_type TEXT NOT NULL DEFAULT '图书下载'`。
- 图书执行要求 `task_id` 非空、账号注册执行要求 `account_registration_task_id` 非空。
- 使用 CHECK 约束保证一个执行只关联一种业务任务。

若决定把 `task_id` 改名为 `book_task_id`，必须在同一版本完整修改所有 SQL、模型、API、测试和 protobuf 转换，不能留下混用。本方案更建议先保留字段名，减少迁移风险。

### 5.5 管理员远程命令

节点暂停、恢复、刷新配置、NAS 核验、代理检测等网页动作应写入 `worker_commands`：

```text
id UUID
command_type 中文
target_node_id UUID NULL
payload JSONB（严格按命令类型校验）
status 中文：待下发、已下发、已接收、执行中、已完成、失败、已过期、已取消
idempotency_key TEXT UNIQUE
created_by UUID
sent_at / accepted_at / completed_at / expires_at
result JSONB
created_at / updated_at
```

不含敏感信息的命令才允许存储 JSONB。账号密码和代理密码仍由资源分配代码按需解密后直接组装 gRPC 消息，禁止放进通用命令表。

## 6. Master 主导的统一调度

### 6.1 Worker 只报告空闲和能力

新增协议消息，名称可按当前 protobuf 风格调整：

```proto
message WorkRequest {
  string node_id = 1;
  uint32 slot_index = 2;
  repeated string supported_task_types = 3;
  string request_id = 4;
}
```

Worker 的意思只能是：

```text
“我的这个槽位空闲，并支持这些工作类型，请 Master 决定是否分配。”
```

Worker 不能指定“我要账号注册”或“我要下载”。Master 根据已启动批次、优先级、资源和能力作最终选择。

旧 `SessionRequest.task_type` 保留一个兼容周期，但 Master 应忽略客户端偏好，仅把它用于老版本能力推断和审计。protobuf 字段编号禁止复用。

### 6.2 Worker 能力

节点上线时上报：

```text
图书下载
账号注册
NAS核验
代理检测
人工确认续接
```

能力由 Worker 版本和本地依赖检测生成，不能由网页随意勾选伪造。Master 可另外配置“允许该节点执行的任务类型”，最终能力取二者交集。

### 6.3 调度选择顺序

Master 收到 `WorkRequest` 后：

1. 验证节点身份、批准状态、连接世代和槽位归属。
2. 检查节点在线、未暂停、未维护、未禁用。
3. 检查槽位确实空闲，没有活跃租约。
4. 读取该节点实际支持和管理员允许的任务类型。
5. 从所有运行中的业务队列选取候选。
6. 计算优先级与等待时间。
7. 原子锁定任务、槽位、账号、代理。
8. 创建执行会话和执行记录。
9. 事务提交后下发 `CreateSession`。

建议归一化评分：

```text
effective_priority = batch_priority + waiting_age_bonus + retry_penalty + task_type_weight
```

其中等待时间加成用于防止低优先级永久饥饿；重试任务应有冷却时间，不能在短时间内压住所有新任务。

### 6.4 任务类型占用同一总槽位

第一阶段每台 Worker 仍使用一个总槽位数，例如 5。所有浏览器型任务都消耗槽位：

| 任务类型 | 默认槽位权重 |
|---|---:|
| 图书下载 | 1 |
| 账号注册 | 1 |
| 代理检测 | 1 |
| NAS 核验 | 1 |

建议增加每节点类型上限：

```text
max_account_registration_slots
max_download_slots
```

默认账号注册最多占总槽位的一半，避免大批注册任务阻塞全部下载。总占用永远不能超过 `max_slots`。

### 6.5 启动后的自动下发

“开始批次”接口完成事务后应：

- 更新批次为“执行中”。
- 发布调度唤醒事件。
- 扫描当前在线且有空闲槽位的 Worker。
- 对空闲连接触发工作分配，而不是等很长的下一次轮询。

同时 Worker 仍应周期性发送 `WorkRequest`，用于处理唤醒事件丢失、Master 重启和网络恢复。调度正确性不能依赖一次内存通知。

### 6.6 暂停、恢复和取消

默认语义：

- 暂停：停止分配新任务，正在执行的任务允许完成。
- 恢复：批次回到执行中，立即唤醒调度器。
- 取消：未开始任务改为已取消；已分配但未接收的撤销租约；执行中的任务发送精确取消命令。
- 强制停止：作为独立危险操作，需要二次确认和审计。

取消命令必须携带：

```text
node_id + session_id + execution_id + task_id + stage_version
```

旧 Worker 或僵尸执行不能凭过期任务编号取消或覆盖新执行。

## 7. gRPC 协议增量

### 7.1 Worker 到 Master

在 `WorkerMessage.oneof` 使用新的、未占用字段号新增：

```text
WorkRequest
RegistrationTaskAccepted
RegistrationTaskProgress
RegistrationTaskResult
ManualActionRequired
CommandAccepted
CommandResult
```

账号注册结果至少包括：

```proto
message RegistrationTaskResult {
  string session_id = 1;
  string execution_id = 2;
  string registration_task_id = 3;
  uint32 stage_version = 4;
  uint32 attempt = 5;
  string result = 6;
  string reason = 7;
  bool already_exists = 8;
  bool awaiting_verification = 9;
  string completed_at = 10;
}
```

不得上报密码、完整 Cookie、验证码或页面 HTML。

### 7.2 Master 到 Worker

在 `MasterMessage.oneof` 新增：

```text
AssignRegistrationTask
ContinueManualAction
CancelRegistrationTask
ExecuteCommand
```

账号注册任务消息至少包括：

```proto
message AssignRegistrationTask {
  string session_id = 1;
  string execution_id = 2;
  string registration_task_id = 3;
  uint32 attempt = 4;
  uint32 stage_version = 5;
  string lease_expires_at = 6;
  bool needs_mail_code = 7;
}
```

账号凭据可以继续通过 `CreateSession.account` 下发，不应在多个消息重复传输。

### 7.3 接收确认和可靠性

所有业务任务遵循：

```text
Master 创建执行租约
→ 下发任务
→ Worker 写入本地执行现场
→ Worker 可靠上报 Accepted
→ Master 改为执行中并 ACK
```

如果 Worker 未在 `accept_timeout` 内确认：

- 不立即重复下发给另一节点。
- 等待连接状态和短接收超时。
- 撤销未接受租约并增加 `stage_version`。
- 重新排队，原消息随后到达时因执行编号或版本失效而被拒绝。

结果事件继续使用 Worker 本地 Outbox，Master 以 `event_id` 幂等处理，确认持久化后才返回 `EventAck.accepted=true`。

## 8. Worker 按任务类型执行

### 8.1 重构会话入口

当前 `execute_session_loop` 必须拆分为类型明确的编排器：

```rust
match task_type {
    TaskType::BookDownload => execute_download_session(...),
    TaskType::AccountRegister => execute_registration_session(...),
    TaskType::NasVerify => execute_nas_verify_session(...),
    TaskType::ProxyCheck => execute_proxy_check_session(...),
}
```

禁止所有类型继续落入图书下载默认分支。未知任务类型必须返回“不支持的任务类型”，不能默认为图书下载。

当前 Master 解析失败时 `unwrap_or(TaskType::BookDownload)` 也应移除，改为严格解析并拒绝。

### 8.2 图书下载会话

继续复用现有流程：

```text
代理启动
→ 浏览器会话
→ 账号登录
→ SessionReady
→ 循环领取图书
→ 下载和校验
→ NAS 原子入库
→ 可靠结果上报
→ 会话收尾
```

### 8.3 账号注册会话

注册流程必须不同于登录下载流程：

```text
代理启动
→ 打开不自动登录的浏览器会话
→ SessionReady
→ 接收 AssignRegistrationTask
→ 调用 AutomationEngine::register_account
→ 上报成功、已存在、待验证或失败
→ 关闭浏览器和代理
```

需要调整 `automation-core` 会话创建职责：

- 打开浏览器、配置 Profile 和代理不应强制先登录。
- 图书下载编排器显式执行登录。
- 账号注册编排器直接打开注册页。
- 不要为“待注册账号”执行登录后再注册。

### 8.4 账号注册结果裁决

| Worker 结果 | 注册任务状态 | 账号状态 |
|---|---|---|
| 注册并确认成功 | 已完成 | 已注册 |
| 站点提示已存在 | 已完成或失败，按产品规则固定 | 已禁用或待人工处理 |
| 等待邮箱验证码 | 等待人工确认 | 待验证 |
| 临时网络/站点异常 | 正在重试 | 待注册 |
| 参数或账号不可恢复错误 | 失败 | 登录失败或已禁用 |
| 管理员取消 | 已取消 | 待注册 |

“站点已存在”的具体处置必须固定，不能在不同 Worker 上出现不同结果。建议默认转为“待人工处理”，避免误禁用管理员已有账号。

### 8.5 本地执行现场

Worker 本地 SQLite/Outbox 的执行现场需要支持任务类型和账号注册任务编号：

```text
task_type
book_task_id nullable
registration_task_id nullable
execution_id
session_id
stage_version
stage
result_event_id
```

重启后应能区分：

- 图书下载可继续 NAS 入库或结果重放。
- 账号注册如果已经提交表单但结果不确定，应上报“待确认”，不能盲目再次提交注册表单。

## 9. 人工确认流程

账号注册可能遇到邮箱验证码、图片验证码、条款确认或风控。必须提供完整状态闭环。

### 9.1 Worker 上报

Worker 上报 `ManualActionRequired`：

```text
action_id
task_type
registration_task_id
execution_id
action_type：邮箱验证码、图片验证码、人工确认、风控
prompt 中文说明
expires_at
optional_artifact_id
```

截图不能直接塞入 gRPC JSON 或日志；使用受限大小的证据上传接口或对象存储，生成短期访问地址。

### 9.2 网页处理

新增“待确认事项”页面，管理员可：

- 查看任务、Worker、账号脱敏信息和截图。
- 输入验证码。
- 确认继续。
- 取消任务。

验证码字段必须：

- 不写普通操作日志。
- 不通过 SSE 广播。
- 只传给仍持有相同执行租约的 Worker。
- 使用一次后立即失效。
- 超时后不能被新执行复用。

### 9.3 槽位保留

等待人工确认期间可以短时保留浏览器和槽位，但必须设置上限，例如 10 分钟。超时后：

- Worker 保存必要的非敏感现场摘要。
- 关闭浏览器和代理。
- 释放槽位和资源租约。
- 任务转为“等待人工确认”或“待确认”。

不能无限占用槽位。

## 10. 一本书固定同一代理 IP

### 10.1 数据库字段

为 `book_tasks` 增加：

```text
bound_proxy_id UUID NULL
bound_exit_ip INET/TEXT NULL
proxy_bound_at TIMESTAMPTZ NULL
proxy_change_count INT NOT NULL DEFAULT 0
```

第一次实际执行前，Master 在任务领取事务中绑定代理。Worker `SessionReady.exit_ip` 必须上报实际出口 IP，Master 验证后记录。

### 10.2 重试规则

- 未绑定的图书可以由任意健康代理会话领取。
- 已绑定的图书只能由使用同一 `bound_proxy_id` 的会话领取。
- Worker 上报的出口 IP 与 `bound_exit_ip` 不一致时，不开始下载，任务进入“代理异常”。
- 代理临时不可用时进入冷却等待，不自动换 IP。
- 管理员强制换代理必须二次确认、填写原因、增加 `proxy_change_count` 并审计。

### 10.3 调度适配

由于当前会话先拿代理、再领图书，候选查询必须增加：

```text
bound_proxy_id IS NULL OR bound_proxy_id = current_session.proxy_id
```

调度器还应优先为“有高优先级重试任务但没有匹配会话”的代理创建新会话，否则绑定任务可能长期饥饿。

若 Webshare 产品本身会在同一代理端点轮换 IP，仅保存 `proxy_id` 不足以满足要求。必须使用 Webshare 支持的静态或粘性会话参数，并以 Worker 探测的 `exit_ip` 为最终证据。

## 11. 账号和代理租赁

### 11.1 事务边界

账号注册任务分配时必须在同一事务中完成：

```text
锁定注册任务
锁定空闲槽位
锁定待注册账号
锁定健康代理
创建执行会话
创建执行记录
写入所有租约
提交
```

任意资源不足时整体回滚，不能留下被占用但没有执行的账号或代理。

### 11.2 账号独占

同一账号同一时刻最多属于一个活跃会话。数据库条件更新必须包含：

```text
lease_session_id IS NULL OR lease_expires_at < now()
```

不能依赖应用先 SELECT 再 UPDATE。

### 11.3 敏感凭据

- 数据库继续使用现有 AES-256-GCM 或当前加密实现。
- 加密主密钥仅从运行环境/密钥管理系统加载。
- Rust 明文凭据类型不得实现 `Serialize`。
- Debug 输出必须脱敏或禁止。
- gRPC 只向已批准、mTLS 验证成功且持有当前会话的 Worker 下发。
- Worker 会话结束后删除内存和临时 Profile 中不再需要的敏感数据。

## 12. 云端前端改造

### 12.1 下载批次页

删除当前文本框逐行粘贴作为主要入口，改为文件选择器和两阶段向导：

```text
选择 CSV
→ 设置批次名称、格式、优先级和重试次数
→ 上传并预检
→ 展示有效、重复、已有文件、错误统计
→ 管理员确认创建
→ 选择“仅创建”或“创建并开始”
```

页面必须支持下载：

- 图书 CSV 模板。
- 错误行 CSV。
- 批次结果 CSV。

### 12.2 账号页

保留单个新增，同时增加：

- “批量导入账号”按钮。
- 导入模式选择：待注册、已注册。
- 文件预检和脱敏预览。
- “为待注册账号创建注册批次”。
- 账号详情中的注册任务历史。

密码输入和上传页面不得把明文保存到 LocalStorage、URL、SSE 或错误追踪工具。

### 12.3 账号注册批次页

新增：

```text
/account-registration-batches
/account-registration-batches/:id
```

功能：

- 创建、开始、暂停、恢复、取消。
- 查看总数、待处理、执行中、待人工确认、成功、失败。
- 查看分配 Worker、代理出口 IP、尝试次数和最后错误。
- 单任务重试和取消。
- 跳转待确认事项。

### 12.4 统一任务页

现有任务页增加任务类型筛选。可以使用组合查询接口展示多种任务，但后端资源详情仍应使用类型化 API，避免用一个无限 JSON 接口承载所有业务。

### 12.5 操作反馈

所有写操作必须：

- 有加载态和防重复提交。
- 成功显示中文 Toast。
- 失败显示中文错误和 `request_id`。
- 409 后刷新当前资源。
- 422 显示到具体字段或错误行。
- 危险操作二次确认。
- SSE 只用于刷新和展示，不能作为唯一成功依据。

## 13. HTTP API 清单

### 13.1 导入

```text
POST /api/imports/books/preview
POST /api/imports/books/commit
POST /api/imports/accounts/preview
POST /api/imports/accounts/commit
DELETE /api/imports/{id}
GET /api/imports/{id}/errors.csv
```

### 13.2 下载批次

继续使用并补充幂等控制：

```text
GET  /api/batches
GET  /api/batches/{id}
POST /api/batches/{id}/start
POST /api/batches/{id}/pause
POST /api/batches/{id}/resume
POST /api/batches/{id}/cancel
PATCH /api/batches/{id}/priority
```

旧 `POST /api/batches/import` 可保留一个兼容周期，但新前端必须使用文件预检/提交接口。

### 13.3 账号注册批次

```text
GET  /api/account-registration-batches
POST /api/account-registration-batches
GET  /api/account-registration-batches/{id}
POST /api/account-registration-batches/{id}/start
POST /api/account-registration-batches/{id}/pause
POST /api/account-registration-batches/{id}/resume
POST /api/account-registration-batches/{id}/cancel
PATCH /api/account-registration-batches/{id}/priority
GET  /api/account-registration-batches/{id}/tasks
```

### 13.4 账号注册任务

```text
GET  /api/account-registration-tasks
GET  /api/account-registration-tasks/{id}
POST /api/account-registration-tasks/{id}/retry
POST /api/account-registration-tasks/{id}/cancel
```

### 13.5 人工确认

```text
GET  /api/manual-actions
GET  /api/manual-actions/{id}
POST /api/manual-actions/{id}/resolve
POST /api/manual-actions/{id}/cancel
```

所有写接口必须由服务端执行角色校验和审计，不能只依赖前端隐藏按钮。

## 14. 权限模型

| 操作 | 只读用户 | 任务管理员 | 超级管理员 |
|---|---:|---:|---:|
| 查看批次和任务 | 是 | 是 | 是 |
| 上传图书 CSV | 否 | 是 | 是 |
| 启停下载批次 | 否 | 是 | 是 |
| 上传账号文件 | 否 | 否 | 是 |
| 创建账号注册批次 | 否 | 否 | 是 |
| 查看脱敏账号 | 是 | 是 | 是 |
| 查看或修改敏感凭据 | 否 | 否 | 受控 |
| 输入人工验证码 | 否 | 是或按策略 | 是 |
| 强制更换图书代理 | 否 | 否 | 是 |
| 调整 Worker 槽位和能力 | 否 | 否 | 是 |

账号文件上传和注册批次默认只允许超级管理员，因为它们处理凭据并可能触发第三方站点操作。

## 15. 中文状态和字典

新增状态必须定义在 `platform-domain`，数据库 CHECK、REST、gRPC 展示值和前端字典保持一致。

禁止：

- 在前端组件中散落英文转中文映射。
- 遇到未知状态时回落为“正常”。
- 使用 `unwrap_or(图书下载)` 处理未知任务类型。
- 数据库保存 `Pending`、`Running` 等英文业务值。

技术协议枚举名、HTTP 方法、文件格式和版本号可以使用英文标识；管理员看到的业务状态必须是中文。

## 16. 文件级实施清单

### 16.1 Master 新增

建议新增或按现有模块合并：

```text
cloud-platform/crates/master-server/migrations/0006_business_tasks.sql
cloud-platform/crates/master-server/src/api/imports.rs
cloud-platform/crates/master-server/src/api/account_registration_batches.rs
cloud-platform/crates/master-server/src/api/account_registration_tasks.rs
cloud-platform/crates/master-server/src/api/manual_actions.rs
cloud-platform/crates/master-server/src/store/import_job.rs
cloud-platform/crates/master-server/src/store/account_registration.rs
cloud-platform/crates/master-server/src/scheduler/select_work.rs
```

### 16.2 Master 修改

```text
cloud-platform/crates/master-server/src/api/mod.rs
cloud-platform/crates/master-server/src/grpc/inbound.rs
cloud-platform/crates/master-server/src/grpc/convert.rs
cloud-platform/crates/master-server/src/scheduler/allocate.rs
cloud-platform/crates/master-server/src/scheduler/reaper.rs
cloud-platform/crates/master-server/src/store/task.rs
cloud-platform/crates/master-server/src/store/session.rs
cloud-platform/crates/master-server/src/models.rs
cloud-platform/crates/master-server/src/config.rs
cloud-platform/crates/master-server/src/events.rs
```

### 16.3 协议和领域

```text
cloud-platform/crates/platform-proto/proto/worker.proto
cloud-platform/crates/platform-proto/src/display.rs
cloud-platform/crates/platform-domain/src/enums.rs
cloud-platform/crates/platform-domain/src/transitions.rs
cloud-platform/crates/platform-domain/src/failure.rs
```

### 16.4 Worker 和自动化

```text
cloud-platform/crates/worker-agent/src/slot.rs
cloud-platform/crates/worker-agent/src/client.rs
cloud-platform/crates/worker-agent/src/outbox.rs
cloud-platform/crates/worker-agent/src/bus.rs
cloud-platform/crates/automation-core/src/engine.rs
cloud-platform/crates/automation-core/src/types.rs
cloud-platform/crates/automation-core/src/real.rs
cloud-platform/crates/automation-core/src/simulated.rs
```

### 16.5 前端

```text
cloud-platform/admin-web/src/pages/BatchesPage.tsx
cloud-platform/admin-web/src/pages/AccountsPage.tsx
cloud-platform/admin-web/src/pages/AccountRegistrationBatchesPage.tsx
cloud-platform/admin-web/src/pages/AccountRegistrationBatchDetailPage.tsx
cloud-platform/admin-web/src/pages/ManualActionsPage.tsx
cloud-platform/admin-web/src/pages/TasksPage.tsx
cloud-platform/admin-web/src/lib/api.ts
cloud-platform/admin-web/src/lib/types.ts
cloud-platform/admin-web/src/App.tsx
```

### 16.6 禁止修改

```text
frontend/**
```

## 17. 分阶段实施顺序

### 阶段 0：建立当前行为测试

- 固定原客户端图书 CSV 和账号文件兼容样例。
- 固定当前图书调度、租约、Outbox、NAS 入库测试。
- 记录 `git diff -- frontend` 基线。

完成条件：已有能力有自动测试保护。

### 阶段 1：真实 CSV 预检和提交

- 实现 `import_jobs`。
- 增加 multipart 流式上传。
- 复用标准 CSV 解析器。
- 增加预检、错误报告、幂等提交和过期清理。
- 前端批次页改成文件上传向导。

完成条件：管理员可上传原客户端 CSV 创建批次，文本粘贴不再是主要入口。

### 阶段 2：账号导入与注册任务模型

- 实现账号文件两种导入模式。
- 新增账号注册批次和任务表。
- 扩展执行记录。
- 实现注册批次 REST API 和页面。

完成条件：待注册账号可以形成可管理、可追踪的任务队列。

### 阶段 3：Master 主导工作选择

- 新增 `WorkRequest`。
- Worker 上报空闲槽位和能力。
- Master 跨业务队列选择任务类型。
- 启动和恢复批次后立即唤醒在线 Worker。
- 保留旧协议兼容期。

完成条件：Worker 不能自行决定任务类型，网页启动的任务会自动进入调度。

### 阶段 4：Worker 多类型编排

- 拆分图书下载和账号注册会话。
- 注册会话不先执行登录。
- 接入现有 `register_account`。
- 增加注册 Accepted、Progress、Result 和本地 Outbox。
- 增加严格未知类型处理。

完成条件：真实 Worker 能接收一个账号注册任务并可靠回传结果。

### 阶段 5：人工确认和异常恢复

- 增加待确认事项表、协议和页面。
- 验证码一次性下发。
- 增加超时释放槽位。
- 覆盖 Worker 断线、Master 重启和结果重放。

完成条件：验证码或结果不确定不会把账号错误标记为已注册。

### 阶段 6：图书代理任务级绑定

- 增加任务级代理和出口 IP 字段。
- 修改候选查询和会话创建策略。
- 增加强制换代理审计。
- 验证 Webshare 粘性会话配置。

完成条件：同一本书跨重试仍保持同一出口 IP。

### 阶段 7：回归、部署和清理兼容代码

- 运行全量 Rust、前端、协议和 E2E 测试。
- 灰度升级 Master，再升级 Worker。
- 观察旧 `SessionRequest.task_type` 使用量。
- 下一兼容版本删除旧 JSON 导入主入口和 Worker 指定任务类型逻辑。

完成条件：全部管理动作在网页可用，原桌面客户端未受影响。

## 18. 测试要求

### 18.1 CSV 解析测试

- 单列图书 CSV。
- 四列图书 CSV。
- 有表头和无表头。
- UTF-8 BOM。
- 字段中带英文逗号并使用引号。
- 空白行。
- 重复行。
- 非法 ISBN。
- 超大文件和超多行。
- 文件中途断开。
- 相同预检令牌重复提交。
- 过期令牌和其他管理员盗用令牌。
- 错误 CSV 的公式注入转义。
- 账号兼容 `email----password`。
- 标准账号 CSV。
- 账号密码不出现在预览、日志和响应错误中。

### 18.2 调度测试

- 批次待开始时不下发。
- 点击开始后自动唤醒在线 Worker。
- 无在线 Worker 时任务保持待处理，Worker 后上线后自动领取。
- Worker 在线但无槽位时不超分配。
- NAS 异常节点不接收下载任务，但可按能力执行非 NAS 任务。
- Worker 不支持账号注册时不接收注册任务。
- Worker 伪造任务类型请求不影响 Master 选择。
- 下载和注册任务共享总槽位上限。
- 注册任务不会占满超过配置的注册槽位上限。
- 低优先级任务随等待时间获得执行机会。
- 暂停后不分配新任务。
- 恢复后立即重新调度。
- 取消和接收确认并发时只有一个最终结果。

### 18.3 租约和幂等测试

- 同一任务不能被两个 Worker 同时有效执行。
- 未确认下发超时后可安全回收。
- 旧执行迟到 Accepted 被拒绝。
- 旧执行迟到 Result 不覆盖新执行。
- 重复结果事件只处理一次。
- Master 返回 ACK 前崩溃，Worker 重放后仍只处理一次。
- Worker 重启后恢复本地注册任务现场。
- 账号、代理和槽位在事务失败时全部回滚。

### 18.4 账号注册测试

- 成功注册更新任务和账号状态。
- 站点已存在进入固定处置状态。
- 临时失败按退避重试。
- 不可恢复失败不无限重试。
- 邮箱验证码进入等待人工确认。
- 人工确认超时释放槽位。
- 验证码不能被另一执行使用。
- 注册任务取消后 Worker 不再提交表单。
- 注册引擎没有走图书下载登录路径。

### 18.5 代理固定测试

- 图书首次执行原子绑定代理。
- 同任务重试只能匹配相同代理。
- 出口 IP 变化时下载不开始。
- 代理冷却时任务等待而不静默换 IP。
- 管理员强制换 IP 有二次确认和审计。
- 相同图书跨批次仍复用同一全局任务和代理绑定。

### 18.6 NAS 测试

- 文件先写临时名再原子提交。
- 已存在相同哈希文件判定幂等成功。
- 已存在不同文件不覆盖。
- NAS 断开、空间不足和权限失败正确重试或待确认。
- Master 完成状态必须有 NAS 文件证据。

### 18.7 前端测试

- 文件选择、上传进度、预检和确认。
- 错误行下载。
- 只读用户无法上传和启动。
- 账号预览脱敏。
- 批次开始、暂停、恢复、取消接口正确。
- 账号注册批次页面状态统计正确。
- 人工确认操作失败时有明确反馈。
- SSE 断线后页面最终可通过查询刷新恢复一致。

## 19. 端到端验收场景

### 场景 A：上传图书 CSV 自动下载

1. 管理员上传原客户端可用的图书 CSV。
2. 网页展示正确预检统计。
3. 管理员创建并启动批次。
4. Master 自动分配给在线 Worker，不进行人工节点选择。
5. Worker 接收、确认、执行和回报进度。
6. 文件直接写入 NAS。
7. 网页显示已完成和文件相对路径。

### 场景 B：启动时没有 Worker

1. 创建并启动批次时所有 Worker 离线。
2. 任务保持待处理，不失败、不丢失。
3. 一台已批准 Worker 上线并报告空闲槽位。
4. Master 自动下发任务。

### 场景 C：账号批量注册

1. 超级管理员上传账号文件，选择“待注册”。
2. 预检页面不显示密码。
3. 创建账号注册批次并启动。
4. Master 选择支持账号注册的在线 Worker。
5. Worker 调用注册引擎而非下载流程。
6. 成功账号转为“已注册”，失败账号显示中文原因。

### 场景 D：人工验证码

1. Worker 注册账号时遇到邮箱验证码。
2. 网页出现“待确认事项”。
3. 管理员输入验证码。
4. Master 只下发给持有原执行租约的 Worker。
5. Worker 完成注册并可靠回报。

### 场景 E：断线与重试固定 IP

1. Worker 下载一本书期间断线。
2. 租约保护期内不重复分配。
3. 超时后任务重新排队。
4. 重试仍使用原 Webshare 代理和出口 IP。
5. 旧 Worker 的迟到结果不能覆盖新执行。

## 20. 部署与兼容

推荐上线顺序：

1. 备份 PostgreSQL，记录在线 Worker 版本。
2. 部署兼容新旧 protobuf 的 Master。
3. 执行只新增表和字段的数据库迁移。
4. 部署支持文件上传的新 `admin-web`。
5. 升级一台 Worker 验证 `WorkRequest` 和账号注册。
6. 分批升级其余 Worker。
7. 观察旧协议使用量至少一个发布周期。
8. 再删除旧 JSON 导入主入口和 Worker 指定任务类型逻辑。

回滚要求：

- 首次迁移不删除 `book_tasks`、旧协议字段或现有批次接口。
- 新 Worker 必须能识别 Master 暂不支持新消息并给出中文提示。
- 新 Master 在兼容期仍能服务旧 Worker 的图书下载。
- 回滚不能通过清空任务、会话、账号或证书表实现。

## 21. 安全威胁模型摘要

| 边界 | 主要风险 | 必须控制 |
|---|---|---|
| 浏览器上传文件 | 超大文件、恶意 CSV、凭据泄露 | 流式限制、严格解析、临时目录、脱敏、最短保留 |
| 管理员 API | 越权启动、重复提交、CSRF | 服务端 RBAC、幂等键、CSRF、审计 |
| Master 到 Worker | 伪造节点、凭据泄露 | mTLS、节点令牌、批准状态、会话归属 |
| Worker 结果 | 重放、篡改、迟到覆盖 | event_id、execution_id、stage_version、租约校验 |
| 数据库 | 密码泄露、任务并发 | 应用层加密、参数化 SQL、行锁和约束 |
| NAS | 路径穿越、覆盖已有文件 | 相对路径校验、根目录约束、非覆盖原子提交 |
| Webshare | 代理泄露、IP 漂移 | 凭据最小下发、任务级绑定、出口 IP 核验 |

所有实现必须把滥用场景加入测试：上传凭据文件后越权查看、伪造 Worker 请求注册任务、重复提交验证码、过期 Worker 回传结果、CSV 导出公式注入等。

发布前必须运行前端锁文件对应包管理器的原生依赖审计，评估可达的高危和严重漏洞；不得自动执行强制跨版本修复。

## 22. 禁止事项

- 禁止修改 `frontend/**`。
- 禁止继续把文本框粘贴伪装成 CSV 文件上传。
- 禁止前端自行用字符串切分解析 CSV。
- 禁止上传即直接创建批次而没有预检和幂等提交。
- 禁止长期保存账号原始明文文件。
- 禁止日志、SSE、错误响应或前端状态包含密码和验证码。
- 禁止由 Worker 决定任务类型。
- 禁止未知任务类型默认变成图书下载。
- 禁止账号注册会话进入图书下载循环。
- 禁止只改账号状态而没有注册任务记录。
- 禁止不持久化就直接向 Worker 发网页命令。
- 禁止“已下发”直接显示为“执行中”或“已完成”。
- 禁止租约过期后接受旧执行结果覆盖新执行。
- 禁止只在会话级固定代理而不在图书任务级绑定。
- 禁止一本书失败后静默更换出口 IP。
- 禁止 Master 接收或中转图书文件。
- 禁止数据库和管理员页面出现英文业务状态。

## 23. 最终验收清单

- [ ] 云端下载批次通过真实 CSV 文件创建。
- [ ] CSV 格式与原客户端兼容。
- [ ] 上传具备预检、错误报告、确认和幂等提交。
- [ ] 图书全局去重仍由数据库保证。
- [ ] 批次启动后自动下发给符合条件的在线 Worker。
- [ ] 没有在线 Worker 时任务不会失败或丢失。
- [ ] Worker 只报告空闲槽位，Master 决定工作类型。
- [ ] 下载、账号注册、NAS 核验和代理检测共享统一槽位调度规则。
- [ ] 账号支持单个新增和文件批量导入。
- [ ] 待注册账号能创建账号注册批次和独立任务记录。
- [ ] Worker 账号注册路径实际调用 `register_account`。
- [ ] 注册任务有 Accepted、Progress、Result 和可靠 Outbox。
- [ ] 人工验证码有网页闭环和超时释放机制。
- [ ] 所有凭据均加密、脱敏并按最小范围下发。
- [ ] 任务租约、执行编号和阶段版本阻止重复执行和迟到覆盖。
- [ ] 一本书跨重试仍使用同一 Webshare 出口 IP。
- [ ] 文件只由 Worker 经局域网写入 NAS。
- [ ] NAS 入库保持临时文件、校验和非覆盖原子提交。
- [ ] 所有管理员可见业务状态和提示为中文。
- [ ] 前端所有操作有成功、失败、权限和冲突反馈。
- [ ] Rust、前端、协议、数据库和 E2E 测试全部通过。
- [ ] `git diff -- frontend` 证明原桌面客户端未受影响。

## 24. 实现智能体交付报告

实现完成后必须提交：

```text
1. 实际修改文件清单
2. CSV 兼容格式和限制
3. 数据库迁移及回滚说明
4. REST API 请求/响应清单
5. protobuf 新字段号及兼容说明
6. Master 调度选择时序
7. Worker 下载与账号注册两条执行时序
8. 账号、代理、槽位和任务的事务边界
9. 人工确认与超时处理
10. 任务级代理/IP 固定实现
11. 安全控制和敏感字段清单
12. 已运行的测试命令和结果
13. 未完成、跳过或使用 mock 的项目
14. git diff -- frontend 的结果
```

不得只回复“已实现”。任何兼容代码、临时限制、模拟执行或未验证的真实站点流程都必须明确披露。
