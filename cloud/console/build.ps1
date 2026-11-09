[CmdletBinding()]
param(
    [string]$SourcePath = 'D:\DOSO\vendor\thingsboard-4.3.1.8',
    [string]$YarnPath = 'D:\DOSO\toolchains\yarn-1.22.22\node_modules\yarn\bin\yarn.js',
    [string]$NginxImage = 'nginx:1.30.6-alpine@sha256:583b7f2fce1cdb8da5ad5c39ebd3c33b98b87d493190e548bdee1fa8c619c311',
    [switch]$SkipInstall,
    [switch]$SkipCompile,
    [switch]$SkipImage
)

$ErrorActionPreference = 'Stop'
$commit = 'ebbfc6789b81a4e04cae2711208130fb2841b31f'
$SourcePath = (Resolve-Path -LiteralPath $SourcePath).Path
$ui = Join-Path $SourcePath 'ui-ngx'
$patch = Join-Path $PSScriptRoot 'patches/doso.patch'
$output = Join-Path $ui 'target/generated-resources/public'
$compileRecord = Join-Path $output 'doso-build.json'
if (-not (Test-Path -LiteralPath $patch)) { throw 'DOSO source patch is missing.' }
if ((& git -C $SourcePath rev-parse HEAD).Trim() -ne $commit -or $LASTEXITCODE -ne 0) {
    throw 'The source must be ThingsBoard CE v4.3.1.8 at the recorded commit.'
}
if (-not $SkipImage -and $NginxImage -notmatch '^nginx:[a-zA-Z0-9._-]+@sha256:[a-f0-9]{64}$') {
    throw 'Pass the verified official Nginx image tag and sha256 digest.'
}

& git -C $SourcePath apply --reverse --check $patch 2>$null
if ($LASTEXITCODE -ne 0) {
    if (& git -C $SourcePath status --porcelain) {
        throw 'Source has other changes and does not match the DOSO patch; preserve and inspect them first.'
    }
    & git -C $SourcePath apply --check $patch
    if ($LASTEXITCODE -ne 0) { throw 'DOSO patch does not apply to this source.' }
    & git -C $SourcePath apply $patch
    if ($LASTEXITCODE -ne 0) { throw 'Applying DOSO patch failed.' }
}

function AssertSourcePatch {
    $actual = (& git -C $SourcePath diff --binary HEAD) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Reading source changes failed.' }
    $expected = (Get-Content -LiteralPath $patch -Raw).Replace("`r`n", "`n")
    $untracked = & git -C $SourcePath ls-files --others --exclude-standard
    if ($LASTEXITCODE -ne 0 -or $untracked -or $actual.TrimEnd() -cne $expected.TrimEnd()) {
        throw 'Source changes differ from the recorded DOSO patch; preserve and inspect them first.'
    }
}
AssertSourcePatch
$nodeVersion = (& node --version).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Reading Node version failed.' }
$compilation = [ordered]@{
    sourceCommit = $commit
    patchSha256 = (Get-FileHash -LiteralPath $patch -Algorithm SHA256).Hash.ToLowerInvariant()
    yarnLockSha256 = (Get-FileHash -LiteralPath (Join-Path $ui 'yarn.lock') -Algorithm SHA256).Hash.ToLowerInvariant()
    nodeVersion = $nodeVersion
}
if ($SkipCompile) {
    if (-not (Test-Path -LiteralPath $compileRecord)) { throw 'A verified compilation record is required for SkipCompile.' }
    $previousCompilation = Get-Content -LiteralPath $compileRecord -Raw | ConvertFrom-Json
    foreach ($entry in $compilation.GetEnumerator()) {
        if ($previousCompilation.($entry.Key) -cne $entry.Value) { throw 'Frontend output does not match the current build inputs; compile again.' }
    }
}

$previousWorkers = $env:NG_BUILD_MAX_WORKERS
$previousConcurrency = $env:CHILD_CONCURRENCY
$previousParallelTs = $env:NG_BUILD_PARALLEL_TS
Push-Location $ui
try {
    if (-not $SkipInstall) {
        if ((& node $YarnPath --version).Trim() -ne '1.22.22' -or $LASTEXITCODE -ne 0) {
            throw 'Yarn 1.22.22 is required.'
        }
        $env:CHILD_CONCURRENCY = '1'
        & node $YarnPath install --frozen-lockfile --non-interactive --network-concurrency 2 --network-timeout 100000
        if ($LASTEXITCODE -ne 0) { throw 'Installing locked frontend dependencies failed.' }
    }
    if (-not $SkipCompile) {
        if (Test-Path -LiteralPath $compileRecord) { Remove-Item -LiteralPath $compileRecord }
        $env:NG_BUILD_MAX_WORKERS = '1'
        $env:NG_BUILD_PARALLEL_TS = '0'
        & node --max-old-space-size=4096 ./node_modules/@angular/cli/bin/ng build --configuration production
        if ($LASTEXITCODE -ne 0) { throw 'Building the DOSO frontend failed.' }
        AssertSourcePatch
        $compilation | ConvertTo-Json | Set-Content -LiteralPath $compileRecord
    }
} finally {
    Pop-Location
    $env:NG_BUILD_MAX_WORKERS = $previousWorkers
    $env:CHILD_CONCURRENCY = $previousConcurrency
    $env:NG_BUILD_PARALLEL_TS = $previousParallelTs
}

if (-not (Test-Path -LiteralPath (Join-Path $output 'index.html'))) { throw 'Frontend output index.html is missing.' }
if (-not (Test-Path -LiteralPath (Join-Path $output '3rdpartylicenses.txt'))) { throw 'Frontend third-party notices are missing.' }
$buildId = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
$stage = Join-Path $PSScriptRoot ".build/$buildId"
[void][IO.Directory]::CreateDirectory((Join-Path $stage 'dist'))
[void][IO.Directory]::CreateDirectory((Join-Path $stage 'licenses'))
Get-ChildItem -LiteralPath $output -Force | Copy-Item -Destination (Join-Path $stage 'dist') -Recurse
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'nginx.conf'),(Join-Path $PSScriptRoot 'Dockerfile') -Destination $stage
Copy-Item -LiteralPath (Join-Path $SourcePath 'LICENSE') -Destination (Join-Path $stage 'licenses/Apache-2.0.txt')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'NOTICE') -Destination (Join-Path $stage 'licenses/NOTICE.txt')
Copy-Item -LiteralPath (Join-Path $output '3rdpartylicenses.txt') -Destination (Join-Path $stage 'licenses/3rdpartylicenses.txt')
Get-ChildItem -LiteralPath $SourcePath -File -Filter '*NOTICE*' | Copy-Item -Destination (Join-Path $stage 'licenses')
@{
    sourceCommit = $commit
    patchSha256 = $compilation.patchSha256
    yarnLockSha256 = $compilation.yarnLockSha256
    builtAtUtc = [DateTime]::UtcNow.ToString('o')
    nginxImage = $NginxImage
    nodeVersion = $nodeVersion
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stage 'manifest.json')
if (-not $SkipImage) {
    & docker build --build-arg "NGINX_IMAGE=$NginxImage" --tag doso/console:4.3.1.8 $stage
    if ($LASTEXITCODE -ne 0) { throw 'Building the DOSO console image failed.' }
}
[pscustomobject]@{ stage = $stage; image = if ($SkipImage) { $null } else { 'doso/console:4.3.1.8' } }
