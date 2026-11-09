$ErrorActionPreference = 'Stop'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('doso-bootstrap-test-' + [Guid]::NewGuid().ToString('N'))
$statePath = Join-Path $directory 'state.json'
$fixture = @{ AdminPassword = 'sysadmin'; UserPassword = $null; Changes = 0; Activations = 0; Keys = 0; Calls = 0 }

function Invoke-RestMethod {
    param($Method, $Uri, $Headers, $MaximumRedirection, $TimeoutSec, $ContentType, $Body)
    $fixture.Calls++
    if ($MaximumRedirection -ne 0 -or $TimeoutSec -ne 15) { throw 'unsafe_bootstrap_request' }
    $bodyValue = if ($Body) { $Body | ConvertFrom-Json } else { $null }
    if ($Uri.EndsWith('/api/auth/login')) {
        if ($bodyValue.username -eq 'sysadmin@thingsboard.org' -and $bodyValue.password -eq $fixture.AdminPassword) { return @{ token = 'admin-fixture' } }
        if ($bodyValue.username -like 'telemetry-*@doso.invalid' -and $bodyValue.password -eq $fixture.UserPassword) { return @{ token = 'user-fixture' } }
        throw 'unauthorized_fixture'
    }
    if ($Uri.EndsWith('/api/auth/changePassword')) {
        $saved = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
        if ($saved.adminPassword -ne $bodyValue.newPassword) { throw 'password_not_saved_before_change' }
        $fixture.AdminPassword = $bodyValue.newPassword
        $fixture.Changes++
        return @{ token = 'admin-fixture' }
    }
    if ($Uri.Contains('/api/tenants?')) { return @{ data = @() } }
    if ($Uri.EndsWith('/api/tenant')) { return @{ id = @{ id = '11111111-1111-4111-8111-111111111111' } } }
    if ($Uri.Contains('/users?')) { return @{ data = @() } }
    if ($Uri.EndsWith('/api/user?sendActivationMail=false')) {
        return @{ id = @{ id = '22222222-2222-4222-8222-222222222222' }; authority = $bodyValue.authority; tenantId = $bodyValue.tenantId }
    }
    if ($Uri.EndsWith('/activationLink')) { return 'http://127.0.0.1:18080/api/noauth/activate?activateToken=fixture-token' }
    if ($Uri.EndsWith('/api/noauth/activate?sendActivationMail=false')) {
        $fixture.UserPassword = $bodyValue.password
        $fixture.Activations++
        return @{ token = 'user-fixture' }
    }
    if ($Uri.EndsWith('/api/apiKey')) {
        $fixture.Keys++
        return @{ value = 'ApiKey ' + ('k' * 32) }
    }
    throw 'unexpected_bootstrap_request'
}

try {
    $first = & "$PSScriptRoot/bootstrap.ps1" -StateFile $statePath
    $second = & "$PSScriptRoot/bootstrap.ps1" -StateFile $statePath
    $saved = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
    if (-not $first.initialized -or -not $second.initialized -or $fixture.Changes -ne 1 -or
        $fixture.Activations -ne 1 -or $fixture.Keys -ne 1 -or $saved.apiKey -ne ('k' * 32) -or
        $saved.adminPassword.Length -lt 32 -or $saved.userPassword.Length -lt 32) { throw 'bootstrap_idempotence_failed' }
    $acl = Get-Acl -LiteralPath $statePath
    if (-not $acl.AreAccessRulesProtected) { throw 'bootstrap_state_acl_not_private' }
    $before = $fixture.Calls
    $rejected = $false
    try { & "$PSScriptRoot/bootstrap.ps1" -StateFile $statePath -Url 'http://example.com' | Out-Null } catch { $rejected = $_.Exception.Message -eq 'invalid_local_bootstrap_url' }
    if (-not $rejected -or $fixture.Calls -ne $before) { throw 'remote_bootstrap_target_accepted' }
    Write-Output 'Passed: password saved before change, no-mail activation, private state, bare API key and idempotent rerun (HTTP fixture only).'
} finally {
    foreach ($file in @($statePath, (Join-Path $directory 'thingsboard.env'))) {
        if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file }
    }
    if (Test-Path -LiteralPath $directory) { Remove-Item -LiteralPath $directory }
}
