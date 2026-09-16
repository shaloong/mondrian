param(
    [string]$BuildDirectory = 'target/native/decklink-reference-output',
    [string]$PackageDirectory,
    [ValidateRange(1, 4)][int]$Parallel = 1
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'The API 12.0 COM bridge requires Windows x64/MSVC/MIDL' }
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$source = Join-Path $repository 'native/reference-output-decklink'
$vendor = Join-Path $source 'vendor/decklink-api-12.0'
$manifest = Get-Content -LiteralPath (Join-Path $vendor 'sha256.json') -Raw | ConvertFrom-Json
if (@($manifest).Count -ne 27) { throw 'Pinned BMD interface inventory must contain exactly 27 files' }
foreach ($entry in $manifest) {
    if ($entry.file -notmatch '^DeckLinkAPI[A-Za-z0-9_]*\.(idl|h)$') { throw 'Invalid interface manifest filename' }
    $path = Join-Path $vendor $entry.file
    if ((Get-Item -LiteralPath $path).Length -ne $entry.bytes -or
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.sha256) {
        throw "Pinned interface digest mismatch: $($entry.file)"
    }
    $content = [IO.File]::ReadAllText($path)
    if (-not $content.Contains('Copyright (c)') -or -not $content.Contains('Blackmagic Design') -or
        -not $content.Contains('Permission is hereby granted') -or -not $content.Contains('must be included in all copies')) {
        throw "BMD license notice missing: $($entry.file)"
    }
}
function Resolve-BuildPath([string]$Value) {
    if ([IO.Path]::IsPathRooted($Value)) { return [IO.Path]::GetFullPath($Value) }
    return [IO.Path]::GetFullPath((Join-Path $repository $Value))
}
function Invoke-NativeBuild([string]$Executable, [string[]]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WorkingDirectory = $repository
    # Some launchers expose both PATH and Path. MSBuild's .NET Framework
    # child environment rejects duplicates; rebuild a case-insensitive map.
    $start.Environment.Clear()
    foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
        $start.Environment[[string]$entry.Key] = [string]$entry.Value
    }
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    try { $process.WaitForExit(); if ($process.ExitCode -ne 0) { throw "Native command failed ($($process.ExitCode)): $Executable" } }
    finally { $process.Dispose() }
}
$build = Resolve-BuildPath $BuildDirectory
$cmake = (Get-Command cmake -ErrorAction Stop).Source
Invoke-NativeBuild $cmake @('-S', $source, '-B', $build, '-G', 'Visual Studio 17 2022', '-A', 'x64')
Invoke-NativeBuild $cmake @('--build', $build, '--config', 'Release', '--parallel', [string]$Parallel)
$abi = & (Join-Path $build 'Release/mondrian_decklink_abi_probe.exe')
if ($LASTEXITCODE -ne 0) { throw 'DeckLink ABI/failed-open ownership checks failed' }
$vanc = & (Join-Path $build 'Release/mondrian_decklink_vanc_probe.exe')
if ($LASTEXITCODE -ne 0) { throw 'DeckLink raw VANC boundary checks failed' }
$lifecycle = & (Join-Path $build 'Release/mondrian_decklink_lifecycle_probe.exe')
if ($LASTEXITCODE -ne 0) { throw 'DeckLink native owner fault checks failed' }
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText((Join-Path $build 'native-abi-probe.json'), [string]$abi, $utf8)
[IO.File]::WriteAllText((Join-Path $build 'native-vanc-probe.json'), [string]$vanc, $utf8)
[IO.File]::WriteAllText((Join-Path $build 'native-lifecycle-probe.json'), [string]$lifecycle, $utf8)
$image = Join-Path $build 'Release/mondrian_reference_decklink.dll'
$hash = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
$packaged = $null
if ($PackageDirectory) {
    $package = Resolve-BuildPath $PackageDirectory
    New-Item -ItemType Directory -Force -Path $package | Out-Null
    $packaged = Join-Path $package 'mondrian_reference_decklink.dll'
    Copy-Item -LiteralPath $image -Destination $packaged
    if ((Get-FileHash -LiteralPath $packaged -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw 'Packaged native image digest mismatch' }
}
$receipt = [ordered]@{
    schema_version = 1; qualifying = $false; api_version = '12.0'; c_abi_version = 1
    interface_repository = 'https://github.com/obsproject/obs-studio'
    interface_commit = '671fb57daf4972fcd506689a48a474dd4eda9e66'
    interface_manifest_sha256 = (Get-FileHash -LiteralPath (Join-Path $vendor 'sha256.json') -Algorithm SHA256).Hash.ToLowerInvariant()
    image_path = $image; image_sha256 = $hash; packaged_path = $packaged
    limitation = 'Build/ABI evidence only; SDK16, Desktop Video driver installation and physical SDI/reference qualification are not implied'
} | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText((Join-Path $build 'native-build-receipt.json'), $receipt, $utf8)
Write-Output "MONDRIAN_DECKLINK_NATIVE_IMAGE=$image"
