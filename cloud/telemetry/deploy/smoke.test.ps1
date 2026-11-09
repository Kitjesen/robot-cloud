$ErrorActionPreference = 'Stop'
$savedUrl = $env:DOSO_THINGSBOARD_URL
$savedKey = $env:DOSO_THINGSBOARD_KEY
$fixture = @{ Calls = 0; Points = @{} }

function Invoke-RestMethod {
    param($Method, $Uri, $Headers, $MaximumRedirection, $TimeoutSec, $ContentType, $Body)
    $fixture.Calls++
    if ($MaximumRedirection -ne 0 -or $TimeoutSec -ne 10 -or
        $Headers['X-Authorization'] -ne ('ApiKey ' + ('x' * 32))) { throw 'unsafe_request' }
    if ($Uri.EndsWith('/api/device')) { return @{ id = @{ id = '11111111-1111-4111-8111-111111111111' } } }
    if ($Uri.EndsWith('/attributes/SERVER_SCOPE')) { return }
    if ($Method -eq 'POST') {
        $sample = $Body | ConvertFrom-Json
        $fixture.Points[[long]$sample.ts] = $sample.values.dosoSmokeValue
        return
    }
    $all = @($fixture.Points.GetEnumerator() | Sort-Object Key | ForEach-Object { @{ ts = $_.Key; value = $_.Value } })
    if ($Uri.Contains('startTs=')) { return @{ dosoSmokeValue = $all } }
    return @{ dosoSmokeValue = @($all[-1]) }
}

try {
    $env:DOSO_THINGSBOARD_KEY = 'x' * 32
    foreach ($invalid in @('http://localhost:18080', 'http://example.com', 'https://127.0.0.1', 'http://127.0.0.1/path', 'http://user:pass@127.0.0.1')) {
        $env:DOSO_THINGSBOARD_URL = $invalid
        $rejected = $false
        try { & "$PSScriptRoot/smoke.ps1" | Out-Null } catch { $rejected = $_.Exception.Message -eq 'invalid_local_smoke_configuration' }
        if (-not $rejected -or $fixture.Calls -ne 0) { throw "unsafe_target_accepted: $invalid" }
    }
    $env:DOSO_THINGSBOARD_URL = 'http://127.0.0.1:18080'
    $result = & "$PSScriptRoot/smoke.ps1"
    if (-not $result.verified -or $result.points -ne 2 -or $fixture.Calls -ne 7) { throw 'smoke_protocol_failed' }
    Write-Output 'Passed: local target guard, deterministic retries, historical readback, latest preservation (HTTP fixture only).'
} finally {
    $env:DOSO_THINGSBOARD_URL = $savedUrl
    $env:DOSO_THINGSBOARD_KEY = $savedKey
}
