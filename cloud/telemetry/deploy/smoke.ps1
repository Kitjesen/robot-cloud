[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$url = $env:DOSO_THINGSBOARD_URL
$key = $env:DOSO_THINGSBOARD_KEY
$uri = $null
$address = $null
if (-not [Uri]::TryCreate($url, [UriKind]::Absolute, [ref]$uri) -or
    $uri.Scheme -ne 'http' -or
    -not [Net.IPAddress]::TryParse($uri.Host.Trim('[', ']'), [ref]$address) -or
    -not [Net.IPAddress]::IsLoopback($address) -or
    $uri.UserInfo -or $uri.Query -or $uri.Fragment -or $uri.AbsolutePath -ne '/' -or
    $key.Length -lt 32 -or $key.Length -gt 512 -or $key -notmatch '^[\x21-\x7e]+$') {
    throw 'invalid_local_smoke_configuration'
}
$url = $uri.GetLeftPart([UriPartial]::Authority)
$headers = @{ 'X-Authorization' = "ApiKey $key" }

function Request([string]$Method, [string]$Path, $Body = $null) {
    $arguments = @{
        Method = $Method
        Uri = "$url$Path"
        Headers = $headers
        MaximumRedirection = 0
        TimeoutSec = 10
    }
    if ($null -ne $Body) {
        $arguments.ContentType = 'application/json'
        $arguments.Body = ConvertTo-Json -InputObject $Body -Depth 10 -Compress
    }
    try {
        Invoke-RestMethod @arguments
    } catch {
        throw 'thingsboard_smoke_request_failed'
    }
}

$name = 'doso-smoke:' + [Guid]::NewGuid().ToString()
$device = Request POST '/api/device' @{ name = $name; type = 'doso-smoke'; label = 'DOSO isolated API smoke' }
$deviceId = $device.id.id
$parsedId = [Guid]::Empty
if (-not [Guid]::TryParse($deviceId, [ref]$parsedId)) { throw 'invalid_smoke_device_response' }
Request POST "/api/plugins/telemetry/DEVICE/$deviceId/attributes/SERVER_SCOPE" @{ simulated = $true; source = 'doso-api-smoke' } | Out-Null
$latestTs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - 1000
$latest = @{ ts = $latestTs; values = @{ dosoSmokeValue = 'latest' } }
Request POST "/api/plugins/telemetry/DEVICE/$deviceId/timeseries/ANY" $latest | Out-Null
Request POST "/api/plugins/telemetry/DEVICE/$deviceId/timeseries/ANY" $latest | Out-Null
Request POST "/api/plugins/telemetry/DEVICE/$deviceId/timeseries/ANY" @{ ts = $latestTs - 1000; values = @{ dosoSmokeValue = 'older' } } | Out-Null

$path = "/api/plugins/telemetry/DEVICE/$deviceId/values/timeseries?keys=dosoSmokeValue"
$history = Request GET "$path&startTs=$($latestTs - 2000)&endTs=$($latestTs + 1)&agg=NONE&orderBy=ASC&limit=10"
$points = @($history.dosoSmokeValue)
if ($points.Count -ne 2 -or $points[0].ts -ne ($latestTs - 1000) -or
    $points[0].value -ne 'older' -or $points[1].ts -ne $latestTs -or $points[1].value -ne 'latest') {
    throw 'smoke_history_mismatch'
}
$current = Request GET $path
if (@($current.dosoSmokeValue).Count -ne 1 -or
    $current.dosoSmokeValue[0].ts -ne $latestTs -or $current.dosoSmokeValue[0].value -ne 'latest') {
    throw 'smoke_latest_mismatch'
}
[pscustomobject]@{ deviceId = $deviceId; name = $name; simulated = $true; points = 2; latestTimestamp = $latestTs; verified = $true }
