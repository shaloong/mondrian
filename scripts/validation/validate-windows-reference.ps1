param(
    [string]$ProfilePath = "tests/validation/windows-alpha-reference.json",
    [string]$MachineReportPath = "target/validation/reference-machine.json",
    [switch]$RequireBaselineEligibility,
    [string]$OutputPath = "target/validation/reference-machine-validation.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Add-Issue([string]$Code, [string]$Expected, [string]$Observed) {
    $script:issues.Add([pscustomobject]@{ code = $Code; expected = $Expected; observed = $Observed })
}

function Resolve-RepositoryPath([string]$Path) {
    $repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Path))
}

$profileAbsolute = Resolve-RepositoryPath $ProfilePath
$reportAbsolute = Resolve-RepositoryPath $MachineReportPath
$outputAbsolute = Resolve-RepositoryPath $OutputPath
$profile = Get-Content -LiteralPath $profileAbsolute -Raw | ConvertFrom-Json
$machine = Get-Content -LiteralPath $reportAbsolute -Raw | ConvertFrom-Json
$issues = [System.Collections.Generic.List[object]]::new()

if ($profile.schema_version -ne 2) { Add-Issue "profile.schema" "2" ([string]$profile.schema_version) }
if ($machine.schema_version -ne 2) { Add-Issue "machine.schema" "2" ([string]$machine.schema_version) }
if ($machine.profile -ne $profile.id) { Add-Issue "machine.profile" $profile.id ([string]$machine.profile) }
if ([string]::IsNullOrWhiteSpace([string]$machine.machine_id)) { Add-Issue "machine.id" "non-empty opaque operator label" "missing" }
if ($machine.privacy.hardware_serials_collected -ne $false) { Add-Issue "machine.privacy" "hardware serials not collected" "serial collection declared" }
if ([int]$machine.os.build_number -lt [int]$profile.os.minimum_build_number) { Add-Issue "machine.os-build" "at least $($profile.os.minimum_build_number)" ([string]$machine.os.build_number) }
if ([string]$machine.os.architecture -notmatch "64") { Add-Issue "machine.architecture" $profile.os.architecture ([string]$machine.os.architecture) }
if ([int]$machine.cpu.logical_processors -lt [int]$profile.hardware.minimum_logical_cpu_count) { Add-Issue "machine.logical-cpu" "at least $($profile.hardware.minimum_logical_cpu_count)" ([string]$machine.cpu.logical_processors) }
$minimumMemoryBytes = [uint64]$profile.hardware.minimum_memory_gib * 1024 * 1024 * 1024
if ([uint64]$machine.memory_bytes -lt $minimumMemoryBytes) { Add-Issue "machine.memory" "at least $minimumMemoryBytes bytes" ([string]$machine.memory_bytes) }
$qualifiedGpus = @($machine.gpus | Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_.name) -and -not [string]::IsNullOrWhiteSpace([string]$_.driver_version) -and $null -ne $_.adapter_ram_bytes -and [uint64]$_.adapter_ram_bytes -gt 0 -and -not [string]::IsNullOrWhiteSpace([string]$_.video_processor) })
if ($profile.hardware.dedicated_gpu_required -and $qualifiedGpus.Count -eq 0) { Add-Issue "machine.gpu" "GPU and driver identity" "none" }
if ($profile.hardware.storage_identity_must_be_recorded -and @($machine.storage | Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_.model) -and $null -ne $_.size_bytes }).Count -eq 0) { Add-Issue "machine.storage" "at least one model and size without serial data" "missing" }
if ($profile.hardware.display_inventory_must_be_recorded -and $null -eq $machine.PSObject.Properties["displays"]) { Add-Issue "machine.display-inventory" "captured display inventory (which may be empty on a headless runner)" "missing" }
if ([string]$machine.tools.rustc -notmatch "^rustc $([regex]::Escape([string]$profile.software.rust_toolchain))([ .-]|$)") { Add-Issue "machine.rust-toolchain" $profile.software.rust_toolchain ([string]$machine.tools.rustc) }
if ($profile.software.ffmpeg_version_must_be_recorded -and [string]::IsNullOrWhiteSpace([string]$machine.tools.ffmpeg)) { Add-Issue "machine.ffmpeg" "recorded FFmpeg version" "missing" }
if ([string]::IsNullOrWhiteSpace([string]$machine.tools.ffprobe)) { Add-Issue "machine.ffprobe" "recorded FFprobe version" "missing" }
if ([string]$machine.git.revision -notmatch '^[0-9a-f]{40}$') { Add-Issue "machine.git-revision" "40-character Git commit" ([string]$machine.git.revision) }
if ($RequireBaselineEligibility -and $profile.evidence.clean_tree_required_for_baseline -and $machine.git.dirty) { Add-Issue "machine.dirty-tree" "clean working tree" "dirty" }

$status = if ($issues.Count -eq 0) { "passed" } else { "failed" }
$result = [ordered]@{
    schema_version = 1
    profile = $profile.id
    machine_id = $machine.machine_id
    machine_report = $reportAbsolute
    baseline_eligibility_required = [bool]$RequireBaselineEligibility
    status = $status
    issues = @($issues)
}
$parent = Split-Path -Parent $outputAbsolute
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputAbsolute -Encoding utf8
Write-Host "Windows reference validation: $status; report: $outputAbsolute"
foreach ($issue in $issues) { Write-Host "[error] $($issue.code): expected $($issue.expected); observed $($issue.observed)" }
if ($status -ne "passed") { exit 1 }
