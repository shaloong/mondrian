param(
    [Parameter(Mandatory = $true, ParameterSetName = "Create")]
    [ValidateNotNullOrEmpty()]
    [string]$GoldenReportPath,

    [Parameter(Mandatory = $true, ParameterSetName = "Create")]
    [ValidateNotNullOrEmpty()]
    [string]$PlaybackReportPath,

    [Parameter(Mandatory = $true, ParameterSetName = "Create")]
    [ValidateNotNullOrEmpty()]
    [string]$OutputDirectory,

    [Parameter(Mandatory = $true, ParameterSetName = "Verify")]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceDirectory,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[0-9a-fA-F]{40}$")]
    [string]$ExpectedSourceSha
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Read-JsonObject([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }
    try {
        return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    } catch {
        throw "$Label is not valid JSON: $($_.Exception.Message)"
    }
}

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if (@(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label does not exactly match the commercial engine contract."
    }
}

function Assert-SourceAttestation([object]$Evidence, [string]$Label, [string]$SourceSha) {
    if (
        $Evidence.starting_revision -ne $SourceSha -or
        $Evidence.ending_revision -ne $SourceSha -or
        $Evidence.starting_dirty -ne $false -or
        $Evidence.ending_dirty -ne $false
    ) {
        throw "$Label is not bound to the expected clean and stable source SHA."
    }
}

function Assert-GoldenReport([object]$Report, [object]$Contract, [string]$SourceSha) {
    if (
        $Report.schema_version -ne $Contract.complete_golden.report_schema_version -or
        $Report.contract_id -ne $Contract.complete_golden.contract_id -or
        $Report.status -ne $Contract.complete_golden.required_status -or
        $Report.baseline_eligible -ne $true -or
        $Report.complete_golden_project -ne $true -or
        $Report.required_consecutive_passes -ne $Contract.complete_golden.required_consecutive_passes -or
        $Report.consecutive_passes -ne $Contract.complete_golden.required_consecutive_passes -or
        $Report.build.timeout_seconds -ne $Contract.complete_golden.build_timeout_seconds -or
        $Report.timeouts.build_timeout_seconds -ne $Contract.complete_golden.build_timeout_seconds -or
        $Report.timeouts.process_timeout_seconds -ne $Contract.complete_golden.process_timeout_seconds -or
        $Report.timeouts.contract_matched -ne $true -or
        $Report.source_attestation.stable -ne $true -or
        $Report.source_attestation.allow_dirty_diagnostic -ne $false
    ) {
        throw "Complete Golden evidence is not baseline-eligible under the commercial engine contract."
    }
    Assert-SourceAttestation $Report.source_attestation "Complete Golden evidence" $SourceSha
    if ([string]$Report.build.executable_sha256 -notmatch "^[0-9a-f]{64}$") {
        throw "Complete Golden evidence has no valid executable SHA-256."
    }
    $passEvidence = @($Report.evidence.passes)
    if ($passEvidence.Count -ne $Contract.complete_golden.required_consecutive_passes) {
        throw "Complete Golden evidence does not retain every required pass."
    }
    if (@($passEvidence | Where-Object { $_.process_timeout_seconds -ne $Contract.complete_golden.process_timeout_seconds }).Count -ne 0) {
        throw "Complete Golden evidence did not apply the contract-owned process timeout to every pass."
    }
    if (@($passEvidence.run_id | Sort-Object -Unique).Count -ne $passEvidence.Count) {
        throw "Complete Golden evidence reused a run identity."
    }
    if (@($passEvidence.project_id | Sort-Object -Unique).Count -ne $passEvidence.Count) {
        throw "Complete Golden evidence reused a Project identity."
    }
}

function Assert-PlaybackReport(
    [object]$Report,
    [object]$Contract,
    [object]$Plan,
    [string]$PlanHash,
    [string]$SourceSha
) {
    if (
        $Report.schema_version -ne $Contract.playback_reference.report_schema_version -or
        $Report.plan.id -ne $Contract.playback_reference.plan_id -or
        $Report.plan.sha256 -ne $PlanHash -or
        $Report.status -ne $Contract.playback_reference.required_status -or
        $Report.baseline_eligible -ne $true -or
        $Report.complete_gate_set -ne $true -or
        $Report.all_selected_gates_ran -ne $true -or
        $Report.machine_validation.status -ne "passed" -or
        $Report.asset_validation.status -ne "passed" -or
        $Report.failure -ne $null -or
        $Report.diagnostic_execution.allow_dirty_requested -ne $false -or
        $Report.diagnostic_execution.allow_unqualified_machine_requested -ne $false -or
        $Report.diagnostic_execution.unqualified_machine_override_applied -ne $false
    ) {
        throw "Playback reference evidence is not baseline-eligible under the commercial engine contract."
    }
    Assert-SourceAttestation $Report.git "Playback reference evidence" $SourceSha
    Assert-ExactStringSet $Contract.playback_reference.required_gate_ids $Report.selected_gate_ids "Selected playback gates"
    $gateReports = @($Report.gates)
    Assert-ExactStringSet $Contract.playback_reference.required_gate_ids @($gateReports | ForEach-Object { $_.gate_id }) "Playback gate reports"
    foreach ($gate in $gateReports) {
        $gateContract = @($Plan.gates | Where-Object { $_.id -eq $gate.gate_id })
        if ($gateContract.Count -ne 1) {
            throw "Playback gate '$($gate.gate_id)' has no unique current plan entry."
        }
        if (
            $gate.passed -ne $true -or
            $gate.report_passed -ne $true -or
            $gate.observed_report_profile -ne $gateContract[0].expected_report_profile -or
            $gate.cargo_exit_code -ne 0 -or
            $gate.process_timed_out -ne $false -or
            $gate.test_build.exit_code -ne 0 -or
            $gate.test_build.timed_out -ne $false -or
            [string]$gate.fixture.sha256 -notmatch "^[0-9a-f]{64}$" -or
            [string]$gate.fixture.attestation_sha256 -notmatch "^[0-9a-f]{64}$"
        ) {
            throw "Playback gate '$($gate.gate_id)' has incomplete or failing execution evidence."
        }
        if (
            $gate.decode_progress.required -eq $true -and
            ($gate.decode_progress.present -ne $true -or [string]$gate.decode_progress.sha256 -notmatch "^[0-9a-f]{64}$")
        ) {
            throw "Playback gate '$($gate.gate_id)' has no valid required decode-progress journal."
        }
        if (
            $gate.packaged_demux_worker.required -eq $true -and
            [string]$gate.packaged_demux_worker.sha256 -notmatch "^[0-9a-f]{64}$"
        ) {
            throw "Playback gate '$($gate.gate_id)' has no valid packaged demux worker identity."
        }
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$contractPath = Resolve-RepositoryPath "tests/validation/windows-commercial-engine.json"
$goldenContractPath = Resolve-RepositoryPath "tests/validation/golden-project.json"
$playbackPlanPath = Resolve-RepositoryPath "tests/validation/playback-reference-gates.json"
$contract = Read-JsonObject $contractPath "Commercial engine contract"
$goldenContract = Read-JsonObject $goldenContractPath "Golden Project contract"
$playbackPlan = Read-JsonObject $playbackPlanPath "Playback reference plan"
$playbackPlanHash = (Get-FileHash -LiteralPath $playbackPlanPath -Algorithm SHA256).Hash.ToLowerInvariant()
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()
$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha) {
    throw "Checked-out source SHA '$headSha' does not match expected SHA '$sourceSha'."
}

if (
    $contract.schema_version -ne 1 -or
    $contract.complete_golden.contract_id -ne $goldenContract.id -or
    $contract.playback_reference.plan_id -ne $playbackPlan.id
) {
    throw "Commercial engine contract does not reference the current validation contracts."
}
Assert-ExactStringSet $contract.playback_reference.required_gate_ids $playbackPlan.baseline_acceptance.required_gate_ids "Contract playback gates"

if ($PSCmdlet.ParameterSetName -eq "Create") {
    $goldenPath = Resolve-RepositoryPath $GoldenReportPath
    $playbackPath = Resolve-RepositoryPath $PlaybackReportPath
    $evidenceRoot = Resolve-RepositoryPath $OutputDirectory
} else {
    $evidenceRoot = Resolve-RepositoryPath $EvidenceDirectory
    $goldenPath = Join-Path $evidenceRoot "complete-golden.json"
    $playbackPath = Join-Path $evidenceRoot "playback-reference.json"
}

$golden = Read-JsonObject $goldenPath "Complete Golden evidence"
$playback = Read-JsonObject $playbackPath "Playback reference evidence"
Assert-GoldenReport $golden $contract $sourceSha
Assert-PlaybackReport $playback $contract $playbackPlan $playbackPlanHash $sourceSha

if ($PSCmdlet.ParameterSetName -eq "Create") {
    New-Item -ItemType Directory -Force -Path $evidenceRoot | Out-Null
    $goldenCopy = Join-Path $evidenceRoot "complete-golden.json"
    $playbackCopy = Join-Path $evidenceRoot "playback-reference.json"
    Copy-Item -LiteralPath $goldenPath -Destination $goldenCopy -Force
    Copy-Item -LiteralPath $playbackPath -Destination $playbackCopy -Force
    $manifest = [ordered]@{
        schema_version = 1
        contract = [ordered]@{
            id = [string]$contract.id
            sha256 = (Get-FileHash -LiteralPath $contractPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        source_sha = $sourceSha
        release_artifact = [string]$contract.release_artifact
        generated_at_utc = [DateTime]::UtcNow.ToString("o")
        evidence = [ordered]@{
            complete_golden = [ordered]@{
                file = "complete-golden.json"
                sha256 = (Get-FileHash -LiteralPath $goldenCopy -Algorithm SHA256).Hash.ToLowerInvariant()
            }
            playback_reference = [ordered]@{
                file = "playback-reference.json"
                sha256 = (Get-FileHash -LiteralPath $playbackCopy -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
        status = "passed"
    }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $evidenceRoot "manifest.json") -Encoding utf8
} else {
    $manifestPath = Join-Path $evidenceRoot "manifest.json"
    $manifest = Read-JsonObject $manifestPath "Commercial engine manifest"
    $contractHash = (Get-FileHash -LiteralPath $contractPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $goldenHash = (Get-FileHash -LiteralPath $goldenPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $playbackHash = (Get-FileHash -LiteralPath $playbackPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if (
        $manifest.schema_version -ne 1 -or
        $manifest.contract.id -ne $contract.id -or
        $manifest.contract.sha256 -ne $contractHash -or
        $manifest.source_sha -ne $sourceSha -or
        $manifest.release_artifact -ne $contract.release_artifact -or
        $manifest.evidence.complete_golden.file -ne "complete-golden.json" -or
        $manifest.evidence.complete_golden.sha256 -ne $goldenHash -or
        $manifest.evidence.playback_reference.file -ne "playback-reference.json" -or
        $manifest.evidence.playback_reference.sha256 -ne $playbackHash -or
        $manifest.status -ne "passed"
    ) {
        throw "Commercial engine manifest identity or evidence hashes are invalid."
    }
}

Write-Host "Commercial engine evidence: passed for $sourceSha"
