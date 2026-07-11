param([string]$OutputPath = "target/validation/reference-machine.json")

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if (-not $IsWindows) { throw "The Windows reference report must be captured on Windows." }

function Read-CommandVersion([string]$Command, [string[]]$Arguments) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) { return $null }
    return ((& $Command @Arguments 2>&1 | Select-Object -First 1) -join "").Trim()
}

$os = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object {
    [ordered]@{ name = $_.Name; driver_version = $_.DriverVersion; adapter_ram_bytes = [int64]$_.AdapterRAM; video_processor = $_.VideoProcessor }
})
$gitStatus = @(git status --porcelain)
$report = [ordered]@{
    schema_version = 1
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
    profile = "windows-alpha-reference-v1"
    git = [ordered]@{ revision = (git rev-parse HEAD).Trim(); dirty = $gitStatus.Count -gt 0 }
    os = [ordered]@{ caption = $os.Caption; version = $os.Version; build_number = $os.BuildNumber; architecture = $os.OSArchitecture }
    cpu = [ordered]@{ names = @($cpu.Name); physical_cores = ($cpu | Measure-Object NumberOfCores -Sum).Sum; logical_processors = ($cpu | Measure-Object NumberOfLogicalProcessors -Sum).Sum }
    memory_bytes = [int64]$os.TotalVisibleMemorySize * 1024
    gpus = $gpus
    tools = [ordered]@{ rustc = Read-CommandVersion "rustc" @("--version"); cargo = Read-CommandVersion "cargo" @("--version"); ffmpeg = Read-CommandVersion "ffmpeg" @("-version") }
}

$parent = Split-Path -Parent $OutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
Write-Host "Windows reference report written to $OutputPath"
