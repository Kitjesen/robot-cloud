param(
    [string]$DeviceName = "thunder-dev-001",
    [int]$RemotePort = 22001,
    [string]$ServerAddr = "api-test.dosorobotics.com",
    [int]$ServerPort = 443,
    [string]$RobotIp = "",
    [string]$RobotUser = "sunrise",
    [string]$TokenFile = "",
    [string]$FrpcBin = ""
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $TokenFile) { $TokenFile = Join-Path $ScriptDir "doso-ops-tunnel.token" }
if (-not $FrpcBin) { $FrpcBin = Join-Path $ScriptDir "frpc" }
$Installer = Join-Path $ScriptDir "install-robot-tunnel.sh"

foreach ($f in @($TokenFile, $FrpcBin, $Installer)) {
    if (-not (Test-Path $f)) {
        Write-Host "[ERROR] Missing required file: $f" -ForegroundColor Red
        exit 1
    }
}

function Test-TcpPort([string]$HostAddr, [int]$Port, [int]$TimeoutMs = 400) {
    try {
        $client = New-Object System.Net.Sockets.TcpClient
        $iar = $client.BeginConnect($HostAddr, $Port, $null, $null)
        $ok = $iar.AsyncWaitHandle.WaitOne($TimeoutMs, $false)
        if ($ok -and $client.Connected) {
            $client.EndConnect($iar)
            $client.Close()
            return $true
        }
        $client.Close()
    } catch {}
    return $false
}

if (-not $RobotIp) {
    Write-Host "Scanning for robot SSH (port 22)..." -ForegroundColor Cyan
    $candidates = @("192.168.66.64", "192.168.66.13", "192.168.123.161", "192.168.1.10", "192.168.12.1")
    foreach ($ip in $candidates) {
        if (Test-TcpPort $ip 22 450) {
            $RobotIp = $ip
            break
        }
    }
}

if (-not $RobotIp) {
    $RobotIp = Read-Host "Robot IP not auto-detected. Enter robot LAN IP (e.g. 192.168.66.64)"
}

if (-not (Test-TcpPort $RobotIp 22 1500)) {
    Write-Host "[ERROR] Cannot reach ${RobotIp}:22. Check Wi-Fi/Ethernet connection to robot." -ForegroundColor Red
    exit 1
}

Write-Host "Target robot: ${RobotUser}@${RobotIp} -> ${DeviceName} (port ${RemotePort})" -ForegroundColor Green
$secPwd = Read-Host "Enter SSH/sudo password for ${RobotUser}" -AsSecureString
$bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secPwd)
$plainPwd = [Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr)
[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
if ([string]::IsNullOrEmpty($plainPwd)) {
    Write-Host "[ERROR] Password cannot be empty." -ForegroundColor Red
    exit 1
}

$askPass = Join-Path $env:TEMP ("doso-askpass-" + [guid]::NewGuid().ToString("N") + ".cmd")
try {
    $escaped = $plainPwd.Replace("%", "%%")
    "@echo off`r`necho $escaped" | Set-Content -Path $askPass -Encoding ASCII
    $env:SSH_ASKPASS = $askPass
    $env:SSH_ASKPASS_REQUIRE = "force"
    $env:DISPLAY = "dummy:0"

    $sshOpts = @(
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=NUL",
        "-o", "ConnectTimeout=10"
    )

    Write-Host "[1/3] Uploading frpc, installer, and token..." -ForegroundColor Cyan
    & scp.exe -O -C @sshOpts $FrpcBin $Installer $TokenFile "${RobotUser}@${RobotIp}:/tmp/"
    if ($LASTEXITCODE -ne 0) { throw "scp upload failed ($LASTEXITCODE)" }

    $remoteTokenName = Split-Path -Leaf $TokenFile
    $remoteFrpcName = Split-Path -Leaf $FrpcBin
    $remoteCmd = @(
        "chmod +x /tmp/install-robot-tunnel.sh /tmp/${remoteFrpcName}",
        "printf '%s\n' '$($plainPwd.Replace("'", "'\''"))' | sudo -S bash /tmp/install-robot-tunnel.sh --name '$DeviceName' --remote-port $RemotePort --server '$ServerAddr' --server-port $ServerPort --token-file '/tmp/${remoteTokenName}' --frpc-bin '/tmp/${remoteFrpcName}'",
        "rm -f '/tmp/${remoteTokenName}' '/tmp/${remoteFrpcName}' /tmp/install-robot-tunnel.sh"
    ) -join " && "

    Write-Host "[2/3] Installing & enabling doso-ops-tunnel.service on robot..." -ForegroundColor Cyan
    & ssh.exe @sshOpts "${RobotUser}@${RobotIp}" $remoteCmd
    if ($LASTEXITCODE -ne 0) { throw "Remote install failed ($LASTEXITCODE)" }

    Write-Host "[3/3] Verifying tunnel service logs..." -ForegroundColor Cyan
    $verifyCmd = "systemctl is-active doso-ops-tunnel.service && printf '%s\n' '$($plainPwd.Replace("'", "'\''"))' | sudo -S journalctl -u doso-ops-tunnel.service -n 8 --no-pager"
    & ssh.exe @sshOpts "${RobotUser}@${RobotIp}" $verifyCmd
    Write-Host "DONE: ${DeviceName} reverse tunnel active on ${ServerAddr}:${ServerPort} -> 127.0.0.1:${RemotePort}" -ForegroundColor Green
} finally {
    Remove-Item -Force -ErrorAction SilentlyContinue $askPass
    Remove-Item Env:SSH_ASKPASS, Env:SSH_ASKPASS_REQUIRE, Env:DISPLAY -ErrorAction SilentlyContinue
}
