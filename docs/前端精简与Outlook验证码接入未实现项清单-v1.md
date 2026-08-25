# 前端精简与 Outlook 验证码接入未实现项清单 V1

> 状态：历史缺口基线；计划内代码已于 2026-08-25 实施
> 验证日期：2026-08-25
> 适用目录：`cloud-platform`
> 完整方案：`docs/前端信息架构精简与账号中心调整方案-v1.md`
> 最新结果：`docs/前端精简与Outlook验证码接入实施验收报告-v1.md`

> 本文第 1–10 节保留实施前的审计证据，不再代表当前代码状态；当前完成度与剩余外部验收以最新报告为准。

## 1. 结论

当前实现完成了“前端导航和路由收敛壳”，但没有完成整套调整方案。

不能标记为已完成的主要原因：

1. `outlook-mail` 自动取码没有接入云端 Worker；
2. 计划中的 `MailCodeProvider` Seam、三个 Adapter 和 Provider Router 都不存在；
3. 账号中心和系统设置没有 Provider 健康状态与验证码接收配置；
4. 图书总库、获取任务、数据导入、数据质量和系统设置仍复用旧页面实现；
5. 移动端固定侧栏导致主内容严重压缩；
6. 空表格存在无效 DOM，真实浏览器会输出 React 控制台错误；
7. 现有测试没有覆盖新导航、新路由、移动端和 Outlook 注册闭环。

## 2. 已实现基线

以下内容已实现，后续不应重复建设：

| 项目 | 状态 | 验证结果 |
| --- | --- | --- |
| 8 个一级导航入口 | 已实现 | 桌面端浏览器实测为 8 项 |
| 导航分为图书馆、运行、管理 | 已实现 | 导航分组正常 |
| 旧路由重定向 | 已实现 | `/catalog/search?q=test&language=zh` 能跳转并保留查询参数 |
| 新总览页 | 已实现 | 桌面端布局和总库进度轨可正常显示 |
| 账号中心基础页 | 部分实现 | 账号摘要、账号列表、批量导入、注册队列入口存在 |
| 注册队列与分组详情 | 部分实现 | 能使用原有注册批次接口展示和操作 |
| Operations / Attention / System 容器路由 | 已实现 | 二级路由容器存在 |
| TypeScript 生产构建 | 通过 | `pnpm build` 通过 |
| 前端现有测试 | 通过 | 7 个测试文件，28/28 通过 |
| `automation-core` 现有测试 | 通过 | 54/54 通过 |
| `worker-agent` 现有测试 | 通过 | 64/64 通过 |

上述通过项不包含 Outlook 功能，因为当前代码库没有 Outlook 云端实现和对应测试。

## 3. 状态总表

| 优先级 | 未实现项 | 当前影响 | 完成后的结果 |
| --- | --- | --- | --- |
| P0 | Outlook 自动取码 | 邮箱验证码仍需人工处理 | Worker 自动取码、填写、确认成功 |
| P0 | Provider 热切换与 Manual 降级 | 外部邮件接入无可替换 Seam | 新任务使用新 Provider，运行中任务固定配置快照 |
| P0 | Provider 密钥与 SSRF 防护 | 未实现外部调用，也未实现相应控制 | API Key 不回显，只能访问允许的 HTTPS 主机 |
| P1 | 图书总库游标分页 | 仍使用 offset 深分页 | 大数据量查询保持稳定 |
| P1 | 获取任务页精简 | 仍展示“已下载”快捷状态 | 默认只显示可行动任务 |
| P1 | 数据导入流程 | 仍以粘贴大段 CSV 文本为主 | 上传补充书单或选择服务器 manifest |
| P1 | 数据质量合并 | 仍要求人工输入源/目标 UUID | 疑似重复并排对比后确认合并 |
| P1 | 验证码接收设置 | 系统设置仍是通用键值编辑 | 类型化设置、健康状态和密钥更换 |
| P1 | 待注册账号强制入队 | 管理员仍可取消创建注册分组 | 选择“待注册”即自动入队 |
| P1 | 移动端导航 | 固定 224px 侧栏压缩主内容 | 桌面侧栏，移动抽屉 |
| P2 | 空表格 DOM | `<tr>` 被渲染到 `<div>` 下 | 空行位于 `<tbody>` 内，控制台无错误 |
| P2 | SSE 常驻状态 | 正常连接也常驻顶栏 | 只在连接中、重连和断开时提示 |
| P2 | 新路由与响应式测试 | 现有 28 项测试不覆盖新信息架构 | 每个兼容路由和角色入口都有自动测试 |

