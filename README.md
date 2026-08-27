# Cloud Platform (云端主控调度平台)

分布式爬虫与采集自动化云端中枢，集成 Web 管理后台、gRPC 节点管理（双向 mTLS 证书强校验）、分布式任务调度下发与 PostgreSQL 数据持久化。

---

## 架构拓扑

```text
                  公网 (只开 80 / 443 端口)
                             │
                     ┌───────▼────────┐
                     │ Caddy (双入口) │
                     └───┬────────┬───┘
      ADMIN_DOMAIN       │        │   WORKER_DOMAIN
  (admin.43-165-64-253.nip.io)   │ (worker.43-165-64-253.nip.io)
    Web/API/Enroll 注册  │        │ 仅 OpenLink (mTLS)
                         ▼        ▼
                    Master 8080 / 9443 (Docker 内网)
                         │
                         ▼
             PostgreSQL 16 + OpenSearch（Docker 内网）
```

---

## 📦 Worker 跨平台客户端下载与一键启动

Worker 节点采用全自动跨平台构建，已在 GitHub Releases 中发布各系统预打包安装包：

🔗 **[点击前往 GitHub Releases 下载页面](https://github.com/JWJW000/cloud-platform/releases)**

| 操作系统 | 下载压缩包 | 一键点击启动方式 |
| :--- | :--- | :--- |
| **Windows (x64)** | `worker-agent-windows-x64.zip` | 解压后双击 **`一键启动Worker.bat`** |
| **macOS (Apple Silicon M系列)** | `worker-agent-macos-arm64.zip` | 解压后双击 **`一键启动Worker.command`** |
| **macOS (Intel芯片)** | `worker-agent-macos-intel.zip` | 解压后双击 **`一键启动Worker.command`** |
| **Linux (x64 / NAS)** | `worker-agent-linux-x64.tar.gz` | 解压后执行 **`./start-worker.sh`** |

> 详细配置与审核流程请参阅：[Worker 跨平台客户端发布与使用指南](docs/Worker跨平台客户端发布与使用指南.md)

---

## 🚀 云端主控 Master 快速开始

### 1. 服务器准备与初始化

```bash
# 克隆仓库到服务器
git clone https://github.com/JWJW000/cloud-platform.git /data/cloud-platform
cd /data/cloud-platform

# 配置生产环境变量
cp deploy/.env.example deploy/.env
vim deploy/.env
```

### 2. 启动服务与初始化管理员

```bash
# 使用管理脚本启动所有容器（Postgres + Master + Caddy）
./deploy/manage.sh start

# 创建初始管理员账号
./deploy/manage.sh create-admin admin 'YourStrongPassword123!'
```

---

## 🛠️ 云端运维命令速查 (`deploy/manage.sh`)

| 命令 | 功能说明 |
| :--- | :--- |
| `./deploy/manage.sh start` | 启动所有容器服务 |
| `./deploy/manage.sh stop` | 停止服务 |
| `./deploy/manage.sh restart` | 重启服务 |
| `./deploy/manage.sh status` | 查看容器运行与健康检查状态 |
| `./deploy/manage.sh logs [master/caddy/postgres]` | 实时查看指定服务日志 |
| `./deploy/manage.sh update` | 自动备份数据库、拉取最新 GHCR 镜像并平滑重启 Master |
| `./deploy/manage.sh backup` | 手动备份 PostgreSQL 数据库到 `backups/` 目录 |
| `./deploy/manage.sh create-admin <user> <pwd>` | 初始化或重置管理员账号 |
| `./deploy/manage.sh reindex-catalog [batch]` | 从 PostgreSQL 全量重建 OpenSearch 书目索引 |

---

## ⚙️ CI/CD 自动化流水线说明 (GitHub Actions)

项目内置三条自动化流水线：
1. **`.github/workflows/ci.yml`**：代码门禁（Rust fmt/clippy 静态检查、全套单元测试、PostgreSQL 真实迁移集成测试、前端构建与依赖安全审计）。
2. **`.github/workflows/deploy.yml`**：推送 `main` 分支时自动构建 Master 生产镜像至 GitHub Packages (`ghcr.io`)，并通过 SSH 自动触发服务器一键滚动部署。
3. **`.github/workflows/release-worker.yml`**：推送版本 Tag（如 `git tag v0.2.0 && git push origin v0.2.0`）或在 Actions 界面手动触发时，自动完成 Windows/macOS/Linux 跨平台编译并发布 GitHub Release。

---

## 📚 详细文档索引

- [Worker 跨平台客户端发布与使用指南](docs/Worker跨平台客户端发布与使用指南.md)
- [云端自动化平台部署与运维指南](docs/部署与运维指南.md)
- [OpenSearch 接入与重建指南](docs/OpenSearch接入与重建指南.md)
- [前端云端化与 Worker 直连注册实施方案](docs/前端云端化与Worker直连注册修复实施方案-v5.md)
- [CSV 建批与业务任务统一下发方案](docs/CSV建批与业务任务统一下发实现方案-v6.md)
