$ErrorActionPreference = 'Stop'
$cacheRoot = 'C:\kithara-ci\cache'
$bin = 'C:\kithara-ci\bin'
New-Item -ItemType Directory -Force -Path $bin, "$cacheRoot\kache" | Out-Null
Get-Verified -Url $settings.kache_url -Sha256 $settings.kache_sha256 -Path "$bin\kache.exe"
Copy-Item 'E:\kache-host.toml' "$cacheRoot\kache\host.toml" -Force
$cacheEnvironment = @{}
foreach ($line in Get-Content 'E:\cache.env') {
    if (-not $line) { continue }
    $key, $value = $line -split '=', 2
    if ($key -notmatch '^(AWS_ACCESS_KEY_ID|AWS_SECRET_ACCESS_KEY|AWS_EC2_METADATA_DISABLED|SCCACHE_BUCKET|SCCACHE_ENDPOINT|SCCACHE_REGION|SCCACHE_S3_USE_SSL|SCCACHE_S3_KEY_PREFIX)$' -or -not $value) {
        throw 'invalid scoped cache environment'
    }
    $cacheEnvironment[$key] = $value
}
$cacheEnvironment['KACHE_HOST_CONFIG'] = "$cacheRoot\kache\host.toml"
$cacheEnvironment['KITHARA_CI_COMPILER_CACHE_ROOT'] = $cacheRoot
$cacheEnvironment['RUSTC_WRAPPER'] = 'kache'
$cacheEnvironment['CMAKE_C_COMPILER_LAUNCHER'] = 'kache'
$cacheEnvironment['CMAKE_CXX_COMPILER_LAUNCHER'] = 'kache'
$cacheEnvironment['CARGO_INCREMENTAL'] = '0'
foreach ($entry in $cacheEnvironment.GetEnumerator()) {
    [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Machine')
    [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
}
$env:PATH = "$bin;$env:PATH"
[Environment]::SetEnvironmentVariable('PATH', "$bin;" + [Environment]::GetEnvironmentVariable('PATH', 'Machine'), 'Machine')
$stats = & "$bin\kache.exe" stats --json | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $stats.daemon_connected -or $stats.daemon_remote_readonly -ne $false -or $stats.remote -notlike 's3://*') {
    throw 'Windows provisioning requires a connected writable RustFS cache'
}
