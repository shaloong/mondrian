param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineId,
    [ValidateSet("All", "Video", "Audio")][string]$Gate = "All",
    [switch]$RegenerateGeneratedFixtures,
    [switch]$AllowDirtyDiagnostic,
    [switch]$AllowUnqualifiedDiagnostic,
    [string]$RunRoot = "target/validation/runs"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Invoke-ScriptChecked([string]$Path, [hashtable]$Parameters) {
    $global:LASTEXITCODE = 0
    & $Path @Parameters
    if ($LASTEXITCODE -ne 0) {
        throw "Validation script failed with exit code ${LASTEXITCODE}: $Path"
    }
}

function Read-LastJsonLine([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    $lines = @(Get-Content -LiteralPath $Path | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($lines.Count -eq 0) { return $null }
    return $lines[-1] | ConvertFrom-Json
}

function Select-GateReport([object]$Envelope, [string]$ReportPath) {
    if ($null -eq $Envelope) { return $null }
    if ($ReportPath -eq '$') { return $Envelope }
    $property = $Envelope.PSObject.Properties[$ReportPath]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Invoke-PlaybackGate([object]$GateContract, [object]$Fixture, [string]$RunDirectory) {
    $artifactPath = Resolve-RepositoryPath (Join-Path "tests/fixtures" ([string]$Fixture.path))
    $reportPath = Join-Path $RunDirectory "$($GateContract.id)-report.jsonl"
    $logPath = Join-Path $RunDirectory "$($GateContract.id)-cargo.log"
    Remove-Item -LiteralPath $reportPath, $logPath -Force -ErrorAction SilentlyContinue

    $oldMedia = [Environment]::GetEnvironmentVariable([string]$GateContract.media_environment, "Process")
    $oldPerf = [Environment]::GetEnvironmentVariable("MONDRIAN_PERF_OUTPUT", "Process")
    try {
        [Environment]::SetEnvironmentVariable([string]$GateContract.media_environment, $artifactPath, "Process")
        [Environment]::SetEnvironmentVariable("MONDRIAN_PERF_OUTPUT", $reportPath, "Process")
        $cargoArguments = @(
            "test", "-p", "mondrian-app", "--release", [string]$GateContract.cargo_test,
            "--", "--ignored", "--nocapture", "--test-threads=1"
        )
        & cargo @cargoArguments 2>&1 | Tee-Object -LiteralPath $logPath
        $exitCode = $LASTEXITCODE
    } finally {
        [Environment]::SetEnvironmentVariable([string]$GateContract.media_environment, $oldMedia, "Process")
        [Environment]::SetEnvironmentVariable("MONDRIAN_PERF_OUTPUT", $oldPerf, "Process")
    }

    $envelope = $null
    $reportReadError = $null
    try { $envelope = Read-LastJsonLine $reportPath } catch { $reportReadError = $_.Exception.Message }
    $gateReport = Select-GateReport $envelope ([string]$GateContract.expected_report_path)
    $profileObserved = if ($null -eq $gateReport) { $null } else { [string]$gateReport.profile }
    $reportPassed = $null -ne $gateReport -and $gateReport.passed -eq $true
    $passed = $exitCode -eq 0 -and $reportPassed -and $profileObserved -eq $GateContract.expected_report_profile
    $artifact = Get-Item -LiteralPath $artifactPath
    $attestationPath = "$artifactPath$($Fixture.generation.attestation_suffix)"
    return [pscustomobject]@{
        gate_id = [string]$GateContract.id
        fixture = [ordered]@{
            id = [string]$Fixture.id
            path = $artifactPath
            size_bytes = [int64]$artifact.Length
            sha256 = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
            recipe_path = [string]$Fixture.generation.recipe_path
            recipe_sha256 = [string]$Fixture.generation.recipe_sha256
            attestation_path = $attestationPath
            attestation_sha256 = (Get-FileHash -LiteralPath $attestationPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        command = "cargo $($cargoArguments -join ' ')"
        cargo_exit_code = $exitCode
        report_path = $reportPath
        log_path = $logPath
        report_read_error = $reportReadError
        expected_report_profile = [string]$GateContract.expected_report_profile
        observed_report_profile = $profileObserved
        report_passed = $reportPassed
        passed = $passed
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$planPath = Join-Path $repositoryRoot "tests/validation/playback-reference-gates.json"
$manifestPath = Join-Path $repositoryRoot "tests/validation/corpus-manifest.json"
$machineProfilePath = Join-Path $repositoryRoot "tests/validation/windows-alpha-reference.json"
$plan = Get-Content -LiteralPath $planPath -Raw | ConvertFrom-Json
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$machineProfile = Get-Content -LiteralPath $machineProfilePath -Raw | ConvertFrom-Json
if ($plan.schema_version -ne 2) { throw "Unsupported playback reference gate plan schema." }
if ($manifest.schema_version -ne 2) { throw "Unsupported corpus manifest schema." }
if ($machineProfile.id -ne $plan.machine_profile) { throw "Playback plan and machine profile disagree." }
$baselineMemoryClass = [string]$plan.baseline_machine_requirements.memory_class
$diagnosticWaivableMachineIssueCodes = @($plan.diagnostic_execution.waivable_machine_issue_codes | ForEach-Object { [string]$_ })
if ($plan.diagnostic_execution.explicit_unqualified_machine_opt_in_required -ne $true) {
    throw "Playback plan must require explicit opt-in for unqualified-machine diagnostics."
}

$selectedGateIds = @(switch ($Gate) {
    "All" { @("video", "audio") }
    "Video" { @("video") }
    "Audio" { @("audio") }
})
$selectedGates = @($plan.gates | Where-Object { $_.id -in $selectedGateIds })
if ($selectedGates.Count -ne $selectedGateIds.Count) { throw "Playback plan does not define every selected gate." }

$timestamp = [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ")
$runId = "$timestamp-$MachineId-$(([guid]::NewGuid().ToString('N')).Substring(0, 8))"
$runDirectory = Resolve-RepositoryPath (Join-Path $RunRoot $runId)
New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
$runStartedAt = [DateTime]::UtcNow

$machineReportPath = Join-Path $runDirectory "reference-machine.json"
$machineValidationPath = Join-Path $runDirectory "reference-machine-validation.json"
$assetReportPath = Join-Path $runDirectory "reference-assets.json"
$gateResults = [System.Collections.Generic.List[object]]::new()
$machineReport = $null
$machineValidation = $null
$assetValidation = $null
$failurePhase = $null
$failureMessage = $null
$machineValidationExitCode = $null
$unqualifiedDiagnosticApplied = $false
try {
    $failurePhase = "machine-capture"
    Invoke-ScriptChecked (Join-Path $PSScriptRoot "capture-windows-reference.ps1") @{
        MachineId = $MachineId
        OutputPath = $machineReportPath
    }
    $machineReport = Get-Content -LiteralPath $machineReportPath -Raw | ConvertFrom-Json

    $failurePhase = "machine-validation"
    $machineValidationArguments = @{
        ProfilePath = $machineProfilePath
        MachineReportPath = $machineReportPath
        OutputPath = $machineValidationPath
        RequiredMemoryClass = $baselineMemoryClass
        RequireBaselineEligibility = (-not $AllowDirtyDiagnostic -and $Gate -eq "All")
    }
    $global:LASTEXITCODE = 0
    & (Join-Path $PSScriptRoot "validate-windows-reference.ps1") @machineValidationArguments
    $machineValidationExitCode = $LASTEXITCODE
    $machineValidation = Get-Content -LiteralPath $machineValidationPath -Raw | ConvertFrom-Json
    if ($machineValidationExitCode -ne 0) {
        $observedIssueCodes = @($machineValidation.issues | ForEach-Object { [string]$_.code })
        $unwaivableIssueCodes = @($observedIssueCodes | Where-Object { $_ -notin $diagnosticWaivableMachineIssueCodes })
        if (-not $AllowUnqualifiedDiagnostic) {
            throw "Reference machine is not qualified; use -AllowUnqualifiedDiagnostic only for an explicitly waivable diagnostic run. Issues: $($observedIssueCodes -join ', ')"
        }
        if ($unwaivableIssueCodes.Count -gt 0) {
            throw "Reference machine has non-waivable execution-prerequisite issues: $($unwaivableIssueCodes -join ', ')"
        }
        $unqualifiedDiagnosticApplied = $true
    }

    if ($RegenerateGeneratedFixtures) {
        $failurePhase = "fixture-generation"
        Invoke-ScriptChecked (Join-Path $PSScriptRoot "generate-reference-playback-media.ps1") @{
            Profile = $Gate
            Force = $true
        }
    }

    $failurePhase = "asset-validation"
    Invoke-ScriptChecked (Join-Path $PSScriptRoot "validate-reference-assets.ps1") @{
        Tier = "Nightly"
        Scope = "Playback"
        OutputPath = $assetReportPath
    }
    $assetValidation = Get-Content -LiteralPath $assetReportPath -Raw | ConvertFrom-Json

    $fixtureById = @{}
    foreach ($fixture in $manifest.entries) { $fixtureById[$fixture.id] = $fixture }
    foreach ($gateContract in $selectedGates) {
        $failurePhase = "gate-$($gateContract.id)"
        if (-not $fixtureById.ContainsKey($gateContract.fixture_id)) { throw "Unknown fixture '$($gateContract.fixture_id)'." }
        $gateResults.Add((Invoke-PlaybackGate $gateContract $fixtureById[$gateContract.fixture_id] $runDirectory))
    }
    $failurePhase = $null
} catch {
    $failureMessage = $_.Exception.Message
}
if ($null -eq $machineReport -and (Test-Path -LiteralPath $machineReportPath -PathType Leaf)) {
    $machineReport = Get-Content -LiteralPath $machineReportPath -Raw | ConvertFrom-Json
}
if ($null -eq $machineValidation -and (Test-Path -LiteralPath $machineValidationPath -PathType Leaf)) {
    $machineValidation = Get-Content -LiteralPath $machineValidationPath -Raw | ConvertFrom-Json
}
if ($null -eq $assetValidation -and (Test-Path -LiteralPath $assetReportPath -PathType Leaf)) {
    $assetValidation = Get-Content -LiteralPath $assetReportPath -Raw | ConvertFrom-Json
}

$endingRevision = (git -C $repositoryRoot rev-parse HEAD).Trim()
$endingDirty = @(git -C $repositoryRoot status --porcelain).Count -gt 0
$allSelectedGatesRan = $gateResults.Count -eq $selectedGates.Count
$allGatesPassed = $allSelectedGatesRan -and @($gateResults | Where-Object { -not $_.passed }).Count -eq 0
$completeGateSet = @($plan.baseline_acceptance.required_gate_ids | Where-Object { $_ -notin $selectedGateIds }).Count -eq 0
$preflightPassed = $null -ne $assetValidation -and $assetValidation.status -eq "passed" -and $null -ne $machineValidation -and $machineValidation.status -eq "passed"
$startingRevision = if ($null -eq $machineReport) { $null } else { [string]$machineReport.git.revision }
$startingDirty = if ($null -eq $machineReport) { $null } else { [bool]$machineReport.git.dirty }
$baselineEligible = $null -eq $failureMessage -and $preflightPassed -and $allGatesPassed -and $completeGateSet -and -not $AllowDirtyDiagnostic -and -not $AllowUnqualifiedDiagnostic -and $startingDirty -eq $false -and -not $endingDirty -and $endingRevision -eq $startingRevision
$status = if ($null -ne $failureMessage) { "failed" } elseif (-not $allGatesPassed) { "failed" } elseif ($baselineEligible) { "passed-baseline" } else { "passed-diagnostic" }
$machineValidationIssueCodes = if ($null -eq $machineValidation) { @() } else { @($machineValidation.issues | ForEach-Object { [string]$_.code }) }
$evidence = [ordered]@{
    schema_version = 2
    run_id = $runId
    plan = [ordered]@{
        id = $plan.id
        path = $planPath
        sha256 = (Get-FileHash -LiteralPath $planPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    corpus = [ordered]@{
        revision = $manifest.corpus_revision
        path = $manifestPath
        sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    machine_profile = [ordered]@{
        id = $machineProfile.id
        path = $machineProfilePath
        sha256 = (Get-FileHash -LiteralPath $machineProfilePath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    started_at_utc = $runStartedAt.ToString("o")
    completed_at_utc = [DateTime]::UtcNow.ToString("o")
    selected_gate_ids = $selectedGateIds
    git = [ordered]@{
        starting_revision = $startingRevision
        ending_revision = $endingRevision
        starting_dirty = $startingDirty
        ending_dirty = $endingDirty
    }
    diagnostic_execution = [ordered]@{
        allow_dirty_requested = [bool]$AllowDirtyDiagnostic
        allow_unqualified_machine_requested = [bool]$AllowUnqualifiedDiagnostic
        unqualified_machine_override_applied = $unqualifiedDiagnosticApplied
        contract_waivable_machine_issue_codes = $diagnosticWaivableMachineIssueCodes
        observed_machine_issue_codes = $machineValidationIssueCodes
    }
    machine_report = [ordered]@{ path = $machineReportPath; present = Test-Path -LiteralPath $machineReportPath -PathType Leaf }
    machine_validation = [ordered]@{
        path = $machineValidationPath
        exit_code = $machineValidationExitCode
        status = if ($null -eq $machineValidation) { $null } else { $machineValidation.status }
        required_memory_class = if ($null -eq $machineValidation) { $baselineMemoryClass } else { $machineValidation.required_memory_class }
        observed_memory_class = if ($null -eq $machineValidation) { $null } else { $machineValidation.observed_memory_class }
    }
    asset_validation = [ordered]@{ path = $assetReportPath; status = if ($null -eq $assetValidation) { $null } else { $assetValidation.status } }
    gates = @($gateResults)
    all_selected_gates_ran = $allSelectedGatesRan
    complete_gate_set = $completeGateSet
    baseline_eligible = $baselineEligible
    failure = if ($null -eq $failureMessage) { $null } else { [ordered]@{ phase = $failurePhase; message = $failureMessage } }
    status = $status
}
$evidencePath = Join-Path $runDirectory "evidence.json"
$evidence | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $evidencePath -Encoding utf8
Write-Host "Playback reference run: $status"
Write-Host "Evidence: $evidencePath"
if ($status -eq "failed") { exit 1 }
