param(
    [string]$OutputDir = "target/perf/current",
    [switch]$Include4KExport,
    [ValidateRange(1, [int]::MaxValue)]
    [int]$ProcessTimeoutSeconds = 2700
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$env:CARGO_INCREMENTAL = "0"
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
Import-Module (Join-Path $repositoryRoot "scripts/validation/playback-gate-process.psm1") -Force
Import-Module (Join-Path $repositoryRoot "scripts/perf/perf-owner-closure.psm1") -Force
$OutputDir = if ([IO.Path]::IsPathRooted($OutputDir)) {
    [IO.Path]::GetFullPath($OutputDir)
}
else {
    [IO.Path]::GetFullPath((Join-Path $repositoryRoot $OutputDir))
}
$suiteMutex = [System.Threading.Mutex]::new($false, "Mondrian.PerfSuite.v1")
$suiteLockTaken = $false
try {
    $suiteLockTaken = $suiteMutex.WaitOne(0)
}
catch [System.Threading.AbandonedMutexException] {
    $suiteLockTaken = $true
}
if (!$suiteLockTaken) {
    $suiteMutex.Dispose()
    throw "Another Mondrian performance suite is already running"
}

try {
function Clear-ReportPath {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Force -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $Path) {
        throw "Unable to establish a fresh performance report path: $Path"
    }
}

function Read-StrictJsonLines {
    param([Parameter(Mandatory = $true)][string]$Path)

    $file = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($file.Length -eq 0) {
        throw "Performance report is empty: $Path"
    }
    $rows = @()
    foreach ($rawLine in Get-Content -LiteralPath $Path) {
        $line = $rawLine.Trim()
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $value = $line | ConvertFrom-Json -ErrorAction Stop
        if ($value -is [array]) {
            $rows += @($value)
        }
        else {
            $rows += $value
        }
    }
    if ($rows.Count -eq 0) {
        throw "Performance report contains no JSON records: $Path"
    }
    return $rows
}

function Assert-NonFailingColorReport {
    param(
        [Parameter(Mandatory = $true)]$Container,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$Property,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$Context
    )

    if (!($Container.PSObject.Properties.Name -contains $Property)) {
        throw "$Context is missing color-health report '$Property'"
    }
    $report = $Container.$Property
    if ($null -eq $report -or !($report.PSObject.Properties.Name -contains "verdict")) {
        throw "$Context color-health report '$Property' is missing its verdict"
    }
    $verdict = [string]$report.verdict
    if ($verdict -eq "Fail") {
        throw "$Context color-health report '$Property' failed"
    }
    if ($verdict -notin @("Pass", "Warn")) {
        throw "$Context color-health report '$Property' has an unknown verdict: $verdict"
    }
}

function Assert-EligibleReport {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][ValidateSet(
            "project",
            "app",
            "export",
            "audio"
        )][string]$Kind,
        [string]$ExpectedScenario
    )

    $rows = @(Read-StrictJsonLines -Path $Path)
    foreach ($row in $rows) {
        if ($row.PSObject.Properties.Name -contains "skipped") {
            throw "Skipped performance work is not eligible evidence: $Path"
        }
    }

    switch ($Kind) {
        "project" {
            $expected = @(
                "project.create_new_project",
                "project.open_existing",
                "project.save_existing"
            )
            if ($rows.Count -ne $expected.Count) {
                throw "Project report must contain exactly three measured cases"
            }
            $actual = @($rows | ForEach-Object { [string]$_.case } | Sort-Object)
            if (Compare-Object -ReferenceObject ($expected | Sort-Object) -DifferenceObject $actual) {
                throw "Project report case set is incomplete or duplicated"
            }
            Assert-MondrianPerfCases -Cases $rows -Scenario "project"
            foreach ($row in $rows) {
                Assert-MondrianCleanOwnerClosure `
                    -Container $row `
                    -Context "Project report '$($row.case)'" `
                    -ExpectedPreviewOwners 0 `
                    -GpuRequired $false
            }
            Assert-MondrianIdenticalOwnerClosures -Containers $rows -Context "Project report cases"
        }
        "app" {
            if ($rows.Count -ne 1 -or [string]$rows[0].scenario -ne $ExpectedScenario) {
                throw "App report has an unexpected scenario or record count"
            }
            if (!($rows[0].PSObject.Properties.Name -contains "cases")) {
                throw "App report does not contain measured cases"
            }
            Assert-MondrianPerfCases -Cases $rows[0].cases -Scenario $ExpectedScenario
            $expectedPreviewOwners = if ($ExpectedScenario -eq "app_ui_scale") { 2 } else { 1 }
            $gpuRequired = $ExpectedScenario -ne "app_ui_scale"
            Assert-MondrianCleanOwnerClosure `
                -Container $rows[0] `
                -Context "App report $ExpectedScenario" `
                -ExpectedPreviewOwners $expectedPreviewOwners `
                -GpuRequired $gpuRequired
            if ($ExpectedScenario -eq "app_ui_scale") {
                Assert-NonFailingColorReport `
                    -Container $rows[0] `
                    -Property "preview_color_report" `
                    -Context "App report $ExpectedScenario"
                Assert-NonFailingColorReport `
                    -Container $rows[0] `
                    -Property "preview_playback_color_report" `
                    -Context "App report $ExpectedScenario"
            }
        }
        "export" {
            if ($rows.Count -ne 1 -or [string]$rows[0].scenario -ne $ExpectedScenario) {
                throw "Export report has an unexpected scenario or record count"
            }
            if (!($rows[0].PSObject.Properties.Name -contains "passed") -or
                $rows[0].passed -isnot [bool] -or !$rows[0].passed) {
                throw "Export report is missing a passing verdict"
            }
            if (!($rows[0].PSObject.Properties.Name -contains "pixel_oracle_proven") -or
                $rows[0].pixel_oracle_proven -isnot [bool] -or
                !$rows[0].pixel_oracle_proven) {
                throw "Export report is missing canonical pixel evidence"
            }
            if (!($rows[0].PSObject.Properties.Name -contains "color_report") -or
                [string]$rows[0].color_report.verdict -ne "Pass") {
                throw "Export report is missing a passing color-health verdict"
            }
            if ($ExpectedScenario -eq "export-4k60-simulated") {
                $resolution = @($rows[0].resolution)
                $expectedFusionFrames = [uint64]$rows[0].simulated_frames + 1
                if ($resolution.Count -ne 2 -or
                    [uint64]$resolution[0] -ne 3840 -or
                    [uint64]$resolution[1] -ne 2160 -or
                    [double]$rows[0].target_fps -ne 60.0 -or
                    [uint64]$rows[0].simulated_frames -lt 16 -or
                    [uint64]$rows[0].layers -ne 2 -or
                    [string]$rows[0].opacity_pattern -ne "blend") {
                    throw "4K60 export report has an ineligible workload contract"
                }
                if (!($rows[0].PSObject.Properties.Name -contains "fused_first_two_execution_proven") -or
                    $rows[0].fused_first_two_execution_proven -isnot [bool] -or
                    !$rows[0].fused_first_two_execution_proven -or
                    [uint64]$rows[0].fused_first_two_frames -ne $expectedFusionFrames) {
                    throw "4K60 export report is missing complete fusion execution evidence"
                }
            }
        }
        "audio" {
            if ($rows.Count -ne 12) {
                throw "Audio load matrix must contain exactly twelve records"
            }
            $keys = [System.Collections.Generic.HashSet[string]]::new()
            $expectedKeys = @(
                "1:0:64", "1:0:256", "1:0:1024",
                "8:2:64", "8:2:256", "8:2:1024",
                "32:8:64", "32:8:256", "32:8:1024",
                "64:16:64", "64:16:256", "64:16:1024"
            )
            foreach ($row in $rows) {
                if ([string]$row.profile -ne "dense_schedule_v2") {
                    throw "Audio load matrix has an unexpected profile"
                }
                $key = "$($row.tracks):$($row.buses):$($row.block_frames)"
                if (!$keys.Add($key)) {
                    throw "Audio load matrix contains a duplicate workload: $key"
                }
                $expectedRoutes = if ([uint64]$row.buses -eq 0) {
                    [uint64]$row.tracks
                }
                else {
                    [uint64]$row.tracks + [uint64]$row.buses
                }
                if ([uint64]$row.routes -ne $expectedRoutes -or
                    [uint64]$row.sample_rate -ne 48000 -or
                    [uint64]$row.channels -ne 2 -or
                    [uint64]$row.iterations -ne 256) {
                    throw "Audio load matrix has an unexpected workload contract: $key"
                }
                if ([uint64]$row.vectorized_p99_us -gt [uint64]$row.deadline_us -or
                    [uint64]$row.vectorized_deadline_misses -ne 0) {
                    throw "Audio load matrix missed its realtime deadline: $key"
                }
            }
            $actualKeys = @($keys.GetEnumerator() | ForEach-Object { [string]$_ })
            if (Compare-Object -ReferenceObject $expectedKeys -DifferenceObject $actualKeys) {
                throw "Audio load matrix workload set is incomplete"
            }
        }
    }
}

