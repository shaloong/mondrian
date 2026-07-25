param(
    [ValidateRange(60, 3600)][int]$ProcessTimeoutSeconds = 600,
    [ValidateRange(0, 60)][int]$NaturalExitGraceSeconds = 5,
    [string]$RunRoot = "target/validation/runs",
    [string]$FixtureRoot = "tests/fixtures"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot "playback-gate-process.psm1") -Force

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Assert-ExactStringSet(
    [object[]]$Expected,
    [object[]]$Actual,
    [string]$Label
) {
    $expectedStrings = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualStrings = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $difference = @(Compare-Object $expectedStrings $actualStrings)
    if ($difference.Count -ne 0) {
        throw "$Label does not exactly match the Golden contract."
    }
}

function Assert-CompleteGoldenReport(
    [pscustomobject]$Report,
    [pscustomobject]$Contract,
    [string[]]$RequiredSliceIds
) {
    if (
        $Report.schema_version -ne 1 -or
        $Report.profile -ne "windows-alpha-complete-golden-project" -or
        $Report.contract_id -ne $Contract.id -or
        $Report.status -ne "pass" -or
        $Report.complete_golden_project -ne $true
    ) {
        $detail = if ($Report.status -eq "fail") { $Report.error } else { "invalid top-level identity or status" }
        throw "Complete Golden process did not produce a passing top-level report: $detail"
    }
    if (
        $Report.required_consecutive_passes -ne $Contract.acceptance.consecutive_passes -or
        $Report.execution_plan.contract_id -ne $Contract.id -or
        $Report.execution_plan.status -ne "complete" -or
        $Report.execution_plan.complete_golden_project -ne $false -or
        $Report.execution_plan.hero_sequence_role -ne $Contract.hero_sequence.role
    ) {
        throw "Complete Golden report changed the structural execution-plan contract."
    }
    foreach ($field in @("fixture_roles", "operations", "content", "exports")) {
        if (@($Report.execution_plan.hero_missing.$field).Count -ne 0) {
            throw "Complete Golden report retained Hero Sequence obligations in '$field'."
        }
    }

    $stages = @($Report.stages)
    Assert-ExactStringSet $RequiredSliceIds @($stages | ForEach-Object { $_.id }) "Executed slice set"
    foreach ($stage in $stages) {
        if (
            $stage.report.contract_id -ne $Contract.id -or
            $stage.report.status -notin @("pass", "passed") -or
            $stage.report.complete_golden_project -ne $false
        ) {
            throw "Slice '$($stage.id)' did not retain partial-only passing semantics."
        }
    }

    $sequenceProperties = @($Report.final_project.sequence_ids.psobject.Properties)
    $primarySequenceProperties = @($Report.final_project.primary_sequence_ids.psobject.Properties)
    Assert-ExactStringSet $RequiredSliceIds @($sequenceProperties | ForEach-Object { $_.Name }) "Sequence ownership roles"
    Assert-ExactStringSet $RequiredSliceIds @($primarySequenceProperties | ForEach-Object { $_.Name }) "Primary Sequence roles"
    $sequenceIds = @(
        $sequenceProperties | ForEach-Object {
            @($_.Value) | ForEach-Object { [string]$_ }
        }
    )
    $uniqueSequenceIds = @($sequenceIds | Sort-Object -Unique)
    if (
        $uniqueSequenceIds.Count -ne [int]$Report.final_project.sequence_count
    ) {
        throw "Final Project Sequence ownership does not cover its exact author set."
    }
    if (
        $Report.final_project.hero_sequence_role -ne $Contract.hero_sequence.role -or
        [string]::IsNullOrWhiteSpace([string]$Report.final_project.hero_sequence_id) -or
        [string]$Report.final_project.hero_sequence_id -notin $uniqueSequenceIds
    ) {
        throw "Final Project did not retain the declared Hero Sequence identity."
    }
    $heroSliceIds = @(
        $Contract.execution_slices |
            Where-Object { $_.sequence_role -eq $Contract.hero_sequence.role } |
            ForEach-Object { [string]$_.id }
    )
    foreach ($sliceId in $heroSliceIds) {
        $primarySequenceId = [string]$Report.final_project.primary_sequence_ids.psobject.Properties[$sliceId].Value
        if ($primarySequenceId -ne [string]$Report.final_project.hero_sequence_id) {
            throw "Hero-assigned slice '$sliceId' used a different primary Sequence."
        }
    }
    if (
        [int]$Report.final_project.proxy_queued -ne 0 -or
        [int]$Report.final_project.proxy_running -ne 0 -or
        [int64]$Report.final_project.proxy_failures -ne 0
    ) {
        throw "Final Project retained non-quiescent or failed proxy work."
    }
    if (
        $Report.final_project.durable_reopen.session_identity_changed -ne $true -or
        $Report.final_project.durable_reopen.project_identity_preserved -ne $true
    ) {
        throw "Final Project durable reopen did not rotate Session identity while preserving Project identity."
    }
    $profiles = @($Report.final_project.exported_profiles)
    if ("H264High" -notin $profiles -or "HevcMain10" -notin $profiles) {
        throw "Final Project lost the reimported H.264 High or HEVC Main10 deliverable."
    }
    if (-not (Test-Path -LiteralPath $Report.final_project.project_path -PathType Leaf)) {
        throw "Final Project archive is absent after the complete Golden run."
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$fixtureRoot = Resolve-RepositoryPath $FixtureRoot
$contractPath = Join-Path $repositoryRoot "tests/validation/golden-project.json"
$contract = Get-Content -LiteralPath $contractPath -Raw | ConvertFrom-Json
$requiredSliceIds = @($contract.execution_slices | ForEach-Object { [string]$_.id })
$requiredPasses = [int]$contract.acceptance.consecutive_passes
$timestamp = [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ")
$runId = "$timestamp-complete-golden-$(([guid]::NewGuid().ToString('N')).Substring(0, 8))"
$runDirectory = Resolve-RepositoryPath (Join-Path $RunRoot $runId)
$gateWorkRoot = Join-Path $runDirectory "work"
$assetReportPath = Join-Path $runDirectory "reference-assets.json"
$buildLogPath = Join-Path $runDirectory "build.log"
$aggregateReportPath = Join-Path $runDirectory "complete-golden-consecutive-report.json"
$goldenExecutable = Join-Path $repositoryRoot "target/debug/mondrian-golden.exe"
$buildArguments = @(
    "build",
    "-p", "mondrian-app",
    "--features", "validation",
    "--bin", "mondrian-golden"
    "-j", "1"
)
New-Item -ItemType Directory -Force -Path $gateWorkRoot | Out-Null

$failurePhase = $null
$failureMessage = $null
$buildResult = $null
$runEvidence = @()
$reports = @()
$oldRunRoot = [Environment]::GetEnvironmentVariable(
    "MONDRIAN_GOLDEN_COMPOSED_RUN_ROOT",
    "Process"
)
$oldCargoIncremental = [Environment]::GetEnvironmentVariable(
    "CARGO_INCREMENTAL",
    "Process"
)
$oldFixtureRoot = [Environment]::GetEnvironmentVariable(
    "MONDRIAN_GOLDEN_FIXTURE_ROOT",
    "Process"
)
try {
    $failurePhase = "contract-and-fixture-validation"
    $global:LASTEXITCODE = 0
    & (Join-Path $PSScriptRoot "validate-reference-assets.ps1") `
        -Tier Pr `
        -Scope All `
        -FixtureRoot $fixtureRoot `
        -OutputPath $assetReportPath
    if ($LASTEXITCODE -ne 0) {
        throw "Golden reference validation failed with exit code $LASTEXITCODE."
    }

    $failurePhase = "validation-binary-build"
    [Environment]::SetEnvironmentVariable(
        "CARGO_INCREMENTAL",
        "0",
        "Process"
    )
    $buildResult = Invoke-BoundedPlaybackGateProcess `
        "cargo" `
        $buildArguments `
        $repositoryRoot `
        $ProcessTimeoutSeconds `
        $buildLogPath
    if ($buildResult.timed_out -or $buildResult.exit_code -ne 0) {
        throw "Complete Golden validation binary did not build successfully."
    }
    if (-not (Test-Path -LiteralPath $goldenExecutable -PathType Leaf)) {
        throw "Complete Golden validation binary is absent after a successful build."
    }

    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_COMPOSED_RUN_ROOT",
        $gateWorkRoot,
        "Process"
    )
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_FIXTURE_ROOT",
        $fixtureRoot,
        "Process"
    )
    for ($index = 1; $index -le $requiredPasses; $index++) {
        $failurePhase = "complete-golden-pass-$index"
        $passDirectory = Join-Path $runDirectory "pass-$index"
        $reportPath = Join-Path $passDirectory "complete-golden-report.json"
        $logPath = Join-Path $passDirectory "process.log"
        New-Item -ItemType Directory -Force -Path $passDirectory | Out-Null
        $arguments = @("complete-run", "--output", $reportPath)
        $processResult = Invoke-TerminalReportGateProcess `
            $goldenExecutable `
            $arguments `
            $repositoryRoot `
            $ProcessTimeoutSeconds `
            $logPath `
            $reportPath `
            $NaturalExitGraceSeconds
        if ($processResult.timed_out) {
            throw "Complete Golden pass $index exceeded its external deadline."
        }
        if (-not $processResult.terminal_report_observed) {
            throw "Complete Golden pass $index exited without a terminal report."
        }

        $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
        Assert-CompleteGoldenReport $report $contract $requiredSliceIds
        if (-not $processResult.forced_after_report -and $processResult.exit_code -ne 0) {
            throw "Complete Golden pass $index returned exit code $($processResult.exit_code)."
        }
        $reports += $report
        $runEvidence += [pscustomobject][ordered]@{
            ordinal = $index
            report_path = $reportPath
            report_sha256 = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash.ToLowerInvariant()
            process_log_path = $logPath
            elapsed_ms = [int64]$processResult.elapsed_ms
            exit_code = $processResult.exit_code
            forced_after_terminal_report = [bool]$processResult.forced_after_report
            run_id = [string]$report.run_id
            project_id = [string]$report.final_project.project_id
        }
    }

    if (@($reports.run_id | Sort-Object -Unique).Count -ne $requiredPasses) {
        throw "Consecutive Golden passes reused a run identity."
    }
    if (@($reports.final_project.project_id | Sort-Object -Unique).Count -ne $requiredPasses) {
        throw "Consecutive Golden passes reused a Project identity."
    }
    $failurePhase = $null
} catch {
    $failureMessage = $_.Exception.Message
} finally {
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_COMPOSED_RUN_ROOT",
        $oldRunRoot,
        "Process"
    )
    [Environment]::SetEnvironmentVariable(
        "CARGO_INCREMENTAL",
        $oldCargoIncremental,
        "Process"
    )
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_FIXTURE_ROOT",
        $oldFixtureRoot,
        "Process"
    )
}

