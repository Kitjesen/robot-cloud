@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"

where python >nul 2>nul
if errorlevel 1 (
  echo 未检测到 Python 3，请先安装 Python 后重试。
  pause
  exit /b 1
)

echo 正在启动 DOSO OTA 管理台 (http://127.0.0.1:8190/) ...
python "%~dp0tools\ota-console\server.py" --port 8190 --open
if errorlevel 1 (
  echo.
  echo 启动失败，请检查上方报错信息。
  pause
)