function Invoke-PerfTest {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$CargoArguments,
        [string]$ReportVariable,
        [string]$ReportPath,
        [string]$ReportKind,
        [string]$ExpectedScenario
    )

    Write-Host "==> $Name"
    if (![string]::IsNullOrWhiteSpace($ReportPath)) {
        Clear-ReportPath -Path $ReportPath
        [Environment]::SetEnvironmentVariable($ReportVariable, $ReportPath, "Process")
    }

    $logName = ([regex]::Replace($Name.ToLowerInvariant(), "[^a-z0-9]+", "-")).Trim("-")
    $logPath = Join-Path $script:perfCargoLogDir "$logName.log"
    $processResult = $null
    $supervisorError = $null
    try {
        $processResult = Invoke-BoundedPlaybackGateProcess `
            -FilePath "cargo" `
            -Arguments $CargoArguments `
            -WorkingDirectory $repositoryRoot `
            -TimeoutSeconds $ProcessTimeoutSeconds `
            -LogPath $logPath
    }
    catch {
        $supervisorError = $_.Exception.Message
    }
    finally {
        if (![string]::IsNullOrWhiteSpace($ReportVariable)) {
            [Environment]::SetEnvironmentVariable($ReportVariable, $null, "Process")
        }
    }

    if ($null -ne $supervisorError) {
        Write-Host "    FAILED (process supervisor: $supervisorError)"
        Write-Host "    log: $logPath"
        return $false
    }
    if ($processResult.timed_out) {
        Write-Host "    FAILED (timed out after $ProcessTimeoutSeconds seconds; descendant process tree terminated)"
        Write-Host "    log: $logPath"
        return $false
    }
    if ($processResult.exit_code -ne 0) {
        Write-Host "    FAILED ($($processResult.exit_code))"
        Write-Host "    log: $logPath"
        return $false
    }

    if (![string]::IsNullOrWhiteSpace($ReportPath)) {
        try {
            Assert-EligibleReport `
                -Path $ReportPath `
                -Kind $ReportKind `
                -ExpectedScenario $ExpectedScenario
        }
        catch {
            Write-Host "    FAILED (ineligible report: $($_.Exception.Message))"
            return $false
        }
    }

    return $true
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$script:perfCargoLogDir = Join-Path $OutputDir "cargo-logs"
New-Item -ItemType Directory -Force -Path $script:perfCargoLogDir | Out-Null

