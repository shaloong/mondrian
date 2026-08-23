param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineId,
    [string]$OutputPath = "target/validation/reference-machine.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if (-not $IsWindows) { throw "The Windows reference report must be captured on Windows." }
if ($MachineId -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$') {
    throw "MachineId must be an opaque 1-64 character label containing only letters, digits, '.', '_' or '-'."
}

function Read-CommandVersion([string]$Command, [string[]]$Arguments) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) { return $null }
    return ((& $Command @Arguments 2>&1 | Select-Object -First 1) -join "").Trim()
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$absoluteOutputPath = if ([IO.Path]::IsPathRooted($OutputPath)) { [IO.Path]::GetFullPath($OutputPath) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $OutputPath)) }
$os = Get-CimInstance Win32_OperatingSystem
$cpu = @(Get-CimInstance Win32_Processor)
$memoryModules = @(Get-CimInstance Win32_PhysicalMemory)
$installedMemoryBytes = ($memoryModules | Measure-Object Capacity -Sum).Sum
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object {
    [ordered]@{
        name = [string]$_.Name
        driver_version = [string]$_.DriverVersion
        adapter_ram_bytes = if ($null -eq $_.AdapterRAM) { $null } else { [uint64]$_.AdapterRAM }
        video_processor = [string]$_.VideoProcessor
    }
})
$storage = @(Get-CimInstance Win32_DiskDrive | ForEach-Object {
    [ordered]@{
        model = [string]$_.Model
        firmware_revision = [string]$_.FirmwareRevision
        interface_type = [string]$_.InterfaceType
        media_type = [string]$_.MediaType
        size_bytes = if ($null -eq $_.Size) { $null } else { [uint64]$_.Size }
    }
})
$volumes = @(Get-CimInstance Win32_LogicalDisk -Filter "DriveType = 3" | ForEach-Object {
    [ordered]@{
        drive = [string]$_.DeviceID
        file_system = [string]$_.FileSystem
        size_bytes = if ($null -eq $_.Size) { $null } else { [uint64]$_.Size }
        free_bytes = if ($null -eq $_.FreeSpace) { $null } else { [uint64]$_.FreeSpace }
    }
})
$displays = @(Get-CimInstance Win32_DesktopMonitor | ForEach-Object {
    [ordered]@{
        name = [string]$_.Name
        monitor_type = [string]$_.MonitorType
        width = if ($null -eq $_.ScreenWidth) { $null } else { [uint32]$_.ScreenWidth }
        height = if ($null -eq $_.ScreenHeight) { $null } else { [uint32]$_.ScreenHeight }
    }
})
$gitStatus = @(git -C $repositoryRoot status --porcelain)
if ($LASTEXITCODE -ne 0) { throw "Cannot read Git working-tree state." }
$gitRevision = (git -C $repositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw "Cannot resolve Git revision." }
$report = [ordered]@{
    schema_version = 3
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
    profile = "windows-alpha-reference-v2"
    machine_id = $MachineId
    privacy = [ordered]@{
        hardware_serials_collected = $false
        machine_id_is_operator_assigned = $true
    }
    git = [ordered]@{
        revision = $gitRevision
        dirty = $gitStatus.Count -gt 0
    }
    os = [ordered]@{
        caption = [string]$os.Caption
        version = [string]$os.Version
        build_number = [int]$os.BuildNumber
        architecture = [string]$os.OSArchitecture
    }
    cpu = [ordered]@{
        names = @($cpu | ForEach-Object { [string]$_.Name })
        physical_cores = [int](($cpu | Measure-Object NumberOfCores -Sum).Sum)
        logical_processors = [int](($cpu | Measure-Object NumberOfLogicalProcessors -Sum).Sum)
    }
    installed_memory_bytes = if ($null -eq $installedMemoryBytes) { $null } else { [uint64]$installedMemoryBytes }
    visible_memory_bytes = [uint64]$os.TotalVisibleMemorySize * 1024
    gpus = $gpus
    storage = $storage
    volumes = $volumes
    displays = $displays
    execution_scoped_capabilities = @("d3d12", "native-video-import", "hdr-output-state")
    tools = [ordered]@{
        rustc = Read-CommandVersion "rustc" @("--version")
        cargo = Read-CommandVersion "cargo" @("--version")
        ffmpeg = Read-CommandVersion "ffmpeg" @("-version")
        ffprobe = Read-CommandVersion "ffprobe" @("-version")
    }
}

$parent = Split-Path -Parent $absoluteOutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $absoluteOutputPath -Encoding utf8
Write-Host "Windows reference report written to $absoluteOutputPath"
