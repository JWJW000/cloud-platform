# Cloud Platform (云端主控调度平台)

分布式爬虫与采集自动化云端中枢，集成 Web 管理后台、gRPC 节点管理（mTLS 证书认证）、任务调度下发与 PostgreSQL 数据持久化。

---

## 架构拓扑

```text
                  公网 (只开 80 / 443 端口)
                             │
                     ┌───────▼────────┐
                     │ Caddy (双入口) │
                     └───┬────────┬───┘
      ADMIN_DOMAIN       │        │   WORKER_DOMAIN
    (admin.example.com)  │        │ (worker.example.com)
    Web/API/Enroll 注册  │        │ 仅 OpenLink (mTLS)
                         ▼        ▼
                    Master 8080 / 9443 (Docker 内网)
                         │
                         ▼
                    PostgreSQL 16 (Docker 内网)
```

---

## 快速开始

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
# 使用管理脚本启动
./deploy/manage.sh start

# 创建初始管理员账号
./deploy/manage.sh create-admin admin 'YourStrongPassword123!'
```

---

## 运维命令速查 (`deploy/manage.sh`)

| 命令 | 功能说明 |
| :--- | :--- |
| `./deploy/manage.sh start` | 启动所有服务 |
| `./deploy/manage.sh stop` | 停止服务 |
| `./deploy/manage.sh restart` | 重启服务 |
| `./deploy/manage.sh status` | 查看容器状态（健康检查） |
| `./deploy/manage.sh logs [master/caddy/postgres]` | 查看实时日志 |
| `./deploy/manage.sh update` | 自动备份数据库、拉取最新镜像并平滑重启 Master |
| `./deploy/manage.sh backup` | 手动备份 PostgreSQL 数据库到 `backups/` 目录 |
| `./deploy/manage.sh create-admin <user> <pwd>` | 创建或重置管理员账号 |

---

## CI/CD 自动化部署配置 (GitHub Actions)

项目内置了两条 GitHub Actions 流水线：
1. **`.github/workflows/ci.yml`**：代码门禁（Rust fmt/clippy、单元测试、PostgreSQL 迁移集成测试、前端构建与审计）。
2. **`.github/workflows/deploy.yml`**：自动构建 Docker 镜像推送到 GitHub Packages (`ghcr.io`)，并通过 SSH 自动触发服务器一键部署。

### 开启自动 SSH 部署（可选）
在 GitHub 仓库 **Settings -> Secrets and variables -> Actions** 中添加以下 Repository Secrets：
- `SERVER_HOST`: `43.165.64.253`
- `SERVER_USER`: `root`
- `SSH_PRIVATE_KEY`: 您的 SSH 私钥内容（即 `Key_us.pem` 文件完整文本）
- `SERVER_PORT`: `22` (可选，默认 22)
