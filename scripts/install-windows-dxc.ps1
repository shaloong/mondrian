# Install the exact supported Windows DX12 compiler runtime. CI and Release
# consume this provider; this is not physical GPU qualification.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$DestinationPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$release = 'v1.8.2505.1'
$archiveHash = '9ad895a6b039e3a8f8c22a1009f866800b840a74b50db9218d13319e215ea8a4'
$root = [IO.Path]::GetFullPath($DestinationPath)
New-Item -ItemType Directory -Path $root -Force | Out-Null
$archive = Join-Path $root 'dxc_2025_07_14.zip'
if (-not (Test-Path $archive -PathType Leaf)) {
    Invoke-WebRequest -Uri "https://github.com/microsoft/DirectXShaderCompiler/releases/download/$release/dxc_2025_07_14.zip" -OutFile $archive
}
if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $archiveHash) {
    throw 'Microsoft DXC release archive SHA-256 mismatch'
}
Expand-Archive -Path $archive -DestinationPath $root -Force
$bin = Join-Path $root 'bin/x64'
foreach ($name in @('dxcompiler.dll', 'dxil.dll', 'dxc.exe')) {
    $file = Join-Path $bin $name
    if (-not (Test-Path $file -PathType Leaf)) { throw "Missing DXC runtime file: $name" }
    Write-Host "$name SHA-256=$((Get-FileHash $file -Algorithm SHA256).Hash.ToLowerInvariant()) version=$((Get-Item $file).VersionInfo.FileVersion)"
}
foreach ($name in @('LICENSE-LLVM.txt', 'LICENSE-MIT.txt', 'LICENSE-MS.txt')) {
    if (-not (Test-Path (Join-Path $root $name) -PathType Leaf)) { throw "Missing DXC license: $name" }
}
$version = & (Join-Path $bin 'dxc.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'DXC version probe failed' }
Write-Host ($version -join [Environment]::NewLine)
[pscustomobject]@{
    Release = $release
    ArchiveSha256 = $archiveHash
    RootDirectory = $root
    BinDirectory = $bin
}