## 4. P0：Outlook 自动取码

### 4.1 当前真实行为

当前 `automation-core` 注册引擎检测到验证码输入框后，直接返回：

```text
awaiting_verification = true
```

`worker-agent` 随后将结果记录为：

```text
等待邮箱验证码
```

该路径没有调用旧单机程序的 `MailClient`，也没有其他等价的邮件 Provider。

### 4.2 计划文件全部缺失

以下计划文件在验收时全部不存在：

```text
crates/automation-core/src/mail_code.rs
crates/worker-agent/src/mail/outlook_http.rs
crates/worker-agent/src/mail/manual.rs
crates/worker-agent/src/mail/router.rs
crates/worker-agent/src/mail/mock.rs
crates/master-server/src/api/mail_provider.rs
crates/master-server/src/store/mail_provider.rs
admin-web/src/features/accounts/MailProviderStatus.tsx
admin-web/src/features/system/MailCodeSettings.tsx
admin-web/src/features/accounts/registrationPhases.ts
```

`crates/platform-proto/proto/worker.proto` 也没有 Provider 配置版本下发或健康上报消息。

### 4.3 必须实现的 Seam

`automation-core` 应定义一个小型、稳定的 `MailCodeProvider` 接口：

```text
prepare(email, deadline) -> MailCodeCursor
awaitCode(cursor, cancellation) -> MailCodeResult
health() -> ProviderHealth
```

接口必须隐藏：

- Outlook HTTP 地址和 JSON 格式；
- 轮询、退避和超时；
- 新邮件游标；
- 验证码正则与去重；
- 错误脱敏；
- 人工降级事项的幂等创建。

调用方只能看到结构化结果和错误分类。

### 4.4 必须实现的 Adapter

1. `OutlookHttpMailCodeAdapter`
   - 兼容旧 `/api/external/emails` 协议；
   - 只读取本次注册提交后的新邮件；
   - 从主题和预览文本提取 4–8 位验证码；
   - 不向 Master 上报邮件正文。
2. `ManualMailCodeAdapter`
   - 生成唯一活动的人工验证事项；
   - 等待 Master 回传人工输入；
   - 任务取消后立即停止等待。
3. `MockMailCodeAdapter`
   - 只允许在测试和非生产环境使用；
   - 生产配置启用时必须启动失败。

### 4.5 Provider Router 热切换

必须满足：

- 配置有单调递增版本；
- 新注册任务使用最新已验证 Provider；
- 已开始的注册尝试固定使用开始时的配置快照；
- 无效配置不覆盖上一个健康版本；
- Outlook 失效可立即切换到 Manual；
- 切换 Provider 不中断图书下载任务。

### 4.6 Master 配置与 Worker 状态

必须补充：

- 超级管理员专用的 Provider 配置接口；
- 配置版本存储；
- Provider 配置下发；
- Worker 应用版本和健康状态上报；
- 认证、限流、超时、解析和人工降级指标；
- 配置更换审计日志。

## 5. P0：Outlook 安全控制

### 5.1 密钥

- 配置只保存 `api_key_secret_ref`；
- 不允许使用明文数据库字段替代密钥管理；
- API Key 不进入前端响应、普通 gRPC 配置、日志、指标或审计详情；
- 前端只能显示“已配置/未配置”；
- 更换密钥必须写审计日志，但审计内容不包含密钥。

### 5.2 外部请求和 SSRF

- 只允许 HTTPS；
- 主机必须命中部署级允许列表；
- 禁止回环、私网、链路本地和云元数据 IP；
- 检查所有 DNS 解析结果；
- 禁止跨主机重定向；
- 设置连接超时、总超时、轮询次数和响应大小上限；
- 外部 JSON 按不可信输入做类型和长度验证。

