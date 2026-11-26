@echo off
setlocal
chcp 65001 >nul
echo ============================================================
echo   DOSO THUNDER 研发机 001 一键开通云端远程运维通道
echo ============================================================
echo 请确认本电脑已连接研发机 1 的 Wi-Fi 或网线，然后继续...
echo.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0field-provision-windows.ps1" -DeviceName "thunder-dev-001" -RemotePort 22001 -ServerAddr "api-test.dosorobotics.com" -ServerPort 443
echo.
pause
