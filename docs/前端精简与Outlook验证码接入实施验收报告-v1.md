# 前端精简与 Outlook 验证码接入实施验收报告 V1

> 验收日期：2026-08-25
> 适用目录：`cloud-platform`
> 结论：计划内代码功能已实现；剩余两项属于外部环境验收，不是代码缺口。

## 1. 完成范围

本轮完成了以下闭环：

1. `MailCodeProvider` 已接入真实注册引擎；Outlook、Manual、Mock Adapter 与版本化 Router 均已实现。
2. Master 按注册尝试下发 Provider 快照，Worker 心跳上报应用版本、实际 Provider 和脱敏健康状态。
3. Outlook 兼容 `/api/external/emails`，支持历史验证码基线、邮件时间窗、允许发件人、中英文 4–8 位验证码、轮询超时、取消和人工降级。
4. 人工验证码在原浏览器注册尝试中恢复；人工事项幂等，验证码只在内存中传递，不入库、不写日志。
5. API Key 使用独立密钥引用和 AES-GCM 密文表；前端只显示“已配置”，更新和审计不包含密钥。
6. Outlook 请求强制 HTTPS、非空主机白名单、全 DNS 地址检查、私网/元数据地址阻断、DNS 固定解析、禁止重定向、连接/请求超时、1 MiB 响应上限和不可信 JSON 边界检查。
7. 未强制 Worker 客户端证书时，Master 不下发第三方密钥，自动安全降级为 Manual。
8. 账号中心和系统设置展示 Provider 版本、健康状态、Worker 应用数和只写密钥表单；注册队列展示自动取码、提交验证码和人工降级阶段。
9. 待注册账号无论旧兼容字段取值如何，均在同一事务创建唯一注册批次与任务。
10. 总库改为 `(updated_at, id)` 键集游标分页；无筛选总数使用 PostgreSQL 统计估值，避免每页全表 `COUNT`。
11. 获取任务默认只显示可行动状态；已完成历史降为低频筛选，并展示 Worker、阶段、尝试、重试时间和错误。
12. 导入主流程改为文件上传或只读服务器 manifest；少量文本粘贴被限制为次要入口。
13. 数据质量页只查询待消歧候选，支持并排选择、影响预览和二次确认；合并事务会锁定源/目标并批量写搜索 Outbox。
14. 系统设置以类型化邮件 Provider 为主，任意键值编辑降为折叠兼容入口；服务端禁止创建未知键、改变 JSON 类型或记录原始值。
15. 移动端使用抽屉导航，支持 Escape、焦点进入和焦点归还；空表格 DOM 与 SSE 常驻提示已修复。
16. 服务器 manifest 部署变量与只读挂载已加入 Compose 示例和运维文档。

## 2. 自动化验收结果

| 门禁 | 结果 |
| --- | --- |
| `cargo check --workspace` | 通过，无警告 |
| `cargo test -p automation-core` | 56/56 通过 |
| `cargo test -p worker-agent --lib` | 76/76 通过；本地 TCP 测试需在沙箱外运行 |
| Worker 邮件 Provider 测试 | 12/12 通过 |
| `cargo test -p platform-proto` | 4/4 通过 |
| `cargo test -p master-server` | 单元 116/116，全部数据库集成测试通过 |
| `pnpm test -- --run` | 10 个文件，35/35 通过 |
| `pnpm build` | 通过 |
| `docker compose ... config --quiet` | 通过 |
| 390 × 844 Chromium | `clientWidth=390`、`scrollWidth=390`，零控制台错误 |
| 1440 × 900 Chromium | 桌面侧栏与 8 个权威入口通过 |

真实浏览器截图：

- `account-center-mobile-390.png`
- `account-center-desktop-1440.png`

## 3. 安全审计说明

RustSec 扫描加载了 1226 条公告并检查 394 个锁定依赖：

- `RUSTSEC-2023-0071` 指向 SQLx 派生宏包锁入的可选 MySQL → RSA 分支。项目只启用 PostgreSQL；`cargo tree -i rsa --target all` 无可达依赖路径，运行产物不使用 RSA。公告无可用修复版本，按不可达中危记录，不属于高危/严重发布阻断。
- `RUSTSEC-2025-0134` 是 Tonic 0.12 间接依赖 `rustls-pemfile 2.2.0` 的“停止维护”警告，不是已知漏洞。后续升级 Tonic 时移除。
- `pnpm audit --prod` 需要把依赖清单发送给公共 npm 安全接口；当前安全策略明确拒绝该外发。必须由项目负责人明确批准，或改用内部 npm 安全镜像执行。

## 4. 尚需外部环境完成的两项验收

代码未实现项：**0**。外部环境未验收项：**2**。

### 4.1 真实 Outlook 服务端到端

当前工作区没有可用的 Outlook 服务地址、主机白名单、API Key 和真实注册站点测试账号，因此不能诚实地把真实外部闭环标为已通过。部署时需验证：

```text
导入待注册账号
  → 自动入注册队列
  → Master 通过 mTLS 下发任务级 Provider 快照
  → Worker 提交注册表单
  → Outlook 返回提交后的新验证码
  → Worker 自动填写并确认账号可用
```

同时验证失败路径：认证失败、429、5xx、超时和断网均只产生一条人工事项，人工提交后继续原浏览器尝试。

### 4.2 npm 在线安全公告

获得依赖元数据外发批准后，在 `admin-web` 执行：

```bash
pnpm audit --prod
```

若组织不允许外发，应在内部 npm 镜像或制品安全平台执行等价扫描并归档结果。

## 5. 上线配置要点

1. 生产必须保持 `MASTER_REQUIRE_CLIENT_CERT=1`。
2. 在系统设置中配置 Outlook HTTPS 端点、非空主机白名单、只写 API Key，并先通过连接测试。
3. 可选配置 `CATALOG_MANIFEST_HOST_PATH`；Compose 会以只读方式挂载到 `/app/catalog-manifests`。
4. 真实 E2E 与 npm 安全公告扫描通过后，才可把“外部环境验收”标为完成。
