#requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$BlenderExecutable,
    [Parameter(Mandatory)][string]$RequestPath,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(1, 3600)][int]$TimeoutSeconds = 600
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$requestAbsolute = [IO.Path]::GetFullPath($RequestPath)
$outputAbsolute = [IO.Path]::GetFullPath($OutputDirectory)
$blenderAbsolute = [IO.Path]::GetFullPath($BlenderExecutable)
$adapter = Join-Path $PSScriptRoot 'cross-application/capture_blender.py'
if (Test-Path -LiteralPath $outputAbsolute) { throw 'Capture requires a fresh output directory.' }
$leases = [Collections.Generic.List[IO.FileStream]]::new()
$paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)

function Open-CaptureLease([string]$Path) {
    $absolute = [IO.Path]::GetFullPath($Path)
    if (-not $paths.Add($absolute)) { return }
    $item = Get-Item -LiteralPath $absolute -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Capture input must be a direct regular file: $absolute"
    }
    $leases.Add([IO.File]::Open($absolute, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read))
}

function Resolve-CaptureInput([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path (Split-Path $requestAbsolute) $Path))
}

$process = $null
$nativeExit = $false
$primary = $null
$cleanupErrors = [Collections.Generic.List[string]]::new()
$logPrefix = $null
$exitCode = $null
$ocioInventory = @()
try {
    Open-CaptureLease $requestAbsolute
    if ((Get-Item -LiteralPath $requestAbsolute).Length -gt 1048576) { throw 'Capture request exceeds 1 MiB.' }
    $request = Get-Content -LiteralPath $requestAbsolute -Raw | ConvertFrom-Json
    Open-CaptureLease $blenderAbsolute
    Open-CaptureLease $adapter
    Open-CaptureLease (Resolve-CaptureInput $request.project)
    $ocio = Resolve-CaptureInput $request.ocio
    Open-CaptureLease $ocio
    foreach ($dependency in $request.dependencies) { Open-CaptureLease (Resolve-CaptureInput $dependency.path) }
    # LUT bytes selected by the OCIO config stay leased across process startup.
    $ocioFiles = @(Get-ChildItem -LiteralPath (Split-Path $ocio) -File -Recurse)
    if ($ocioFiles.Count -gt 4096) { throw 'OCIO dependency inventory exceeds its bound.' }
    $ocioInventory = foreach ($file in $ocioFiles) {
        Open-CaptureLease $file.FullName
        [ordered]@{ path = $file.FullName; sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    $parent = Split-Path $outputAbsolute
    $null = New-Item -ItemType Directory -Force -Path $parent
    $logPrefix = Join-Path $parent ([IO.Path]::GetFileName($outputAbsolute) + '.launcher')
    if (Test-Path -LiteralPath ($logPrefix + '.json')) { $logPrefix = $null; throw 'Launcher receipt already exists.' }
    $start = [Diagnostics.ProcessStartInfo]::new($blenderAbsolute)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.Environment['OCIO'] = $ocio
    foreach ($argument in @('--background', '--factory-startup', '--disable-autoexec', '--python-exit-code', '17', '--python', $adapter, '--', $requestAbsolute, $outputAbsolute)) {
        $start.ArgumentList.Add($argument)
    }
    # Inherit the caller's streams; no unbounded in-memory log capture.
    $process = [Diagnostics.Process]::Start($start)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $process.WaitForExit(100) -and [DateTime]::UtcNow -lt $deadline) { }
    $nativeExit = $process.HasExited
    if (-not $nativeExit) {
        throw "Blender capture deadline elapsed."
    }
    if ($process.ExitCode -ne 0) { throw "Blender capture failed with exit code $($process.ExitCode)." }
    $capturePath = Join-Path $outputAbsolute 'capture.json'
    $capture = Get-Content -LiteralPath $capturePath -Raw | ConvertFrom-Json
    if ($capture.status -cne 'captured' -or $capture.run_id -cne $request.run_id) {
        throw 'Blender returned without a complete same-run acquisition report.'
    }
} catch {
    $primary = $_.Exception.ToString()
} finally {
    if ($null -ne $process) {
        try {
            if (-not $process.HasExited) {
                $process.Kill($true)
                $nativeExit = $process.WaitForExit(5000)
                if (-not $nativeExit) { $cleanupErrors.Add('Blender native process did not settle within cleanup deadline.') }
            } else { $nativeExit = $true }
            if ($nativeExit) { $exitCode = $process.ExitCode }
        } catch { $cleanupErrors.Add($_.Exception.ToString()) }
        finally { $process.Dispose() }
    }
    foreach ($lease in $leases) {
        try { $lease.Dispose() } catch { $cleanupErrors.Add($_.Exception.ToString()) }
    }
}
if ($null -ne $logPrefix) {
    $capturePath = Join-Path $outputAbsolute 'capture.json'
    $captureHash = if (Test-Path -LiteralPath $capturePath -PathType Leaf) {
        (Get-FileHash -LiteralPath $capturePath -Algorithm SHA256).Hash.ToLowerInvariant()
    } else { $null }
    [ordered]@{
        schema_version = 1
        run_id = $request.run_id
        status = $(if ($null -eq $primary -and $cleanupErrors.Count -eq 0 -and $nativeExit) { 'captured' } else { 'failed' })
        native_exit_observed = $nativeExit
        exit_code = $exitCode
        failure = $primary
        cleanup_failures = @($cleanupErrors)
        input_leases_released = ($cleanupErrors.Count -eq 0)
        capture_sha256 = $captureHash
        ocio_inventory = @($ocioInventory)
    } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath ($logPrefix + '.json') -Encoding utf8NoBOM
}
if ($null -ne $primary) { throw $primary }
if ($cleanupErrors.Count -gt 0) { throw ($cleanupErrors -join '; ') }