$passed = $null -eq $failureMessage -and $reports.Count -eq $requiredPasses
$aggregateReport = [ordered]@{
    schema_version = 1
    profile = "windows-alpha-complete-golden-project-consecutive"
    scope = "complete-golden-project"
    contract_id = [string]$contract.id
    status = if ($passed) { "passed" } else { "failed" }
    complete_golden_project = $passed
    required_consecutive_passes = $requiredPasses
    consecutive_passes = $reports.Count
    started_at_utc = $timestamp
    contract_path = $contractPath
    fixture_root = $fixtureRoot
    build = [ordered]@{
        command = "cargo $($buildArguments -join ' ')"
        incremental = $false
        log_path = $buildLogPath
        elapsed_ms = if ($null -eq $buildResult) { $null } else { [int64]$buildResult.elapsed_ms }
        timed_out = if ($null -eq $buildResult) { $false } else { [bool]$buildResult.timed_out }
        exit_code = if ($null -eq $buildResult) { $null } else { $buildResult.exit_code }
    }
    evidence = [ordered]@{
        reference_assets_path = $assetReportPath
        passes = $runEvidence
    }
    failure_phase = $failurePhase
    failure_message = $failureMessage
}
$aggregateReport | ConvertTo-Json -Depth 16 | Set-Content `
    -LiteralPath $aggregateReportPath `
    -Encoding utf8

Write-Host "Complete Golden Project gate: $($aggregateReport.status)"
Write-Host "  consecutive passes: $($reports.Count)/$requiredPasses"
Write-Host "  run: $aggregateReportPath"
if (-not $passed) {
    Write-Host "  failed phase: $failurePhase"
    Write-Host "  reason: $failureMessage"
    exit 1
}
