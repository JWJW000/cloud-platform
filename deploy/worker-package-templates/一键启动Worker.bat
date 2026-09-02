@echo off
title Cloud Platform - Worker Agent
cd /d "%~dp0"

echo ========================================================
echo    Cloud Platform - Worker Agent
echo ========================================================
echo.

if not exist "worker.toml" (
    echo [ERROR] worker.toml was not found in the current directory.
    pause
    exit /b 1
)

if not exist "data" mkdir data

echo [INFO] Starting Worker Agent...
echo [INFO] On first run, approve this node in the cloud admin console.
echo [INFO] Press Ctrl+C or close this window to stop the Worker.
echo --------------------------------------------------------
echo.

:run_worker
"%~dp0worker-agent.exe" --config worker.toml run
set "EXIT_CODE=%ERRORLEVEL%"

if "%EXIT_CODE%"=="0" goto worker_done

echo.
echo [WARN] Worker Agent exited unexpectedly with code %EXIT_CODE%.
echo [WARN] Restarting automatically in 5 seconds. Press Ctrl+C to stop.
timeout /t 5 /nobreak >nul
goto run_worker

:worker_done
echo.
echo ========================================================
echo [INFO] Worker Agent has exited.
echo ========================================================
pause