$projectOut = Join-Path $OutputDir "project-lifecycle.jsonl"
$uiOut = Join-Path $OutputDir "app-ui-scale.jsonl"
$previewMediaOut = Join-Path $OutputDir "preview-media.jsonl"
$previewPlaybackOut = Join-Path $OutputDir "preview-playback.jsonl"
$export1080Out = Join-Path $OutputDir "export-1080p2997.jsonl"
$export4KOut = Join-Path $OutputDir "export-4k60.jsonl"
$export4KPassthroughOut = Join-Path $OutputDir "export-4k60-passthrough.jsonl"
$audioOut = Join-Path $OutputDir "audio-load-matrix.jsonl"

foreach ($path in @(
        $projectOut,
        $uiOut,
        $previewMediaOut,
        $previewPlaybackOut,
        $export1080Out,
        $export4KOut,
        $export4KPassthroughOut,
        $audioOut
    )) {
    Clear-ReportPath -Path $path
}

$failed = [System.Collections.Generic.List[string]]::new()

# Preview requires the same-profile product executable, not a lib-test binary or
# a stale worker override. Build and tests share Cargo's target configuration.
if (![string]::IsNullOrWhiteSpace($env:MONDRIAN_PREVIEW_DEMUX_WORKER_PATH)) {
    throw "Unset MONDRIAN_PREVIEW_DEMUX_WORKER_PATH before the qualification suite"
}
if (!(Invoke-PerfTest -Name "release packaged Preview worker build" `
    -CargoArguments @("build", "-p", "mondrian-app", "--release", "-j", "1", "--bin", "mondrian"))) {
    throw "Release product build failed; Preview qualification cannot start"
}
$appCargoPrefix = @("test", "-p", "mondrian-app", "--release", "-j", "1", "--lib")
if (!(Invoke-PerfTest -Name "release App test runner build" `
    -CargoArguments ($appCargoPrefix + @("--no-run")))) {
    throw "Release App test build failed; no App measurements were started"
}
$testSuffix = @("--", "--ignored", "--nocapture", "--test-threads=1")

