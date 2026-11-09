param(
    [Parameter(Mandatory)][guid]$Device,
    [Parameter(Mandatory)][string]$RunDir
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$env:MOSQUITTO_BIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto.exe'
$env:MOSQUITTO_ACL_PLUGIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto_acl_file.dll'
$env:OPENSSL_BIN = 'C:\Users\99563\miniconda3\Library\bin\openssl.exe'
New-Item -ItemType Directory -Path $RunDir -Force | Out-Null
$record = Join-Path $RunDir 'process.json'
try {
    $process = Start-Process -FilePath (Join-Path $root 'target/debug/lab.exe') `
        -ArgumentList @('uplink', $Device.ToString()) -WorkingDirectory $root `
        -RedirectStandardOutput (Join-Path $RunDir 'uplink.log') `
        -RedirectStandardError (Join-Path $RunDir 'uplink.err') `
        -WindowStyle Hidden -PassThru
    @{ pid = $process.Id; startedAt = [DateTime]::UtcNow.ToString('o'); deviceId = $Device.ToString() } |
        ConvertTo-Json | Set-Content -LiteralPath $record -Encoding UTF8
    $process.WaitForExit()
    @{ pid = $process.Id; exitedAt = [DateTime]::UtcNow.ToString('o'); exitCode = $process.ExitCode } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDir 'exit.json') -Encoding UTF8
    exit $process.ExitCode
} catch {
    $_ | Out-String | Set-Content -LiteralPath (Join-Path $RunDir 'runner.err') -Encoding UTF8
    exit 1
}
