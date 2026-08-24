#!/usr/bin/env bash
cd "$(dirname "$0")"
chmod +x ./worker-agent 2>/dev/null || true

echo "========================================================"
echo "   Cloud Platform - 局域网 Worker Agent"
echo "========================================================"
echo ""

if [ ! -f "worker.toml" ]; then
    echo "[提示] 未检测到 worker.toml 配置文件，请确保 worker.toml 位于当前目录。"
    read -n 1 -s -r -p "按任意键退出..."
    exit 1
fi

mkdir -p data

echo "[信息] 正在启动 Worker Agent..."
echo "[信息] 首次运行会自动向云端 Master 登记，等待管理员在 Web 后台审核批准。"
echo "[信息] 如需停止 Worker，请直接关闭窗口或按 Ctrl + C。"
echo "--------------------------------------------------------"
echo ""

./worker-agent --config worker.toml run

echo ""
echo "========================================================"
echo "[提示] Worker Agent 已退出，按任意键关闭窗口..."
echo "========================================================"
read -n 1 -s -r