if (!(Invoke-PerfTest `
            -Name "project lifecycle smoke" `
            -CargoArguments ($appCargoPrefix + @("perf_project_lifecycle_smoke") + $testSuffix) `
            -ReportVariable "MONDRIAN_PERF_OUTPUT" `
            -ReportPath $projectOut `
            -ReportKind "project")) {
    $failed.Add("project lifecycle smoke")
}

if (!(Invoke-PerfTest `
            -Name "app UI scale smoke" `
            -CargoArguments ($appCargoPrefix + @("app_ui_scale_smoke") + $testSuffix) `
            -ReportVariable "MONDRIAN_PERF_OUTPUT" `
            -ReportPath $uiOut `
            -ReportKind "app" `
            -ExpectedScenario "app_ui_scale")) {
    $failed.Add("app UI scale smoke")
}

if (!(Invoke-PerfTest `
            -Name "preview media decode/cache smoke" `
            -CargoArguments ($appCargoPrefix + @("preview_media_decode_cache_smoke") + $testSuffix) `
            -ReportVariable "MONDRIAN_PERF_OUTPUT" `
            -ReportPath $previewMediaOut `
            -ReportKind "app" `
            -ExpectedScenario "preview_media_decode_cache")) {
    $failed.Add("preview media decode/cache smoke")
}

if (!(Invoke-PerfTest `
            -Name "preview continuous-playback smoke" `
            -CargoArguments ($appCargoPrefix + @("preview_media_continuous_playback_smoke") + $testSuffix) `
            -ReportVariable "MONDRIAN_PERF_OUTPUT" `
            -ReportPath $previewPlaybackOut `
            -ReportKind "app" `
            -ExpectedScenario "preview_media_continuous_playback")) {
    $failed.Add("preview continuous-playback smoke")
}

$exportCargoPrefix = @("test", "-p", "mondrian-export", "--release", "-j", "2", "--lib")
if (!(Invoke-PerfTest `
            -Name "export 1080p29.97 smoke" `
            -CargoArguments ($exportCargoPrefix + @("export_1080p2997_simulated_perf") + $testSuffix) `
            -ReportVariable "MONDRIAN_EXPORT_SIM_OUTPUT" `
            -ReportPath $export1080Out `
            -ReportKind "export" `
            -ExpectedScenario "export-1080p2997-simulated")) {
    $failed.Add("export 1080p29.97 smoke")
}

if ($Include4KExport) {
    if (!(Invoke-PerfTest `
                -Name "export 4K60 smoke" `
                -CargoArguments ($exportCargoPrefix + @("export_4k60_simulated_perf") + $testSuffix) `
                -ReportVariable "MONDRIAN_EXPORT_SIM_OUTPUT" `
                -ReportPath $export4KOut `
                -ReportKind "export" `
                -ExpectedScenario "export-4k60-simulated")) {
        $failed.Add("export 4K60 smoke")
    }
    if (!(Invoke-PerfTest `
                -Name "export 4K60 single-layer passthrough smoke" `
                -CargoArguments ($exportCargoPrefix + @("export_4k60_single_layer_passthrough_simulated_perf") + $testSuffix) `
                -ReportVariable "MONDRIAN_EXPORT_SIM_OUTPUT" `
                -ReportPath $export4KPassthroughOut `
                -ReportKind "export" `
                -ExpectedScenario "export-4k60-single-layer-passthrough-simulated")) {
        $failed.Add("export 4K60 single-layer passthrough smoke")
    }
}

if (!(Invoke-PerfTest `
            -Name "dense audio schedule scalar/SIMD load matrix" `
            -CargoArguments @(
                "test", "-p", "mondrian-audio", "--release", "-j", "2",
                "--test", "load_matrix", "dense_schedule_multitrack_load_matrix",
                "--", "--ignored", "--nocapture", "--test-threads=1"
            ) `
            -ReportVariable "MONDRIAN_AUDIO_LOAD_MATRIX_OUTPUT" `
            -ReportPath $audioOut `
            -ReportKind "audio")) {
    $failed.Add("dense audio schedule scalar/SIMD load matrix")
}

Write-Host ""
Write-Host "Performance suite finished."
Write-Host "Output directory: $OutputDir"
Write-Host "  project         : $projectOut"
Write-Host "  app UI          : $uiOut"
Write-Host "  preview media   : $previewMediaOut"
Write-Host "  preview playback: $previewPlaybackOut"
Write-Host "  export 1080p    : $export1080Out"
Write-Host "  audio matrix    : $audioOut"
if ($Include4KExport) {
    Write-Host "  export 4K       : $export4KOut"
    Write-Host "  export 4K direct: $export4KPassthroughOut"
}
Write-Host ""
Write-Host "Project and export files with compatible schemas can be compared with:"
Write-Host "  powershell -File scripts/perf/compare-perf.ps1 -BeforeDir target/perf/baseline -AfterDir $OutputDir"

if ($failed.Count -gt 0) {
    Write-Host ""
    Write-Host "Performance suite completed with failures:"
    foreach ($name in $failed) {
        Write-Host "  - $name"
    }
    exit 1
}

}
finally {
    try { $suiteMutex.ReleaseMutex() }
    finally { $suiteMutex.Dispose() }
}
