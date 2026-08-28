# Worker 代理与浏览器执行运行时修复实施方案（V1）

> 日期：2026-08-28
>
> 适用范围：`cloud-platform` Master、Worker Agent、Automation Core 及 Worker 跨平台发布包
>
> 目标：恢复原客户端已经验证稳定的 sticky-proxy 行为，消除浏览器转圈、空白页、gRPC 心跳中断、代理未验证即使用及重复会话问题。

## 1. 结论与目标

当前故障不是 Worker 所在机器无法访问网络，也不是 Webshare 本身必然不可用，而是
cloud-platform 重写代理与并发执行链路后产生的功能回退：

1. Webshare 代理尚未经过 Worker 实测，就进入「可用」状态并被下载会话领取；
2. Worker 自制 HTTP/CONNECT 转发与 gRPC 心跳共用 Tokio 运行时；
3. `rust_drission`/Chromium 的同步调用直接运行在 Tokio 工作线程；
4. 多槽并发时，同步导航占满线程，本地代理无法转发，浏览器持续转圈，gRPC 心跳随之中断；
5. Worker 尚未实现完整的「代理检测」会话，却由 Master 调度此类会话；
6. 断线重连后可能在同一节点槽位留下多个未结束会话。

本方案不继续修补当前自制代理转发，而是复用原客户端的成熟思路：

- GOST 作为独立进程承担代理协议与认证转发；
- Chromium 同步操作由每槽独立 OS 线程串行执行；
- Tokio 只处理 gRPC、调度消息、状态机和异步 I/O；
- 代理必须检测成功、记录出口 IP 和检测时间后才能用于生产下载；
- 浏览器导航和文件 HTTP 下载必须使用同一会话代理；
- 发布、打包和生产更新全部经 GitHub Actions 完成。

## 2. 生产故障证据

2026-08-28 生产检查得到以下事实：

- Webshare 共 100 条，其中 94 条「可用」、5 条「已占用」、1 条「已停用」；
- 所有代理的 `last_checked_at`、`latency_ms`、`exit_ip` 均为空，成功次数均为 0；
- 最近下载任务出现目标站点下载 URL 网络超时；
- Worker 在多浏览器运行期间周期性出现 `h2 protocol error` 并离线；
- 5 个槽位曾出现 6 个 `ended_at IS NULL` 的执行会话；
- 最近三小时没有成功下载，结果以「跳过」「结果不确定」「可重试失败」为主；
- 浏览器现场表现为：部分窗口停在新标签页，部分窗口访问站点持续转圈。

这些现象符合「浏览器等待本地代理、本地代理等待 Tokio 调度、Tokio 被同步浏览器调用占满」的循环阻塞模型。

## 3. 原客户端与 cloud-platform 的关键差异

| 能力 | 原客户端 | 当前 cloud-platform | 本方案 |
| --- | --- | --- | --- |
| 上游代理转发 | 独立 GOST 进程 | Worker 内自制 Hyper 转发 | 独立 GOST 进程 |
| 浏览器执行 | 独立 OS 线程 | `tokio::spawn` 内执行同步浏览器调用 | 每槽独立 OS 线程 |
| 多 Worker 启动 | 2 秒错峰 | 基本同时启动 | 可配置错峰，默认 2 秒 |
| 代理准入 | listener/拓扑验证、故障观察 | 未检测即可分配 | HTTPS、认证、出口 IP、目标站点四项检测 |
| 故障控制 | 分类、熔断、冷却、轮换 | 主要依赖租约超时 | 分类、熔断、冷却、重新检测 |
| 下载流量 | 与浏览器共用固定代理 | 尚未形成可验证闭环 | 浏览器与 HTTP 下载强制共用会话代理 |
| 心跳隔离 | 浏览器线程不影响控制面 | 浏览器可阻塞控制面 | 控制面与浏览器/代理进程隔离 |

原客户端可参考实现：

- `../../src/proxy.rs`
- `../../src/browser/worker.rs`
- `../../scripts/start_proxy.sh`
- `../../scripts/proxy_pool/validate_topology.py`

## 4. 目标架构

