#!/usr/bin/env bash
set -e

# 定位脚本所在目录的上级目录（项目根目录）
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.yml"
ENV_FILE="${SCRIPT_DIR}/.env"

cd "${ROOT_DIR}"

ensure_env() {
  if [ ! -f "${ENV_FILE}" ]; then
    echo "[!] 警告: ${ENV_FILE} 不存在。请复制 ${SCRIPT_DIR}/.env.example 为 ${ENV_FILE} 并配置参数！"
    exit 1
  fi
}

reload_caddy() {
  echo "==> 校验并热加载 Caddy 配置..."
  docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" exec -T caddy \
    caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
  docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" exec -T caddy \
    caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
}

case "$1" in
  start)
    ensure_env
    echo "==> 启动 cloud-platform 服务..."
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" up -d
    # Caddyfile 是 bind mount。文件内容更新不会改变 Compose 的服务配置哈希，
    # `up -d` 可能保留旧容器和旧的内存配置，因此部署后必须显式 reload。
    reload_caddy
    echo "==> 查看服务状态:"
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" ps
    ;;

  stop)
    echo "==> 停止 cloud-platform 服务..."
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" down
    ;;

  restart)
    ensure_env
    echo "==> 重启 cloud-platform 服务..."
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" restart
    ;;

  status|ps)
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" ps
    ;;

  logs)
    shift
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" logs -f "$@"
    ;;

  update)
    ensure_env
    echo "==> 1. 备份数据库..."
    mkdir -p "${ROOT_DIR}/backups"
    docker exec drission-postgres pg_dump -U postgres drission_book > "${ROOT_DIR}/backups/backup_$(date +%Y%m%d_%H%M%S).sql" || true
    
    echo "==> 2. 拉取最新 Master 镜像..."
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" pull master
    
    echo "==> 3. 平滑重启 Master 服务..."
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" up -d --no-deps master

    echo "==> 4. 应用最新 Caddy 配置..."
    reload_caddy
    
    echo "==> 5. 检查服务健康状态..."
    sleep 5
    docker compose --env-file "${ENV_FILE}" -f "${COMPOSE_FILE}" ps
    ;;

  backup)
    mkdir -p "${ROOT_DIR}/backups"
    BACKUP_FILE="${ROOT_DIR}/backups/drission_book_$(date +%Y%m%d_%H%M%S).sql"
    echo "==> 正在备份数据库至 ${BACKUP_FILE} ..."
    docker exec drission-postgres pg_dump -U postgres drission_book > "${BACKUP_FILE}"
    echo "==> 备份完成！大小: $(du -sh "${BACKUP_FILE}" | cut -f1)"
    ;;

  create-admin)
    USERNAME="$2"
    PASSWORD="$3"
    if [ -z "${USERNAME}" ] || [ -z "${PASSWORD}" ]; then
      echo "用法: $0 create-admin <用户名> <密码>"
      exit 1
    fi
    docker exec -i drission-master master-server --config /app/config/master.toml create-admin --username "${USERNAME}" --password "${PASSWORD}"
    ;;

  *)
    echo "============================================="
    echo " Cloud Platform 运维管理脚本"
    echo "============================================="
    echo "用法: $0 {start|stop|restart|status|logs|update|backup|create-admin}"
    echo ""
    echo "  start         - 启动所有容器服务"
    echo "  stop          - 停止所有容器服务"
    echo "  restart       - 重启所有容器服务"
    echo "  status / ps   - 查看容器运行与健康状态"
    echo "  logs [svc]    - 实时查看服务日志 (如: $0 logs master)"
    echo "  update        - 自动备份DB、拉取最新镜像并平滑更新 Master"
    echo "  backup        - 手动备份 PostgreSQL 数据库"
    echo "  create-admin  - 初始化或创建管理员账号 (用法: $0 create-admin admin 123456)"
    echo "============================================="
    exit 1
    ;;
esac
