param(
    [string]$SdkDirectory,
    [string]$BuildDirectory = "target/native/aja-reference-output",
    [string]$PackageDirectory,
    [ValidateRange(1,16)][int]$Parallel = 2
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$pinnedRevision = "3a23acd800cd05f40434ede8c714bd0271c94f53"
function Resolve-TaskPath([string]$Value) {
    if ([IO.Path]::IsPathRooted($Value)) { return [IO.Path]::GetFullPath($Value) }
    return [IO.Path]::GetFullPath((Join-Path $repository $Value))
}
if (-not $SdkDirectory) { $SdkDirectory = "target/native-deps/libajantv2-$pinnedRevision" }
$sdk = Resolve-TaskPath $SdkDirectory
$build = Resolve-TaskPath $BuildDirectory
if (-not (Test-Path -LiteralPath $sdk)) {
    New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($sdk)) | Out-Null
    & git clone --no-checkout https://github.com/aja-video/libajantv2.git $sdk
    if ($LASTEXITCODE -ne 0) { throw "Official AJA SDK clone failed" }
    & git -C $sdk checkout --detach $pinnedRevision
    if ($LASTEXITCODE -ne 0) { throw "Pinned AJA SDK checkout failed" }
}
$revision = ([string](& git -C $sdk rev-parse HEAD)).Trim()
if ($LASTEXITCODE -ne 0 -or $revision -ne $pinnedRevision) { throw "AJA SDK must use exact commit $pinnedRevision" }
$dirty = @(& git -C $sdk status --porcelain --untracked-files=normal)
if ($LASTEXITCODE -ne 0 -or $dirty.Count -ne 0) { throw "Pinned SDK has local modifications or unexpected files" }
$cmake = (Get-Command cmake -ErrorAction Stop).Source
$source = Join-Path $repository "native/reference-output-aja"
$cachePath = Join-Path $build "CMakeCache.txt"
$needsConfigure = $true
if (Test-Path -LiteralPath $cachePath -PathType Leaf) {
    $cachedSdk = $null
    $cachedSource = $null
    foreach ($line in [IO.File]::ReadLines($cachePath)) {
        if ($line -match '^MONDRIAN_AJA_SDK:[^=]+=(.+)$') { $cachedSdk = [IO.Path]::GetFullPath($Matches[1]) }
        if ($line -match '^CMAKE_HOME_DIRECTORY:[^=]+=(.+)$') { $cachedSource = [IO.Path]::GetFullPath($Matches[1]) }
    }
    $needsConfigure = $cachedSdk -ne $sdk -or $cachedSource -ne $source
}
if ($needsConfigure) {
    & $cmake -S $source -B $build -A x64 "-DMONDRIAN_AJA_SDK=$sdk"
    if ($LASTEXITCODE -ne 0) { throw "AJA CMake configuration failed" }
}
& $cmake --build $build --config Release --target mondrian_aja_abi_probe mondrian_aja_vanc_probe --parallel $Parallel
if ($LASTEXITCODE -ne 0) { throw "AJA native bridge build failed" }
$probeResult = & (Join-Path $build "Release/mondrian_aja_abi_probe.exe")
if ($LASTEXITCODE -ne 0) { throw "AJA native ABI / rejected ownership probe failed" }
[IO.File]::WriteAllText((Join-Path $build "native-abi-probe.json"),[string]$probeResult,[Text.UTF8Encoding]::new($false))
$vancProbe = & (Join-Path $build "Release/mondrian_aja_vanc_probe.exe")
if ($LASTEXITCODE -ne 0) { throw "AJA exact VANC raster boundary probe failed" }
[IO.File]::WriteAllText((Join-Path $build "native-vanc-probe.json"),[string]$vancProbe,[Text.UTF8Encoding]::new($false))
$image = Join-Path $build "Release/mondrian_reference_aja.dll"
if (-not (Test-Path -LiteralPath $image -PathType Leaf)) { throw "Expected native bridge image is missing" }
$hash = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
$packaged = $null
if ($PackageDirectory) {
    $package = Resolve-TaskPath $PackageDirectory
    New-Item -ItemType Directory -Force -Path $package | Out-Null
    $packaged = Join-Path $package "mondrian_reference_aja.dll"
    Copy-Item -LiteralPath $image -Destination $packaged
    if ((Get-FileHash -LiteralPath $packaged -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw "Packaged AJA image digest mismatch" }
}
$buildReceipt = [ordered]@{
    schema_version = 1
    qualifying = $false
    sdk_repository = "https://github.com/aja-video/libajantv2"
    sdk_revision = $pinnedRevision
    sdk_version = "18.1.0"
    c_abi_version = 1
    image_path = $image
    image_sha256 = $hash
    packaged_path = $packaged
    limitation = "Build evidence only; no driver installation, SDI device, reference lock or wire qualification is implied"
} | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText((Join-Path $build "native-build-receipt.json"),$buildReceipt,[Text.UTF8Encoding]::new($false))
Write-Output "MONDRIAN_AJA_NATIVE_IMAGE=$image"