```text
Master
  ├─ Webshare 同步：只写入候选代理，不直接判定可用
  ├─ 代理检测调度：优先检测未验证/过期代理
  ├─ 下载调度：只分配验证有效且未过期的代理
  └─ gRPC 控制面：会话、任务、心跳、结果、出口 IP
             │ mTLS
             ▼
Worker Agent（Tokio 控制面）
  ├─ LinkRuntime：gRPC、心跳、重连、对账
  ├─ ProxyRuntime：GOST 生命周期、代理验证、出口 IP
  └─ SlotRuntime × N
       └─ BrowserExecutor（独立 OS 线程）
            ├─ Chromium / rust_drission
            └─ HTTP 下载命令
                    │
                    ▼
              本槽 GOST 进程
                    │
                    ▼
              固定 Webshare 上游
```

每个下载会话保持「一个槽位 + 一个账号 + 一条代理 + 一个出口 IP」。会话结束前不得静默切换代理；代理失败时结束当前会话，冷却代理并重新编排。

## 5. 模块设计

### 5.1 `ProxyRuntime` 深模块

在 `crates/worker-agent/src/proxy_runtime/` 建立新的模块，替换调用方直接操作
`LocalProxyServer` 的方式。

建议外部接口保持最小：

```rust
pub struct SessionProxySpec {
    pub session_id: Uuid,
    pub slot_index: u32,
    pub upstream: ProxyCredential,
}

pub struct VerifiedProxySession {
    pub browser_proxy_url: Url,
    pub exit_ip: IpAddr,
    pub latency: Duration,
}

pub trait ProxyRuntime: Send + Sync {
    async fn start_verified(
        &self,
        spec: SessionProxySpec,
    ) -> Result<ProxySessionHandle, ProxyRuntimeError>;
}
```

`ProxySessionHandle` 内部拥有 GOST 子进程、临时配置和监听端口；Drop/显式关闭时必须回收全部资源。调用方不需要了解 GOST 参数、凭据格式、监听探测或退出流程。

生产使用 `GostProxyRuntime` adapter；测试使用 `FakeProxyRuntime` adapter。复杂度集中在模块内部，测试通过同一接口观察「验证成功、分类失败、退出回收」结果。

实现要求：

1. 每个槽位使用固定端口，默认 `19001 + slot_index`；
2. 先生成 GOST 配置，再启动进程并等待 listener 就绪；
3. 代理凭据不得出现在命令行、进程标题、普通日志或错误响应中；
4. 临时配置仅当前用户可读，Unix 权限 `0600`，Windows 使用当前用户 ACL；
5. 会话结束立即删除临时配置并终止 GOST；
6. GOST 异常退出时立即终止会话，不允许回退直连；
7. Worker Release 固定 GOST 版本、校验 SHA-256，并记录第三方许可证。

### 5.2 `BrowserExecutor` 深模块

在 `crates/automation-core/src/browser_executor/` 建立浏览器命令执行模块。

每个槽位创建一个独立 OS 线程，`ChromiumPage` 只能在该线程内创建、使用和销毁。
Tokio 侧通过有界 channel 发送命令，通过 oneshot 接收结果：

```rust
pub enum BrowserCommand {
    OpenSession(OpenSessionSpec),
    Login(AccountCredential),
    Search(BookTarget),
    ReadQuota,
    ExportCookies,
    CloseSession,
}

pub trait BrowserExecutor: Send + Sync {
    async fn execute(&self, command: BrowserCommand) -> Result<BrowserResult, BrowserError>;
}
```

约束：

- 不得在 Tokio 工作线程直接调用 `ChromiumPage::new`、`page.get`、CDP 同步方法或 `std::thread::sleep`；
- 一个槽位内命令串行执行，避免同一 Page 被并发访问；
- channel 必须有界，防止 Master 重复指令造成内存堆积；
- 命令必须有超时和取消；超时后关闭浏览器并结束会话；
- 浏览器关闭或线程 panic 必须转换成结构化结果，不能拖垮 Worker 主进程。

### 5.3 `LinkRuntime` 控制面

现有 gRPC 客户端继续使用 Tokio，但只承担：

- 心跳与槽位状态；
- Master 指令接收；
- 可靠 Outbox 重放；
- 断线重连与会话对账；
- 向 `ProxyRuntime`、`BrowserExecutor` 下发有界命令。

心跳不得执行浏览器、NAS 深度扫描或第三方网络请求。耗时指标采集在独立任务中缓存，心跳只读取最近快照。

