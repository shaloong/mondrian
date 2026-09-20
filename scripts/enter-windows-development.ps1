# Activate in the current PowerShell process; does not change the global PATH.
[CmdletBinding()]
param(
    [string]$VcpkgRoot = $env:VCPKG_ROOT,
    [string]$LibClangDirectory = $env:LIBCLANG_PATH,
    [string]$TargetDirectory = $env:CARGO_TARGET_DIR
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This development environment requires PowerShell 7 on Windows.' }
if ([string]::IsNullOrWhiteSpace($VcpkgRoot)) {
    $VcpkgRoot = Join-Path $env:SystemDrive 'vcpkg'
}
if ([string]::IsNullOrWhiteSpace($LibClangDirectory)) {
    $LibClangDirectory = Join-Path $env:ProgramFiles 'LLVM/bin'
}
$VcpkgRoot = (Resolve-Path -LiteralPath $VcpkgRoot).Path
$LibClangDirectory = (Resolve-Path -LiteralPath $LibClangDirectory).Path
$prefix = Join-Path $VcpkgRoot 'installed/x64-windows'
$pkgconf = Join-Path $prefix 'tools/pkgconf/pkgconf.exe'
$ffmpegTools = Join-Path $prefix 'tools/ffmpeg'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
foreach ($file in @(
    $vswhere,
    (Join-Path $VcpkgRoot 'vcpkg.exe'),
    (Join-Path $LibClangDirectory 'libclang.dll'),
    (Join-Path $prefix 'include/libavcodec/avcodec.h'),
    (Join-Path $prefix 'lib/avcodec.lib'),
    $pkgconf,
    (Join-Path $ffmpegTools 'ffmpeg.exe'),
    (Join-Path $ffmpegTools 'ffprobe.exe')
)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "Missing development dependency: $file. See docs/dev/windows-development.md."
    }
}
$visualStudio = (& $vswhere -latest -products '*' `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath)
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($visualStudio)) {
    throw 'Visual Studio with the MSVC x64 toolset is required.'
}
$vsDevCmd = Join-Path $visualStudio.Trim() 'Common7/Tools/VsDevCmd.bat'
if (-not (Test-Path -LiteralPath $vsDevCmd -PathType Leaf)) {
    throw "Missing Visual Studio environment entry point: $vsDevCmd"
}
# cmd.exe cannot use a UNC current directory. Restore the caller's location.
Push-Location $env:TEMP
try {
    $environment = & $env:ComSpec /d /s /c `
        ('call "' + $vsDevCmd + '" -no_logo -arch=x64 -host_arch=x64 >nul && set')
    if ($LASTEXITCODE -ne 0) { throw "VsDevCmd failed: $LASTEXITCODE" }
} finally {
    Pop-Location
}
$activatedEnvironment = [System.Collections.Generic.Dictionary[string, string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase
)
foreach ($line in $environment) {
    if ($line -match '^([^=]+)=(.*)$') {
        $name = $Matches[1]
        $value = $Matches[2]
        # cmd.exe can retain an inherited `Path` entry alongside the `PATH`
        # emitted by VsDevCmd. Keep the activated value rather than allowing
        # the inherited spelling to erase the MSVC tool directories.
        if ($name -ceq 'PATH' -or -not $activatedEnvironment.ContainsKey($name)) {
            $activatedEnvironment[$name] = $value
        }
    }
}
foreach ($entry in $activatedEnvironment.GetEnumerator()) {
    [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
}
foreach ($tool in @('cl.exe', 'cmake.exe', 'ninja.exe')) {
    if (-not (Get-Command $tool -CommandType Application -ErrorAction SilentlyContinue)) {
        throw "Missing $tool. Install Visual Studio Desktop development with C++ and CMake tools."
    }
}
# Keep bundled CMake dependencies on the activated MSVC toolchain.
$env:CC = (Get-Command cl.exe -CommandType Application).Source
$env:CXX = $env:CC
$env:ASM = $env:CC
$env:CMAKE_GENERATOR = 'Ninja'
$env:VCPKG_ROOT = $VcpkgRoot
$env:VCPKGRS_TRIPLET = 'x64-windows'
$env:VCPKG_DEFAULT_TRIPLET = 'x64-windows'
$env:VCPKGRS_DYNAMIC = '1'
$env:VCPKG_MAX_CONCURRENCY = '2'
$env:FFMPEG_DIR = $prefix
$env:PKG_CONFIG = $pkgconf
$env:PKG_CONFIG_PATH = Join-Path $prefix 'lib/pkgconfig'
$env:PKG_CONFIG_ALLOW_CROSS = '1'
$env:PKG_CONFIG_ALLOW_SYSTEM_LIBS = '1'
$env:PKG_CONFIG_ALLOW_SYSTEM_CFLAGS = '1'
$env:LIBCLANG_PATH = $LibClangDirectory
$env:Path = (@(
    (Join-Path $prefix 'bin'),
    $ffmpegTools,
    (Split-Path -Parent $pkgconf),
    $LibClangDirectory,
    (Join-Path $env:ProgramFiles 'Git/bin'),
    $env:Path
) -join ';')
# An SDK installed after this terminal started is visible in the registry.
if ([string]::IsNullOrWhiteSpace($env:VULKAN_SDK)) {
    $sdk = [Environment]::GetEnvironmentVariable('VULKAN_SDK', 'User')
    if ([string]::IsNullOrWhiteSpace($sdk)) {
        $sdk = [Environment]::GetEnvironmentVariable('VULKAN_SDK', 'Machine')
    }
    if (-not [string]::IsNullOrWhiteSpace($sdk)) { $env:VULKAN_SDK = $sdk }
}
if (-not [string]::IsNullOrWhiteSpace($env:VULKAN_SDK)) {
    $sdkBin = Join-Path $env:VULKAN_SDK 'Bin'
    if (-not (Test-Path -LiteralPath $sdkBin -PathType Container)) {
        throw "VULKAN_SDK points to an incomplete installation: $env:VULKAN_SDK"
    }
    $env:Path = $sdkBin + ';' + $env:Path
}
if ([string]::IsNullOrWhiteSpace($TargetDirectory)) {
    # Build on local NTFS even when the source checkout is on SMB.
    $TargetDirectory = Join-Path $env:LOCALAPPDATA 'Mondrian/target-windows'
}
$env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($TargetDirectory)
New-Item -ItemType Directory -Path $env:CARGO_TARGET_DIR -Force | Out-Null
$codecVersion = & $pkgconf --modversion libavcodec
if ($LASTEXITCODE -ne 0 -or $codecVersion -notmatch '^62\.') {
    throw "Expected the repository's FFmpeg 8 product profile (libavcodec 62.x), got '$codecVersion'."
}
foreach ($library in @('libavutil', 'libavformat', 'libavfilter', 'libswscale', 'libswresample')) {
    $version = & $pkgconf --modversion $library
    if ($LASTEXITCODE -ne 0) { throw "Cannot resolve $library in the selected FFmpeg installation." }
    Write-Host "$library $version"
}
Write-Host "Windows x64 development environment ready; libavcodec $codecVersion"
Write-Host "Cargo output: $env:CARGO_TARGET_DIR"