### 5.3 邮箱和验证码

- 验证码只存在于 Worker 当前尝试的内存中；
- 使用后立即丢弃，不入库、不缓存、不写日志；
- 不将邮箱、邮件 ID、任务 ID 和验证码作为指标标签；
- 只读取当前注册邮箱、允许发件人和当前时间窗内的邮件。

## 6. P1：前端业务页未完成收敛

### 6.1 图书总库

当前：

- 使用 `offset` 和“第 N / 总页数”；
- 列表仍使用旧页面结构和文案；
- 详情链接仍指向旧 `/catalog/editions/:id` 后依赖重定向。

待实现：

- 用游标分页替换 offset；
- 返回 `next_cursor` 和 `previous_cursor`；
- 将搜索词、筛选和游标写入 URL；
- 详情链接直接使用 `/library/editions/:id`。

### 6.2 获取任务

当前快捷状态仍包含“已下载”，仍以通用搜索结果形式显示。

待实现：

- 默认只显示待下载、排队、运行、暂时失败和人工确认；
- 已完成历史放入低频筛选或图书详情；
- 表格优先显示 Worker、阶段、尝试次数、下次重试和失败原因。

### 6.3 数据导入

当前仍以“来源名称 + 文件名 + 粘贴 CSV/文本”为主要流程。

待实现：

- 上传补充书单；
- 选择已登记的服务器目录 manifest；
- 导入运行、数据源、隔离数据和旧批次使用可恢复的 URL 标签；
- 少量粘贴录入只作为限行数的次要功能。

### 6.4 数据质量

当前仍要求管理员手工输入“源作品 UUID”和“目标作品 UUID”。

待实现：

```text
选择疑似重复
  → 并排对比版本、来源和馆藏
  → 选择保留对象
  → 预览影响数量
  → 二次确认
  → 合并
```

### 6.5 系统设置

当前仍是通用“键/值/编辑”表格。

待实现：

- 按获取与重试、账户与额度、验证码接收、Worker、存储、导入与安全分组；
- 已知设置提供类型、范围、说明和服务端验证；
- API Key 只写不读；
- 验证码接收显示 Provider 状态、配置版本、Worker 应用数和脱敏错误。

### 6.6 账户导入和注册队列

当前“待注册”模式下仍有“自动创建注册队列分组”复选框，用户可以取消。

待实现：

- 选择“待注册”时强制创建内部注册分组；
- 只保留“导入后立即启动”开关；
- 不允许产生没有注册任务的待注册账号；
- 任务列表显示等待邮件、自动取码、提交验证码、人工降级和 Provider 异常阶段。

## 7. P1/P2：布局、DOM 与交互缺口

### 7.1 移动端

实测条件：

```text
视口：390 × 844
固定侧栏：224px
文档 clientWidth：390px
文档 scrollWidth：396px
移动菜单按钮：不存在
```

主内容只剩约 166px，标题、按钮、数字和进度轨均出现严重断行。

待实现：

- `md` 以下隐藏桌面侧栏；
- 顶栏提供具有可访问名称的菜单按钮；
- 菜单使用抽屉展示 8 个入口；
- 打开抽屉时管理焦点，关闭后返回菜单按钮；
- 390px 视口下不得出现整页横向滚动。

### 7.2 空表格 DOM

`Table` 当前在 `</table>` 之后渲染 `empty`，但 `EmptyRow` 返回 `<tr>`。

实测控制台错误：

```text
validateDOMNesting(...): <tr> cannot appear as a child of <div>
```

待实现：

```tsx
<tbody>
  {hasRows ? children : empty}
</tbody>
```

并增加空账号、空设置、空注册分组的 DOM 测试。

### 7.3 SSE 状态

当前 SSE 连接正常时也常驻顶栏。待调整为：

- 连接正常时不显示；
- 首次连接超时、重连中或已断开时显示；
- 状态变化通过 `aria-live="polite"` 通知。

## 8. 需要新增和修改的文件

### 8.1 新增