## 6. 代理验证与状态机

### 6.1 Webshare 同步

新同步或凭据发生变化的代理不能直接设为「可用」。在不扩展状态枚举的前提下，先设为「异常」并清空：

- `last_checked_at`
- `latency_ms`
- `exit_ip`
- 当前租约字段

已有且验证仍在有效期内的代理可保留状态。禁止同步任务覆盖正在使用中的租约状态。

### 6.2 实现真正的「代理检测」会话

当前 `TaskType::ProxyCheck` 已存在于 Master 调度，但 Worker 会话执行器尚未完整支持。必须补齐该分支，且不启动浏览器：

1. 启动本槽 GOST；
2. 检测本地 listener；
3. 通过代理完成 HTTPS CONNECT 与证书校验；
4. 访问固定出口 IP 检测端点并解析公网 IP；
5. 访问 `https://zh.loves.works/` 的轻量请求；
6. 上报耗时、出口 IP 和结构化结果；
7. 关闭 GOST 并释放会话。

检测 URL 必须是程序固定 allowlist，不能接收管理端任意 URL，防止 SSRF。禁止跟随重定向到非 allowlist 域名。

### 6.3 下载代理准入

下载调度只能选择同时满足以下条件的代理：

```sql
status = '可用'
AND last_checked_at >= now() - interval '10 minutes'
AND exit_ip IS NOT NULL
AND lease_session_id IS NULL
AND (cooldown_until IS NULL OR cooldown_until <= now())
```

若没有符合条件的代理，返回「没有已验证代理」，不得启动无代理浏览器，也不得回退直连。

### 6.4 失败分类

| 失败 | 状态处理 | 冷却建议 |
| --- | --- | --- |
| 认证失败/407 | 异常，等待重新同步凭据 | 30 分钟 |
| 付款/套餐限制 | 已停用并告警 | 人工恢复 |
| CONNECT 超时 | 异常 | 5 分钟 |
| 连接重置/EOF | 连续 2 次后异常 | 5 分钟 |
| 目标站点限流 | 冷却中 | 10 分钟 |
| GOST 进程退出 | 当前会话失败，代理待复检 | 5 分钟 |

故障计数按代理记录，不能把代理故障计入图书业务失败次数。

## 7. 浏览器与 HTTP 下载的一致代理

浏览器登录、搜索、获取下载令牌和文件 HTTP 下载必须使用同一
`ProxySessionHandle`：

- Chromium 配置 `browser_proxy_url`；
- HTTP 下载客户端配置同一个本地 GOST URL；
- HTTP 下载复用浏览器导出的 Cookie、User-Agent 和必要请求头；
- 下载前再次确认代理句柄仍存活；
- 下载中 GOST 退出时中止文件写入并保留可恢复现场；
- 禁止 HTTP 下载客户端在代理失败时自动回退系统直连。

`SessionReady.exit_ip` 必须填入代理实测出口 IP。Master 将该 IP 固定到会话与任务证据，最终入库时校验任务使用的代理和出口 IP 未发生变化。

## 8. 并发、背压与会话一致性

### 8.1 槽位并发

实际并发取以下最小值：

```text
管理员批准槽位
已验证代理数
可用账号数
BrowserExecutor 线程容量
NAS 可用容量
```

Worker 启动槽位时默认错峰 2 秒。第一阶段生产强制单槽；通过灰度验收后依次提升至 2、3、5 槽。

### 8.2 数据库唯一性

增加迁移，保证一个节点槽位最多只有一个未结束会话：

```sql
CREATE UNIQUE INDEX ...
ON execution_sessions (node_id, slot_index)
WHERE ended_at IS NULL;
```

迁移前先生成冲突报告，保留最新且与 Worker 对账一致的会话，其余会话以明确原因结束。不得直接删除历史记录。

### 8.3 重连对账

Worker 重连上报本地 Outbox 与内存合并后的活跃执行。Master 按以下规则处理：

1. 双方一致：恢复会话并续租；
2. Master 有、Worker 无：结束会话，任务按阶段回队列或转待确认；
3. Worker 有、Master 无：下发停止并保留本地证据；
4. 同槽多个会话：只允许一个通过唯一性检查，其余结束；
5. 对账完成前不得分配新会话。

