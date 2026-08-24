#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"
chmod +x ./worker-agent 2>/dev/null || true

if [ ! -f "worker.toml" ]; then
    echo "[错误] 缺少 worker.toml 配置文件！"
    exit 1
fi
mkdir -p data

echo "==> 启动 Worker Agent 守护进程..."
./worker-agent --config worker.toml run