```text
crates/automation-core/src/mail_code.rs
crates/worker-agent/src/mail/mod.rs
crates/worker-agent/src/mail/outlook_http.rs
crates/worker-agent/src/mail/manual.rs
crates/worker-agent/src/mail/router.rs
crates/worker-agent/src/mail/mock.rs
crates/master-server/src/api/mail_provider.rs
crates/master-server/src/store/mail_provider.rs
admin-web/src/features/accounts/MailProviderStatus.tsx
admin-web/src/features/accounts/registrationPhases.ts
admin-web/src/features/system/MailCodeSettings.tsx
admin-web/src/__tests__/navigation_and_routes.test.tsx
admin-web/src/__tests__/responsive_layout.test.tsx
admin-web/src/__tests__/mail_provider.test.tsx
```

### 8.2 修改

```text
crates/automation-core/src/lib.rs
crates/automation-core/src/real.rs
crates/automation-core/src/types.rs
crates/worker-agent/src/slot.rs
crates/worker-agent/src/dynamic.rs
crates/platform-proto/proto/worker.proto
crates/master-server/src/api/mod.rs
crates/master-server/src/grpc/convert.rs
crates/master-server/src/grpc/inbound.rs
crates/master-server/src/models.rs
admin-web/src/App.tsx
admin-web/src/components/layout.tsx
admin-web/src/components/ui.tsx
admin-web/src/features/accounts/AccountCenterPage.tsx
admin-web/src/features/accounts/RegistrationQueuePage.tsx
admin-web/src/features/system/SystemLayout.tsx
admin-web/src/pages/CatalogSearchPage.tsx
admin-web/src/pages/CatalogAcquisitionsPage.tsx
admin-web/src/pages/CatalogImportsPage.tsx
admin-web/src/pages/CatalogQualityPage.tsx
admin-web/src/pages/SettingsPage.tsx
admin-web/src/lib/api.ts
admin-web/src/lib/types.ts
```

### 8.3 暂不删除

以下后端表和调度逻辑在新注册闭环通过之前不删除：

- `account_registration_batches`；
- `account_registration_tasks`；
- `manual_actions`；
- 注册任务租约和重试逻辑；
- 旧路由重定向。

## 9. 建议实施顺序

### 阶段 A：修复当前前端硬错误

1. 修复 `Table` 空状态 DOM；
2. 增加移动抽屉导航；
3. 增加新导航、旧路由和响应式测试；
4. 清理 React Router Future Flag 测试警告。

### 阶段 B：建立邮件 Provider Seam

1. 定义 `MailCodeProvider` 接口和错误分类；
2. 实现 Mock Adapter 和接口测试；
3. 改造注册引擎，在提交前 `prepare`，验证码输入框出现后 `awaitCode`；
4. 确认取消能立即中断等待。

### 阶段 C：Outlook、Manual 和 Router

1. 迁移旧 Outlook 响应解析与新邮件游标逻辑；
2. 实现 Manual Adapter 及人工事项幂等；
3. 实现版本化 Router 和配置快照；
4. 实现 Outlook 失败自动降级；
5. 加入超时、限流和熔断指标。

### 阶段 D：Master 配置和安全控制

1. 实现密钥引用，不使用明文设置表；
2. 实现超级管理员权限与审计；
3. 实现 HTTPS、主机允许列表、DNS/IP 检查和重定向限制；
4. 实现配置下发与 Worker 健康上报；
5. 增加 Provider 测试连接接口，响应不含邮件或验证码。

### 阶段 E：账号中心和系统设置

1. 账号中心显示 Provider 健康状态；
2. 注册队列显示自动取码阶段和验证方式；
3. 系统设置增加类型化验证码接收配置；
4. 移除“是否创建注册分组”复选框；
5. 保留人工验证下钻和失败回传。

### 阶段 F：收敛其他业务页

1. 图书总库游标分页；
2. 获取任务只显示可行动状态；
3. 数据导入改为上传和 manifest；
4. 数据质量改为并排对比合并；
5. 系统设置移除通用键值主交互。

## 10. 测试与验收门禁

### 10.1 前端