## 9. 安全与威胁模型

### 9.1 信任流

```text
Webshare API → Master 数据库 → mTLS gRPC → Worker 内存 → GOST 私有配置
```

资产包括 Webshare 用户名/密码、站点账号、会话 Cookie、下载令牌、客户端证书和 NAS 文件。

### 9.2 强制控制

- 代理密码继续加密存储，只有已批准 Worker 的 mTLS 会话可接收明文；
- 所有结构化日志对代理 URL、用户名、密码、Cookie、token 做字段级脱敏；
- GOST 配置不得写入普通配置目录或随诊断包上传；
- 子进程必须使用绝对可验证路径，禁止通过 PATH 搜索未知 `gost`；
- 固定 GOST 版本与 SHA-256，GitHub Actions 校验后才打包；
- 出口检测和目标探测只允许固定 HTTPS 域名，拒绝私网、loopback、链路本地和云元数据地址；
- 诊断接口只返回代理 ID、状态、延迟、出口 IP，不返回主机、用户名和密码；
- 代理认证失败日志只记录错误分类，不记录响应中的敏感头；
- Worker 退出时清理代理配置；异常退出后的陈旧配置在下次启动时安全清理。

## 10. 分阶段实施

### 阶段 0：生产止损

- 使用全局暂停停止新下载分配；
- 保留现有失败、待确认和事件记录，不批量删除；
- 将当前 Worker 批准槽位临时降为 1；
- 记录当前 Master 镜像 SHA、Worker Release 版本和回滚点。

### 阶段 1：代理运行时

- 引入并固定 GOST 跨平台二进制；
- 实现 `ProxyRuntime`、进程回收、私有配置和 listener 探测；
- 实现代理 HTTPS、出口 IP、目标站点检测；
- 补齐 ProxyCheck Worker 分支和 Master 结果入库；
- 下载调度改为只选择新鲜验证代理。

### 阶段 2：浏览器执行隔离

- 实现每槽 `BrowserExecutor` OS 线程；
- 将 `ChromiumPage` 创建、导航、DOM、CDP、Cookie 导出、关闭全部迁移进该线程；
- 删除 Tokio 任务中直接调用同步浏览器方法的路径；
- 加入有界队列、超时、取消与 panic 隔离。

### 阶段 3：下载与状态一致性

- HTTP 下载复用同一代理句柄；
- 上报并持久化出口 IP；
- 增加活跃会话唯一索引与重连对账修复；
- 代理故障与业务失败分开计数；
- 清理旧的自制 `proxy_forward` 实现及其内部测试，避免双实现长期并存。

### 阶段 4：管理端与可观测性

- 节点页展示心跳间隔、活跃浏览器线程、GOST 进程数；
- 代理页展示最后检测时间、延迟、出口 IP、冷却原因；
- 下载任务展示会话代理 ID、出口 IP、浏览器阶段和 HTTP 下载阶段；
- 增加「未验证代理被拒绝」「代理运行时退出」「心跳超期」告警；
- 禁止 UI 展示或返回任何代理凭据。

## 11. 文件级改造清单

| 路径 | 改造 |
| --- | --- |
| `crates/worker-agent/src/proxy_forward.rs` | 被 `proxy_runtime/` 替换，验收后删除 |
| `crates/worker-agent/src/slot.rs` | 通过两个深模块执行会话，补齐 ProxyCheck |
| `crates/worker-agent/src/client.rs` | 心跳只读取快照，重连对账期间禁止领新会话 |
| `crates/automation-core/src/real.rs` | 移除 Tokio 线程上的同步 Chromium 调用 |
| `crates/automation-core/src/browser_executor/` | 新增每槽 OS 线程和命令接口 |
| `crates/master-server/src/scheduler/allocate.rs` | 仅选择新鲜验证代理 |
| `crates/master-server/src/store/resource.rs` | 代理检测结果、冷却与错误分类原子更新 |
| `crates/master-server/src/grpc/inbound.rs` | 处理出口 IP、检测结果和严格重连对账 |
| `crates/master-server/migrations/0015_*.sql` | 活跃槽位唯一索引及必要数据清理 |
| `.github/workflows/release-worker.yml` | 固定并校验 GOST，打入四平台 Worker 包 |
| `deploy/worker-package-templates/` | 增加 GOST 文件、许可证和运行说明 |

