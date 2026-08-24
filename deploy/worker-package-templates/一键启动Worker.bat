@echo off
chcp 65001 >nul
title Cloud Platform - 局域网 Worker Agent
cd /d "%~dp0"

echo ========================================================
echo    Cloud Platform - 局域网 Worker Agent
echo ========================================================
echo.

if not exist "worker.toml" (
    echo [提示] 未检测到 worker.toml 配置文件，请确保 worker.toml 位于当前目录。
    pause
    exit /b 1
)

if not exist "data" mkdir data

echo [信息] 正在启动 Worker Agent...
echo [信息] 首次运行会自动向云端 Master 登记，等待管理员在 Web 后台审核批准。
echo [信息] 如需停止 Worker，请直接关闭窗口或按 Ctrl + C。
echo --------------------------------------------------------
echo.

"%~dp0worker-agent.exe" --config worker.toml run

echo.
echo ========================================================
echo [提示] Worker Agent 已退出。
echo ========================================================
pause
