param(
    [string]$OutputDir = "target/perf/current",
    [switch]$Include8K
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-PerfTest {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Command
    )

    Write-Host "==> $Name"
    Write-Host "    $Command"
    Invoke-Expression $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "    FAILED ($LASTEXITCODE)"
        return $false
    }
    return $true
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$projectOut = Join-Path $OutputDir "project-lifecycle.jsonl"
$previewOut = Join-Path $OutputDir "preview.jsonl"
$exportOut = Join-Path $OutputDir "export.jsonl"
$audioOut = Join-Path $OutputDir "audio.jsonl"

Remove-Item -Force -ErrorAction SilentlyContinue $projectOut, $previewOut, $exportOut, $audioOut

$env:MONDRIAN_PERF_OUTPUT = $projectOut
$env:MONDRIAN_PREVIEW_SIM_OUTPUT = $previewOut
$env:MONDRIAN_EXPORT_SIM_OUTPUT = $exportOut
$env:MONDRIAN_AUDIO_SIM_OUTPUT = $audioOut

$failed = New-Object System.Collections.Generic.List[string]

if (!(Invoke-PerfTest -Name "project lifecycle smoke perf" -Command "cargo test -p mondrian-app perf_project_lifecycle_smoke -- --ignored --nocapture")) {
    $failed.Add("project lifecycle smoke perf")
}

if (!(Invoke-PerfTest -Name "preview 1080p29.97 perf" -Command "cargo test -p mondrian-app preview_1080p2997_simulated_perf -- --ignored --nocapture")) {
    $failed.Add("preview 1080p29.97 perf")
}
if (!(Invoke-PerfTest -Name "preview 4k60 perf" -Command "cargo test -p mondrian-app preview_4k60_simulated_perf -- --ignored --nocapture")) {
    $failed.Add("preview 4k60 perf")
}
if ($Include8K) {
    if (!(Invoke-PerfTest -Name "preview 8k60 perf" -Command "cargo test -p mondrian-app preview_8k60_simulated_perf -- --ignored --nocapture")) {
        $failed.Add("preview 8k60 perf")
    }
}

if (!(Invoke-PerfTest -Name "export 1080p29.97 perf" -Command "cargo test -p mondrian-export export_1080p2997_simulated_perf -- --ignored --nocapture")) {
    $failed.Add("export 1080p29.97 perf")
}
if (!(Invoke-PerfTest -Name "export 4k60 perf" -Command "cargo test -p mondrian-export export_4k60_simulated_perf -- --ignored --nocapture")) {
    $failed.Add("export 4k60 perf")
}
if (!(Invoke-PerfTest -Name "export 4k60 passthrough perf" -Command "cargo test -p mondrian-export export_4k60_single_layer_passthrough_simulated_perf -- --ignored --nocapture")) {
    $failed.Add("export 4k60 passthrough perf")
}

if (!(Invoke-PerfTest -Name "audio mix perf" -Command "cargo test -p mondrian-media audio_mix_48k_stereo_simulated_perf -- --ignored --nocapture")) {
    $failed.Add("audio mix perf")
}

Write-Host ""
Write-Host "Perf suite finished."
Write-Host "Output directory: $OutputDir"
Write-Host "  project: $projectOut"
Write-Host "  preview: $previewOut"
Write-Host "  export : $exportOut"
Write-Host "  audio  : $audioOut"
Write-Host ""
Write-Host "Next:"
Write-Host "  powershell -File scripts/perf/compare-perf.ps1 -BeforeDir target/perf/baseline -AfterDir $OutputDir"

if ($failed.Count -gt 0) {
    Write-Host ""
    Write-Host "Perf suite completed with failures:"
    foreach ($name in $failed) {
        Write-Host "  - $name"
    }
    exit 1
}