## 12. 测试策略

接口就是测试面，不对 GOST 内部实现或 Chromium 私有状态做脆弱断言。

### 12.1 单元测试

- ProxyRuntime：启动、listener 就绪、认证失败、超时、子进程退出、Drop 回收；
- BrowserExecutor：命令串行、有界背压、超时、取消、线程 panic；
- 代理状态机：验证、冷却、重新检测、过期拒绝；
- 重连对账：双方一致、单边存在、同槽冲突；
- 日志脱敏：任何错误路径均不出现凭据、Cookie 或 token。

### 12.2 本地集成测试

使用测试 TLS 站点和可控上游代理 adapter，覆盖：

- CONNECT、Basic Auth、HTTPS 证书校验；
- 407、429、连接重置、慢响应、GOST 崩溃；
- 浏览器与 HTTP 下载出口 IP 完全一致；
- 5 个慢浏览器任务持续 10 分钟，心跳最大间隔不超过 20 秒；
- gRPC 断线重连后没有重复活跃会话；
- NAS 写入仍保持 no-replace 与哈希验证。

### 12.3 Windows 实机浸泡测试

使用与生产一致的 Windows Worker：

1. 单槽运行 30 分钟；
2. 两槽运行 1 小时；
3. 五槽运行至少 2 小时；
4. 中途分别断开公网、终止 GOST、终止浏览器、重启 Worker；
5. 验证自动恢复、状态一致、无重复会话、无直连泄漏。

## 13. 验收标准

以下条件必须全部满足：

- 未验证代理被分配给下载会话的数量为 0；
- 每个下载会话都有非空 `exit_ip` 和新鲜 `last_checked_at`；
- 浏览器与 HTTP 下载的出口 IP 一致；
- 已知存在资源的 20 本验收书成功率不低于 95%；
- 五槽连续运行 2 小时，Worker 不离线，心跳间隔不超过 20 秒；
- 任意节点槽位最多一个未结束会话；
- 代理故障不会增加图书业务失败次数；
- GOST 或浏览器崩溃后 60 秒内完成状态收敛；
- 日志、进程参数、诊断包和管理接口中不出现任何明文凭据；
- 全局暂停成功返回后不再产生新下载分配；
- Windows、macOS ARM/Intel、Linux 四平台 Release 均通过 GitHub Actions 构建和校验。

## 14. 灰度发布与回滚

### 14.1 发布顺序

1. 保持全局暂停；
2. 合并代码并由 GitHub Actions 构建 Master GHCR 镜像；
3. 部署兼容新版协议的 Master，验证迁移和健康检查；
4. 推送版本 tag，由 `release-worker.yml` 生成 Worker Release；
5. Windows 节点安装 Release 包，批准槽位设为 1；
6. 先运行代理检测，确认代理列表出现检测时间、延迟和出口 IP；
7. 恢复全局下载，使用验收书单验证完整下载和 NAS 入库；
8. 按 1 → 2 → 3 → 5 槽逐级放量，每级观察至少 30 分钟；
9. 记录 Git 提交、Actions 链接、镜像 SHA、Worker Release、迁移和验收数据。

禁止在生产服务器或 Worker 机器现场编译正式产物。

### 14.2 回滚条件

出现以下任一情况立即暂停并回滚：

- Worker 心跳连续两次超期；
- 浏览器或 HTTP 下载发生直连泄漏；
- 同槽产生重复活跃会话；
- 代理凭据进入日志或进程参数；
- 已知资源连续 3 本下载失败；
- NAS 文件校验不一致。

回滚使用上一次已验证的 GHCR 镜像和 Worker Release，不重新现场构建。数据库迁移应采用向前兼容设计；若必须恢复数据库，使用发布前备份并同时回退 Master。

## 15. 交付物

- ProxyRuntime 和两个 adapters；
- BrowserExecutor 和槽位线程管理；
- ProxyCheck 完整执行链路；
- 活跃槽位唯一性迁移；
- Master/Worker 协议与管理端可观测性；
- 四平台含固定 GOST 的 Worker Release；
- 自动化测试、Windows 浸泡记录、灰度记录和回滚记录；
- 更新后的部署、Worker 发布与故障排查文档。
