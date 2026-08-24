# Worker 跨平台客户端发布与使用指南

本文档详细说明分布式 Worker Agent 节点的跨平台自动化构建、GitHub Releases 发布流程以及在 Windows / macOS / Linux 客户端的开箱即用与一键注册操作。

---

## 1. 架构与自动化发布机制

Worker 节点采用 Rust 编写，支持本地桌面或 NAS 环境运行。通过 GitHub Actions（`.github/workflows/release-worker.yml`），每次发布版本都会在原生环境中自动完成跨平台矩阵编译并生成 Release 包：

```text
       GitHub Actions 流水线 (触发: Tag v* 或 手动点击 Run workflow)
                               │
       ┌───────────────────────┼───────────────────────┬───────────────────────┐
       ▼                       ▼                       ▼                       ▼
  Windows (x64)          macOS (Apple Silicon)     macOS (Intel)          Linux (x64)
worker-agent.exe          worker-agent (ARM64)    worker-agent (x86_64)   worker-agent (Linux)
一键启动Worker.bat      一键启动Worker.command   一键启动Worker.command  start-worker.sh
       │                       │                       │                       │
       └───────────────────────┼───────────────────────┴───────────────────────┘
                               ▼
               打包并上传至 GitHub Releases (附带 SHA256 校验和)
```

---

## 2. 客户端软件包清单

用户可在 GitHub 仓库的 **Releases** 页面直接下载对应系统的预打包压缩包：

| 操作系统 | 软件包名称 | 内部包含文件 | 启动方式 |
| :--- | :--- | :--- | :--- |
| **Windows (64位)** | `worker-agent-windows-x64.zip` | `worker-agent.exe`<br>`worker.toml`<br>`一键启动Worker.bat`<br>`README.txt` | 双击 **`一键启动Worker.bat`** |
| **macOS (M系列芯片)** | `worker-agent-macos-arm64.zip` | `worker-agent`<br>`worker.toml`<br>`一键启动Worker.command`<br>`README.txt` | 双击 **`一键启动Worker.command`** |
| **macOS (Intel芯片)** | `worker-agent-macos-intel.zip` | `worker-agent`<br>`worker.toml`<br>`一键启动Worker.command`<br>`README.txt` | 双击 **`一键启动Worker.command`** |
| **Linux (服务器/NAS)** | `worker-agent-linux-x64.tar.gz` | `worker-agent`<br>`worker.toml`<br>`start-worker.sh`<br>`README.txt` | 运行 **`./start-worker.sh`** |

---

## 3. 客户端一键启动与自动注册流程

### 第一步：解压与配置 `worker.toml`
解压下载的压缩包，使用文本编辑器打开 `worker.toml`，按需确认或修改：

```toml
[master]
# Master 云端 gRPC 接入地址（必须为 HTTPS）
endpoint = "https://worker.43-165-64-253.nip.io"
tls_domain = "worker.43-165-64-253.nip.io"

# 本地凭据存储路径（程序自动管理，无需修改）
identity_file = "data/identity.json"
client_cert_file = "data/client.crt"
client_key_file = "data/client.key"
node_ca_file = "data/node-ca.crt"

[storage]
# 运行数据缓存目录
data_dir = "data"

# 局域网 NAS 挂载路径（下载书籍文件直存目录）
# Windows 示例: nas_mount = "\\\\192.168.1.100\\books" 或 "Z:\\books"
# macOS 示例:   nas_mount = "/Volumes/books"
# Linux 示例:   nas_mount = "/mnt/nas/books"
nas_mount = "./data/nas"

[execution]
# 本机请求开启的最大执行槽位数
requested_slots = 5
# 是否启用模拟引擎（生产下载保持 false）
simulated = false
```

### 第二步：双击启动 Worker
- **Windows**：双击 `一键启动Worker.bat`
- **macOS**：双击 `一键启动Worker.command`
  *(首次运行若 macOS 弹出安全提示，在「系统设置 -> 隐私与安全性」中点击「仍要打开」即可)*
- **Linux**：终端运行 `./start-worker.sh`

### 第三步：自动登记与管理员审核
1. 启动后，终端会自动向 Master 发送登记请求，并提示：
   ```text
   [INFO] 正在向云端主控登记注册... 等待管理员在控制台审批
   ```
2. 管理员打开 Web 管理后台（`https://admin.43-165-64-253.nip.io`）并登录。
3. 进入 **「节点管理」** 页面，找到处于「待审核」状态的新节点，点击 **「批准接入」** 并分配允许的槽位数。
4. 审批完成后，Worker 会在几秒内收到通知，**全自动生成私钥并领取 mTLS 双向认证客户端证书**，状态自动变为 **「在线就绪」**，开始自动接收并执行爬取与下载任务！

---

## 4. 如何触发发布新版本 Worker

项目提供了两种便捷的 Release 发布方式：

### 方式 A：Git Tag 推送（推荐）
在本地终端打上符合语义化版本规范的 Tag 并推送到 GitHub：
```bash
git tag v0.2.0
git push origin v0.2.0
```
GitHub Actions 会自动触发多平台矩阵构建，打包各系统专属的运行脚本与文档，并发布到 Releases。

### 方式 B：GitHub Actions 界面手动一键发布
1. 打开 GitHub 仓库页面，点击 **Actions** 标签。
2. 在左侧选择 **Release Worker Binaries** 工作流。
3. 点击右侧 **Run workflow** 按钮。
4. 输入要发布的版本号（例如 `v0.2.0`），点击 **Run workflow** 即可全自动构建并发布。
