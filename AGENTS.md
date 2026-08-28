# 项目协作约束

## 生产构建与发布

- Master 镜像和 Worker 安装包必须由 GitHub Actions 构建、打包并发布。
- 禁止在生产服务器上执行 `docker compose build`、`docker build`、`cargo build` 或前端生产构建。
- Master 生产更新只允许拉取 GitHub Actions 已推送到 GHCR 的镜像，再由部署流水线完成备份、更新和健康检查。
- Worker 更新必须使用 `release-worker.yml` 生成的 GitHub Release 安装包；不得把本地或生产机临时编译的二进制作为正式版本分发。
- 紧急修复也必须先提交并触发 GitHub Actions。若流水线不可用，应停止发布并报告阻塞，不得改为在生产机现场编译。
- 完整操作与回滚步骤见 `docs/部署与运维指南.md` 第 10 节。
