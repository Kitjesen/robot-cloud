[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$StateFile,
    [string]$Url = 'http://127.0.0.1:18080'
)

$ErrorActionPreference = 'Stop'
$uri = $null
$address = $null
if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$uri) -or $uri.Scheme -ne 'http' -or
    -not [Net.IPAddress]::TryParse($uri.Host.Trim('[', ']'), [ref]$address) -or
    -not [Net.IPAddress]::IsLoopback($address) -or $uri.UserInfo -or $uri.Query -or $uri.Fragment -or $uri.AbsolutePath -ne '/') {
    throw 'invalid_local_bootstrap_url'
}
$Url = $uri.GetLeftPart([UriPartial]::Authority)
$StateFile = [IO.Path]::GetFullPath($StateFile)
$sourceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..')).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if ($StateFile.StartsWith($sourceRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'bootstrap_state_must_be_outside_source' }
$privateDirectory = Split-Path $StateFile
[void][IO.Directory]::CreateDirectory($privateDirectory)

function SavePrivate([string]$Path, [string]$Content) {
    $temporary = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        $stream = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew)
        $stream.Dispose()
        $acl = [Security.AccessControl.FileSecurity]::new()
        $acl.SetAccessRuleProtection($true, $false)
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($identity, 'FullControl', 'Allow'))
        Set-Acl -LiteralPath $temporary -AclObject $acl
        [IO.File]::WriteAllText($temporary, $Content, [Text.UTF8Encoding]::new($false))
        [IO.File]::Move($temporary, $Path, $true)
    } finally {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary }
    }
}

function SaveState { SavePrivate $StateFile ($state | ConvertTo-Json -Depth 10) }

function Request([string]$Method, [string]$Path, $Body = $null, [string]$Token = '') {
    $arguments = @{ Method = $Method; Uri = "$Url$Path"; TimeoutSec = 15; MaximumRedirection = 0 }
    if ($Token) { $arguments.Headers = @{ 'X-Authorization' = "Bearer $Token" } }
    if ($null -ne $Body) {
        $arguments.ContentType = 'application/json'
        $arguments.Body = ConvertTo-Json -InputObject $Body -Depth 10 -Compress
    }
    try { Invoke-RestMethod @arguments } catch { throw 'thingsboard_bootstrap_request_failed' }
}

function Login([string]$Email, [string]$Password) {
    try { (Request POST '/api/auth/login' @{ username = $Email; password = $Password }).token } catch { $null }
}

if (Test-Path -LiteralPath $StateFile) {
    $state = Get-Content -LiteralPath $StateFile -Raw | ConvertFrom-Json -AsHashtable
    if ($state.version -ne 1 -or $state.url -ne $Url -or -not $state.adminPassword -or -not $state.userPassword -or -not $state.labId) {
        throw 'invalid_bootstrap_state'
    }
} else {
    $labId = [Guid]::NewGuid().ToString('N')
    $state = @{
        version = 1; url = $Url; labId = $labId; adminChanged = $false
        adminPassword = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
        userPassword = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
        tenantTitle = "DOSO telemetry lab $labId"; userEmail = "telemetry-$labId@doso.invalid"
    }
    SaveState
}

$adminToken = Login 'sysadmin@thingsboard.org' $state.adminPassword
if (-not $adminToken) {
    if ($state.adminChanged) { throw 'saved_admin_credentials_rejected' }
    $adminToken = Login 'sysadmin@thingsboard.org' 'sysadmin'
    if (-not $adminToken) { throw 'fresh_install_admin_credentials_rejected' }
    $changed = Request POST '/api/auth/changePassword' @{ currentPassword = 'sysadmin'; newPassword = $state.adminPassword } $adminToken
    $adminToken = $changed.token
    if (-not $adminToken) { $adminToken = Login 'sysadmin@thingsboard.org' $state.adminPassword }
    if (-not $adminToken) { throw 'changed_admin_credentials_unconfirmed' }
}
$state.adminChanged = $true
SaveState

if (-not $state.tenantId) {
    $search = [Uri]::EscapeDataString($state.tenantTitle)
    $tenants = Request GET "/api/tenants?pageSize=100&page=0&textSearch=$search" $null $adminToken
    $matching = @($tenants.data | Where-Object { $_.title -eq $state.tenantTitle })
    if ($matching.Count -gt 1) { throw 'ambiguous_bootstrap_tenant' }
    $tenant = if ($matching.Count) { $matching[0] } else { Request POST '/api/tenant' @{ title = $state.tenantTitle } $adminToken }
    $state.tenantId = $tenant.id.id
    if (-not $state.tenantId) { throw 'invalid_bootstrap_tenant' }
    SaveState
}

if (-not $state.userId) {
    $search = [Uri]::EscapeDataString($state.userEmail)
    $users = Request GET "/api/tenant/$($state.tenantId)/users?pageSize=100&page=0&textSearch=$search" $null $adminToken
    $matching = @($users.data | Where-Object { $_.email -eq $state.userEmail })
    if ($matching.Count -gt 1) { throw 'ambiguous_bootstrap_user' }
    $user = if ($matching.Count) { $matching[0] } else {
        Request POST '/api/user?sendActivationMail=false' @{
            tenantId = @{ entityType = 'TENANT'; id = $state.tenantId }
            email = $state.userEmail; authority = 'TENANT_ADMIN'; firstName = 'Telemetry'
        } $adminToken
    }
    if ($user.authority -ne 'TENANT_ADMIN' -or $user.tenantId.id -ne $state.tenantId) { throw 'invalid_bootstrap_user' }
    $state.userId = $user.id.id
    SaveState
}

$userToken = Login $state.userEmail $state.userPassword
if (-not $userToken) {
    $link = Request GET "/api/user/$($state.userId)/activationLink" $null $adminToken
    if ($link -notmatch '(?:\?|&)activateToken=([^&]+)') { throw 'invalid_bootstrap_activation_link' }
    $activated = Request POST '/api/noauth/activate?sendActivationMail=false' @{
        activateToken = [Uri]::UnescapeDataString($Matches[1]); password = $state.userPassword
    }
    $userToken = $activated.token
    if (-not $userToken) { throw 'bootstrap_activation_unconfirmed' }
}

if (-not $state.apiKey -or $state.apiKeyExpiresAt -le [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()) {
    $expires = [DateTimeOffset]::UtcNow.AddDays(7).ToUnixTimeMilliseconds()
    $apiKey = Request POST '/api/apiKey' @{
        userId = @{ entityType = 'USER'; id = $state.userId }
        description = "DOSO telemetry lab $($state.labId)"; enabled = $true; expirationTime = $expires
    } $userToken
    $value = $apiKey.value -replace '^ApiKey\s+', ''
    if ($value.Length -lt 32 -or $value.Length -gt 256 -or $value -notmatch '^[\x21-\x7e]+$') { throw 'invalid_bootstrap_api_key' }
    $state.apiKey = $value
    $state.apiKeyExpiresAt = $expires
    SaveState
}
SavePrivate (Join-Path $privateDirectory 'thingsboard.env') "DOSO_THINGSBOARD_URL=$Url`nDOSO_THINGSBOARD_KEY=$($state.apiKey)`n"
[pscustomobject]@{ initialized = $true; tenantId = $state.tenantId; userId = $state.userId; apiKeyExpiresAt = $state.apiKeyExpiresAt }