```text
pnpm test
pnpm build
```

必须新增测试：

- 8 个导航按角色显示；
- 所有旧路由跳转目标与参数保留；
- 390px、768px、1440px 布局；
- 空表格无 DOM 错误；
- 待注册账号强制入队；
- Outlook 健康状态、脱敏错误和只写密钥表单；
- 读取接口不回显 API Key。

### 10.2 Rust

```text
cargo test -p automation-core
cargo test -p worker-agent --lib
cargo test -p platform-proto
cargo test -p master-server
```

必须新增测试：

- 邮件游标排除历史验证码；
- 中英文验证码解析；
- `401/403`、`429`、`5xx`、超时、断网和非法 JSON 分类；
- 响应大小和字段长度上限；
- 任务取消停止轮询；
- Provider 版本热切换；
- 无效配置保留上一健康版本；
- 同一注册任务只创建一条活动人工事项；
- 人工提交后原注册尝试恢复；
- Mock Adapter 在生产环境被拒绝。

### 10.3 安全

- HTTPS 强制；
- 回环、私网、链路本地和云元数据地址被拒绝；
- 跨主机重定向被拒绝；
- 普通管理员不能读写 Provider 配置；
- API Key、邮件正文、验证码不出现在前端、日志、指标、审计和数据库明文；
- 针对锁定依赖运行原生依赖审计，对可达严重/高危问题完成处理或有审批的限时豁免。

当前 `pnpm audit --prod` 未完成：环境未授权将依赖清单元数据发送给公共 npm 安全接口。正式发布前必须由项目负责人明确批准或使用内部安全镜像完成审计。

### 10.4 真实端到端

当前浏览器验收使用了隔离 Chromium 和模拟接口，因为验收时 Master、Worker 和 Outlook 服务都没有运行。

正式完成前必须进行：

```text
真实补充账号导入
  → 自动进入注册队列
  → Master 分配 Worker
  → Worker 提交注册
  → Outlook 返回新验证码
  → Worker 自动填写
  → 账号进入可下载状态
```

以及失败路径：

```text
Outlook 超时/限流/认证失败
  → 只生成一条人工验证事项
  → 管理员输入验证码
  → Worker 继续原注册尝试
```

## 11. 完成定义

只有以下项目全部勾选后，才能将完整调整方案标记为“已实现”：

- [x] `MailCodeProvider` 接口存在且被真实注册链调用。
- [x] Outlook、Manual 和 Mock 三个 Adapter 存在。
- [x] Provider Router 支持版本化热切换和运行中快照。
- [ ] Outlook 成功取码并自动填写的端到端测试通过。
- [x] Outlook 失败能降级为唯一人工事项。
- [x] API Key 只以密钥引用保存，前端不回显。
- [x] HTTPS、SSRF、重定向、超时和响应大小控制通过测试。
- [x] 账号中心显示 Provider 健康和验证阶段。
- [x] 系统设置提供类型化的验证码接收配置。
- [x] 待注册账号无法绕过唯一注册队列。
- [x] 图书总库使用游标分页。
- [x] 获取任务默认不展示已完成历史。
- [x] 数据导入的主流程不再是粘贴大段 CSV。
- [x] 数据质量合并不再要求手工输入 UUID。
- [x] 390px 移动端无整页横向滚动，且可使用抽屉导航。
- [x] 空表格不产生 DOM 控制台错误。
- [x] 新路由、旧路由、角色导航、移动端和 Outlook 都有自动化测试。
- [ ] 前端构建、前端测试、Rust 测试、安全审计和真实浏览器验收全部通过。

## 12. 上线判定

实施后判定：

```text
计划内代码功能：已实现
Outlook Provider 云端链路：已实现并通过自动化测试
移动端与浏览器控制台：验收通过
真实 Outlook 外部端到端：等待部署凭据与服务
npm 在线安全公告：等待依赖元数据外发批准或内部镜像
总体：代码可部署联调；完成两项外部验收后再正式上线
```

下一次验收应以本文档第 11 节为唯一完成清单，不再以“文件已创建”或“现有测试通过”替代功能闭环验收。
