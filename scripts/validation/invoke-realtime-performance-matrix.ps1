param(
    [string]$MachineId,
    [switch]$RegenerateGeneratedFixtures,
    [switch]$ValidateOnly,
    [string]$RunRoot = "target/validation/realtime-performance"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot "playback-gate-process.psm1") -Force

$script:repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$script:matrixPath = Join-Path $script:repositoryRoot "tests/validation/realtime-performance-matrix.json"
$script:manifestPath = Join-Path $script:repositoryRoot "tests/validation/corpus-manifest.json"
$script:machineProfilePath = Join-Path $script:repositoryRoot "tests/validation/windows-alpha-reference.json"

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Assert-ExactSet([string]$Name, [string[]]$Expected, [string[]]$Actual) {
    if (@($Actual | Select-Object -Unique).Count -ne $Actual.Count) {
        throw "$Name contains duplicate values."
    }
    if (@(Compare-Object ($Expected | Sort-Object) ($Actual | Sort-Object)).Count -ne 0) {
        throw "$Name differs from the sealed contract."
    }
}

function Read-JsonLines([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return @() }
    return @(
        Get-Content -LiteralPath $Path |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            ForEach-Object { $_ | ConvertFrom-Json }
    )
}

function Get-FileEvidence([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    $item = Get-Item -LiteralPath $Path
    return [ordered]@{
        path = $item.FullName
        size_bytes = [int64]$item.Length
        sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Invoke-ScriptChecked([string]$Path, [hashtable]$Parameters) {
    $global:LASTEXITCODE = 0
    & $Path @Parameters
    if ($LASTEXITCODE -ne 0) { throw "Validation script failed with exit code ${LASTEXITCODE}: $Path" }
}

function Set-GateEnvironment([object]$Gate, [string]$ReportPath, [string]$MediaPath) {
    $saved = @{}
    $bindings = [ordered]@{}
    if ($null -ne $Gate.PSObject.Properties["report_environment"]) {
        $bindings[[string]$Gate.report_environment] = $ReportPath
    }
    if ($null -ne $Gate.PSObject.Properties["media_environment"]) {
        $bindings[[string]$Gate.media_environment] = $MediaPath
    }
    if ($null -ne $Gate.PSObject.Properties["environment"]) {
        foreach ($property in $Gate.environment.PSObject.Properties) {
            $bindings[$property.Name] = [string]$property.Value
        }
    }
    $bindings["MONDRIAN_REALTIME_PERFORMANCE_EXECUTION_POLICY"] = "sealed-required"
    foreach ($name in $bindings.Keys) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, [string]$bindings[$name], "Process")
    }
    return $saved
}

function Restore-GateEnvironment([hashtable]$Saved) {
    foreach ($name in $Saved.Keys) {
        [Environment]::SetEnvironmentVariable($name, $Saved[$name], "Process")
    }
}

function Invoke-CargoGate(
    [object]$Gate,
    [string[]]$BuildArguments,
    [string[]]$RunArguments,
    [string]$ReportPath,
    [string]$LogPrefix,
    [string]$MediaPath
) {
    Remove-Item -LiteralPath $ReportPath, "$LogPrefix-build.log", "$LogPrefix-run.log" -Force -ErrorAction SilentlyContinue
    $saved = Set-GateEnvironment $Gate $ReportPath $MediaPath
    try {
        $build = Invoke-BoundedPlaybackGateProcess "cargo" $BuildArguments $script:repositoryRoot ([int]$Gate.build_timeout_seconds) "$LogPrefix-build.log"
        if ($build.timed_out -or $build.exit_code -ne 0) {
            return [pscustomobject]@{ passed = $false; phase = "build"; build = $build; run = $null }
        }
        $run = Invoke-BoundedPlaybackGateProcess "cargo" $RunArguments $script:repositoryRoot ([int]$Gate.process_timeout_seconds) "$LogPrefix-run.log"
        return [pscustomobject]@{
            passed = -not $run.timed_out -and $run.exit_code -eq 0
            phase = "run"
            build = $build
            run = $run
        }
    } finally {
        Restore-GateEnvironment $saved
    }
}

function Test-RealtimeFixture([object]$Fixture) {
    $artifactPath = Resolve-RepositoryPath (Join-Path "tests/fixtures" ([string]$Fixture.path))
    $attestationPath = "$artifactPath$($Fixture.generation.attestation_suffix)"
    if (-not (Test-Path -LiteralPath $artifactPath -PathType Leaf) -or -not (Test-Path -LiteralPath $attestationPath -PathType Leaf)) {
        return [pscustomobject]@{ qualified = $false; reason = "fixture-or-attestation-missing"; artifact_path = $artifactPath }
    }
    $attestation = Get-Content -LiteralPath $attestationPath -Raw | ConvertFrom-Json
    $artifact = Get-Item -LiteralPath $artifactPath
    $hash = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $qualified = (
        $attestation.schema_version -eq 1 -and
        $attestation.fixture_id -eq $Fixture.id -and
        $attestation.recipe.path -eq $Fixture.generation.recipe_path -and
        $attestation.recipe.sha256 -eq $Fixture.generation.recipe_sha256 -and
        $attestation.artifact.sha256 -eq $hash -and
        [int64]$attestation.artifact.size_bytes -eq [int64]$artifact.Length
    )
    return [pscustomobject]@{
        qualified = $qualified
        reason = if ($qualified) { $null } else { "fixture-attestation-mismatch" }
        artifact_path = $artifactPath
        artifact = Get-FileEvidence $artifactPath
        attestation = Get-FileEvidence $attestationPath
    }
}

function Test-PlaybackReport([object]$Gate, [object]$Report) {
    if ($null -eq $Report) { return $false }
    $frameRate = $Report.media_probe.frame_rate
    return (
        [string]$Report.scenario -eq [string]$Gate.expected_scenario -and
        [int]$Report.frames -ge [int]$Gate.expected_frames -and
        [int]$Report.media_probe.width -eq [int]$Gate.expected_width -and
        [int]$Report.media_probe.height -eq [int]$Gate.expected_height -and
        "$($frameRate.num)/$($frameRate.den)" -eq [string]$Gate.expected_frame_rate -and
        [int]$Report.media_probe.bit_depth -eq [int]$Gate.expected_bit_depth -and
        $Report.media_probe.frame_rate_proven -eq $true -and
        $Report.media_probe.pixel_format_proven -eq $true -and
        $Report.real_media_gates.passed -eq $true -and
        $Report.multilayer_playback.passed -eq $true -and
        [int]$Report.multilayer_playback.expected_layers_per_frame -eq [int]$Gate.expected_layers -and
        $Report.multilayer_playback.authored_full_gpu_extent_exact -eq $true -and
        [int]$Report.headless_gpu.stage_diagnostics.readback_stages -eq 0 -and
        [int]$Report.headless_gpu.stage_diagnostics.gpu_blockers -eq 0 -and
        [int]$Report.headless_gpu.fallback_count -eq 0 -and
        [string]$Report.preview_color_report.verdict -ne "Fail" -and
        @($Report.decode_failure_codes).Count -eq 0 -and
        @($Report.render_failure_codes).Count -eq 0
    )
}

function Test-VisualReports([object]$Gate, [object[]]$Reports) {
    if ($Reports.Count -ne @($Gate.expected_scenarios).Count) { return $false }
    $scenarios = @($Reports | ForEach-Object { [string]$_.scenario })
    if (@(Compare-Object (@($Gate.expected_scenarios) | Sort-Object) ($scenarios | Sort-Object)).Count -ne 0) { return $false }
    return @($Reports | Where-Object {
        [int]$_.schema_version -ne [int]$Gate.expected_report_schema -or
        [string]$_.profile -ne [string]$Gate.expected_profile -or
        $_.passed -ne $true -or
        [string]$_.verdict -ne "pass" -or
        @($_.root_causes).Count -ne 0
    }).Count -eq 0
}

function Test-AuthoringReports([object]$Gate, [object[]]$Reports) {
    $scale = @($Reports | Where-Object record_type -eq "authoring_scale_report")
    $completion = @($Reports | Where-Object record_type -eq "authoring_run_completion")
    if ($scale.Count -ne [int]$Gate.expected_scale_reports -or $completion.Count -ne [int]$Gate.expected_completion_reports) { return $false }
    $runIds = @($Reports | ForEach-Object { [string]$_.run_id } | Select-Object -Unique)
    return (
        $runIds.Count -eq 1 -and
        @($Reports | Where-Object { [int]$_.protocol_schema_version -ne [int]$Gate.expected_protocol_schema }).Count -eq 0 -and
        @($scale | Where-Object { [int]$_.report.schema_version -ne [int]$Gate.expected_report_schema -or $_.report.all_within_reference_budget -ne $true }).Count -eq 0 -and
        @($scale | Where-Object { [int]$_.report.scale.duration_minutes -eq [int]$Gate.required_duration_minutes }).Count -eq 2 -and
        $completion[0].source_unchanged_during_run -eq $true -and
        [int]$completion[0].completed_report_count -eq [int]$Gate.expected_scale_reports
    )
}

$matrix = Get-Content -LiteralPath $matrixPath -Raw | ConvertFrom-Json
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$machineProfile = Get-Content -LiteralPath $machineProfilePath -Raw | ConvertFrom-Json
if ($matrix.schema_version -ne 1 -or $manifest.schema_version -ne 2 -or $machineProfile.schema_version -ne 3) { throw "Unsupported realtime matrix dependency schema." }
if ($matrix.machine_profile -ne $machineProfile.id -or $matrix.execution_policy -ne "sealed-required") { throw "Realtime matrix machine or execution policy is not sealed." }
$requiredGates = @("playback-reference", "real-4k60-dual-layer", "renderer-visual", "long-authoring")
$requiredDimensions = @(
    "real-4k60-main10-dual-layer-decode-publish",
    "4k60-hdr-multilayer-multieffect-scopes",
    "8k30-hdr-multilayer-multieffect-scopes",
    "30-minute-video-playback",
    "30-minute-audio-recovery",
    "120-minute-program-authoring-scale"
)
Assert-ExactSet "realtime gate ids" $requiredGates @($matrix.gates | ForEach-Object { [string]$_.id })
Assert-ExactSet "realtime acceptance gate ids" $requiredGates @($matrix.acceptance.required_gate_ids | ForEach-Object { [string]$_ })
Assert-ExactSet "realtime dimensions" $requiredDimensions @($matrix.required_dimensions | ForEach-Object { [string]$_ })
Assert-ExactSet "gate dimension coverage" $requiredDimensions @($matrix.gates.required_dimensions | ForEach-Object { [string]$_ })
if ($matrix.acceptance.skip_forbidden -ne $true -or $matrix.acceptance.serial_execution_required -ne $true) { throw "Realtime matrix must forbid skips and require serial execution." }
foreach ($gate in $matrix.gates) {
    if ([int]$gate.process_timeout_seconds -le 0) { throw "Gate '$($gate.id)' has no positive process timeout." }
    if ($gate.kind -eq "cargo-test" -and [int]$gate.build_timeout_seconds -le 0) { throw "Gate '$($gate.id)' has no positive build timeout." }
}
$fixture = @($manifest.entries | Where-Object id -eq "generated-performance-4k60-hevc-main10-rec709-v1")
if ($fixture.Count -ne 1) { throw "Realtime 4K60 fixture contract is absent or ambiguous." }
Assert-ExactSet "realtime fixture purposes" @("realtime-performance-4k60", "4k60-playback", "10-bit-decode", "multilayer-playback") @($fixture[0].purposes | ForEach-Object { [string]$_ })
$recipePath = Resolve-RepositoryPath ([string]$fixture[0].generation.recipe_path)
if ((Get-FileHash -LiteralPath $recipePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $fixture[0].generation.recipe_sha256) { throw "Realtime fixture recipe hash differs from the corpus manifest." }
foreach ($path in @($matrix.gates | Where-Object kind -eq "nested-supervisor" | ForEach-Object { Resolve-RepositoryPath ([string]$_.script) })) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Nested supervisor is missing: $path" }
}

if ($ValidateOnly) {
    [pscustomobject]@{
        schema_version = 1
        matrix_id = $matrix.id
        status = "contract-valid"
        gate_ids = $requiredGates
        dimensions = $requiredDimensions
    } | ConvertTo-Json -Depth 6
    return
}
if ([string]::IsNullOrWhiteSpace($MachineId)) { throw "MachineId is required unless -ValidateOnly is used." }

$mutex = [Threading.Mutex]::new($false, "Local\MondrianRealtimePerformanceMatrixV1")
$ownsMutex = $false
try {
    $ownsMutex = $mutex.WaitOne(0)
    if (-not $ownsMutex) { throw "Another realtime performance matrix is already running." }
    $timestamp = [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ")
    $runId = "$timestamp-$MachineId-$(([guid]::NewGuid().ToString('N')).Substring(0, 8))"
    $runDirectory = Resolve-RepositoryPath (Join-Path $RunRoot $runId)
    New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
    $evidencePath = Join-Path $runDirectory "evidence.json"
    $startedRevision = (git -C $script:repositoryRoot rev-parse HEAD).Trim()
    $startedDirty = @(git -C $script:repositoryRoot status --porcelain).Count -gt 0
    $status = "failed"
    $failurePhase = $null
    $failureMessage = $null
    $unqualifiedReasons = [System.Collections.Generic.List[string]]::new()
    $gateResults = [System.Collections.Generic.List[object]]::new()
    $machineReportPath = Join-Path $runDirectory "machine.json"
    $machineValidationPath = Join-Path $runDirectory "machine-validation.json"
    $fixtureEvidence = $null
    $playbackFixtureEvidence = @()
    try {
        $failurePhase = "machine-capture"
        Invoke-ScriptChecked (Join-Path $PSScriptRoot "capture-windows-reference.ps1") @{ MachineId = $MachineId; OutputPath = $machineReportPath }
        $failurePhase = "machine-validation"
        $global:LASTEXITCODE = 0
        & (Join-Path $PSScriptRoot "validate-windows-reference.ps1") -ProfilePath $machineProfilePath -MachineReportPath $machineReportPath -OutputPath $machineValidationPath -RequiredMemoryClass ([string]$matrix.minimum_machine.memory_class) -RequireBaselineEligibility
        $machineValidationExit = $LASTEXITCODE
        $machineReport = Get-Content -LiteralPath $machineReportPath -Raw | ConvertFrom-Json
        if ($machineValidationExit -ne 0) { [void]$unqualifiedReasons.Add("machine-profile-not-qualified") }
        $reportedGpuBytes = @($machineReport.gpus | ForEach-Object { if ($null -eq $_.adapter_ram_bytes) { 0 } else { [uint64]$_.adapter_ram_bytes } } | Measure-Object -Maximum).Maximum
        if ([uint64]$reportedGpuBytes -lt [uint64]$matrix.minimum_machine.minimum_reported_dedicated_gpu_bytes) { [void]$unqualifiedReasons.Add("reported-dedicated-gpu-memory-below-contract") }
        if ($startedDirty) { [void]$unqualifiedReasons.Add("source-tree-dirty-at-start") }

        if ($RegenerateGeneratedFixtures) {
            $failurePhase = "fixture-generation"
            Invoke-ScriptChecked (Join-Path $PSScriptRoot "generate-realtime-performance-media.ps1") @{ Force = $true }
            Invoke-ScriptChecked (Join-Path $PSScriptRoot "generate-reference-playback-media.ps1") @{ Profile = "All"; Force = $true }
        }
        $fixtureEvidence = Test-RealtimeFixture $fixture[0]
        if (-not $fixtureEvidence.qualified) { [void]$unqualifiedReasons.Add([string]$fixtureEvidence.reason) }
        $playbackFixtureIds = @(
            "generated-playback-4k25-hevc-main10-rec709-v1",
            "generated-playback-aac-48k-stereo-v1"
        )
        foreach ($fixtureId in $playbackFixtureIds) {
            $playbackFixture = @($manifest.entries | Where-Object id -eq $fixtureId)
            if ($playbackFixture.Count -ne 1) {
                [void]$unqualifiedReasons.Add("playback-fixture-contract-missing")
                continue
            }
            $playbackEvidence = Test-RealtimeFixture $playbackFixture[0]
            $playbackFixtureEvidence += $playbackEvidence
            if (-not $playbackEvidence.qualified) {
                [void]$unqualifiedReasons.Add("playback-$($playbackEvidence.reason)")
            }
        }
        if ($unqualifiedReasons.Count -gt 0) { throw "UNQUALIFIED: $($unqualifiedReasons -join ', ')" }

        $failurePhase = "playback-reference"
        $nestedRoot = Join-Path $runDirectory "playback-reference"
        $nestedLog = Join-Path $runDirectory "playback-reference.log"
        $nestedGate = $matrix.gates | Where-Object id -eq "playback-reference"
        $pwsh = (Get-Process -Id $PID).Path
        $nestedArgs = @(
            "-NoProfile", "-File", (Resolve-RepositoryPath ([string]$nestedGate.script)),
            "-MachineId", $MachineId, "-Gate", "All", "-RunRoot", $nestedRoot
        )
        if ($RegenerateGeneratedFixtures) { $nestedArgs += "-RegenerateGeneratedFixtures" }
        $nestedProcess = Invoke-BoundedPlaybackGateProcess $pwsh $nestedArgs $script:repositoryRoot ([int]$nestedGate.process_timeout_seconds) $nestedLog
        $nestedEvidenceFiles = @(Get-ChildItem -LiteralPath $nestedRoot -Filter evidence.json -Recurse -File)
        $nestedEvidence = if ($nestedEvidenceFiles.Count -eq 1) { Get-Content -LiteralPath $nestedEvidenceFiles[0].FullName -Raw | ConvertFrom-Json } else { $null }
        $nestedPassed = -not $nestedProcess.timed_out -and $nestedProcess.exit_code -eq 0 -and $null -ne $nestedEvidence -and $nestedEvidence.status -eq $nestedGate.expected_status
        [void]$gateResults.Add([pscustomobject]@{ id = "playback-reference"; passed = $nestedPassed; process = $nestedProcess; report = if ($null -eq $nestedEvidenceFiles -or $nestedEvidenceFiles.Count -ne 1) { $null } else { Get-FileEvidence $nestedEvidenceFiles[0].FullName }; log = Get-FileEvidence $nestedLog })
        if (-not $nestedPassed) { throw "Nested playback reference gate failed." }

        $failurePhase = "real-4k60-dual-layer"
        $gate = $matrix.gates | Where-Object id -eq "real-4k60-dual-layer"
        $reportPath = Join-Path $runDirectory "real-4k60-dual-layer.jsonl"
        $prefix = Join-Path $runDirectory "real-4k60-dual-layer"
        $cargo = Invoke-CargoGate $gate @("test", "-p", "mondrian-app", "--release", "--features", "validation", "--lib", "--no-run", [string]$gate.cargo_test) @("test", "-p", "mondrian-app", "--release", "--features", "validation", "--lib", [string]$gate.cargo_test, "--", "--ignored", "--nocapture", "--test-threads=1") $reportPath $prefix ([string]$fixtureEvidence.artifact_path)
        $reports = Read-JsonLines $reportPath
        $reportPassed = $reports.Count -eq 1 -and (Test-PlaybackReport $gate $reports[0])
        $passed = $cargo.passed -and $reportPassed
        [void]$gateResults.Add([pscustomobject]@{ id = "real-4k60-dual-layer"; passed = $passed; process = $cargo; report = Get-FileEvidence $reportPath; build_log = Get-FileEvidence "$prefix-build.log"; run_log = Get-FileEvidence "$prefix-run.log" })
        if (-not $passed) { throw "Real 4K60 dual-layer gate failed." }

        $failurePhase = "renderer-visual"
        $gate = $matrix.gates | Where-Object id -eq "renderer-visual"
        $reportPath = Join-Path $runDirectory "renderer-visual.jsonl"
        $prefix = Join-Path $runDirectory "renderer-visual"
        $cargo = Invoke-CargoGate $gate @("test", "-p", "mondrian-renderer", "--release", "--test", "realtime_visual_gpu_perf", "--no-run") @("test", "-p", "mondrian-renderer", "--release", "--test", "realtime_visual_gpu_perf", [string]$gate.test_filter, "--", "--ignored", "--nocapture", "--test-threads=1") $reportPath $prefix $null
        $reports = Read-JsonLines $reportPath
        $reportPassed = Test-VisualReports $gate $reports
        $passed = $cargo.passed -and $reportPassed
        [void]$gateResults.Add([pscustomobject]@{ id = "renderer-visual"; passed = $passed; process = $cargo; report = Get-FileEvidence $reportPath; build_log = Get-FileEvidence "$prefix-build.log"; run_log = Get-FileEvidence "$prefix-run.log" })
        if (-not $passed) { throw "Renderer visual matrix gate failed." }

        $failurePhase = "long-authoring"
        $gate = $matrix.gates | Where-Object id -eq "long-authoring"
        $reportPath = Join-Path $runDirectory "long-authoring.jsonl"
        $prefix = Join-Path $runDirectory "long-authoring"
        $cargo = Invoke-CargoGate $gate @("test", "-p", "mondrian-app", "--release", "--features", "validation", "--lib", "--no-run", [string]$gate.cargo_test) @("test", "-p", "mondrian-app", "--release", "--features", "validation", "--lib", [string]$gate.cargo_test, "--", "--ignored", "--nocapture", "--test-threads=1") $reportPath $prefix $null
        $reports = Read-JsonLines $reportPath
        $reportPassed = Test-AuthoringReports $gate $reports
        $passed = $cargo.passed -and $reportPassed
        [void]$gateResults.Add([pscustomobject]@{ id = "long-authoring"; passed = $passed; process = $cargo; report = Get-FileEvidence $reportPath; build_log = Get-FileEvidence "$prefix-build.log"; run_log = Get-FileEvidence "$prefix-run.log" })
        if (-not $passed) { throw "Long-authoring matrix gate failed." }
        $failurePhase = $null
    } catch {
        $failureMessage = $_.Exception.Message
        if ($unqualifiedReasons.Count -eq 0 -and $failurePhase -in @("machine-capture", "machine-validation", "fixture-generation")) {
            [void]$unqualifiedReasons.Add("qualification-prerequisite-unavailable")
        }
    }

    $endingRevision = (git -C $script:repositoryRoot rev-parse HEAD).Trim()
    $endingDirty = @(git -C $script:repositoryRoot status --porcelain).Count -gt 0
    if ($endingRevision -ne $startedRevision -or $endingDirty) { [void]$unqualifiedReasons.Add("source-tree-changed-or-dirty-at-end") }
    $complete = $gateResults.Count -eq $requiredGates.Count -and @($gateResults | Where-Object { -not $_.passed }).Count -eq 0
    if ($unqualifiedReasons.Count -gt 0) {
        $status = "unqualified"
    } elseif ($null -ne $failureMessage -or -not $complete) {
        $status = "failed"
    } else {
        $status = "passed-baseline"
    }
    $evidence = [ordered]@{
        schema_version = 1
        run_id = $runId
        status = $status
        matrix = [ordered]@{ id = $matrix.id; path = $matrixPath; sha256 = (Get-FileHash -LiteralPath $matrixPath -Algorithm SHA256).Hash.ToLowerInvariant() }
        corpus = [ordered]@{ revision = $manifest.corpus_revision; path = $manifestPath; sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant() }
        machine = Get-FileEvidence $machineReportPath
        machine_validation = Get-FileEvidence $machineValidationPath
        fixtures = @($fixtureEvidence) + @($playbackFixtureEvidence)
        source = [ordered]@{ started_revision = $startedRevision; ending_revision = $endingRevision; started_dirty = $startedDirty; ending_dirty = $endingDirty; unchanged = $startedRevision -eq $endingRevision -and -not $startedDirty -and -not $endingDirty }
        required_dimensions = $requiredDimensions
        gates = @($gateResults)
        unqualified_reasons = @($unqualifiedReasons)
        failure_phase = $failurePhase
        failure_message = $failureMessage
    }
    $evidence | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $evidencePath -Encoding utf8
    Write-Host "Realtime performance evidence: $evidencePath"
    Write-Host "Status: $status"
    if ($status -eq "passed-baseline") { exit 0 }
    if ($status -eq "unqualified") { exit 2 }
    exit 1
} finally {
    if ($ownsMutex) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
