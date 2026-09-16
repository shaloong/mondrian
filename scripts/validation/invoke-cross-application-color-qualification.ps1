param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$EvidenceManifestPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineReportPath,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{40}$")][string]$ExpectedSourceSha,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$OutputDirectory,
    [string]$PolicyPath = "tests/validation/cross-application-color-qualification.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Read-JsonObject([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Label is missing: $Path" }
    try { return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json }
    catch { throw "$Label is not valid JSON: $($_.Exception.Message)" }
}

function Get-LowerSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if (@(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label does not exactly match the sealed qualification contract."
    }
}

function Invoke-BoundedCargo([string[]]$Arguments, [int]$TimeoutSeconds, [string]$StdoutPath, [string]$StderrPath) {
    $cargo = (Get-Command cargo -ErrorAction Stop).Source
    $process = Start-Process -FilePath $cargo -ArgumentList $Arguments `
        -WorkingDirectory $script:repositoryRoot -WindowStyle Hidden `
        -RedirectStandardOutput $StdoutPath -RedirectStandardError $StderrPath -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        $null = $process.WaitForExit(1000)
    }
    if (-not $process.HasExited) {
        try { $process.Kill($true); $process.WaitForExit() } catch { }
        throw "Cross-application qualification exceeded its $TimeoutSeconds second deadline."
    }
    $process.WaitForExit()
    return $process.ExitCode
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$policyAbsolute = Resolve-RepositoryPath $PolicyPath
$runtimeProfileAbsolute = Resolve-RepositoryPath $RuntimeProfilePath
$evidenceAbsolute = Resolve-RepositoryPath $EvidenceManifestPath
$machineAbsolute = Resolve-RepositoryPath $MachineReportPath
$outputAbsolute = Resolve-RepositoryPath $OutputDirectory
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()

$policy = Read-JsonObject $policyAbsolute "cross-application qualification policy"
$runtimeProfile = Read-JsonObject $runtimeProfileAbsolute "runtime qualification profile"
$evidence = Read-JsonObject $evidenceAbsolute "sealed evidence manifest"
$null = Read-JsonObject $machineAbsolute "machine report"
$policyHash = Get-LowerSha256 $policyAbsolute
$runtimeProfileHash = Get-LowerSha256 $runtimeProfileAbsolute
$evidenceManifestHash = Get-LowerSha256 $evidenceAbsolute
$machineReportHash = Get-LowerSha256 $machineAbsolute
if ($policy.schema_version -notin @(1, 2) -or $policy.execution_policy -ne "sealed-required") {
    throw "Cross-application qualification policy must use a supported schema and sealed-required."
}
if (
    $policy.local_restricted_evidence_required -ne $true -or
    $policy.exact_versions_and_builds_required -ne $true -or
    $policy.clean_source_required -ne $true -or
    $policy.single_run_identity_required -ne $true -or
    $policy.bounded_decode_required -ne $true -or
    $policy.pairwise_matrix_required -ne $true -or
    $policy.public_specification_oracle_is_separate -ne $true
) {
    throw "Cross-application qualification policy weakened a mandatory invariant."
}
$producerScope = 'full_commercial_matrix'
$expectedProducers = @('mondrian', 'blender', 'davinci_resolve', 'adobe_premiere_pro')
if ($policy.schema_version -eq 2) {
    if ($policy.producer_scope -cne 'blender_and_premiere' -or $runtimeProfile.producer_scope -cne 'blender_and_premiere') {
        throw 'Local producer policy and runtime profile must explicitly bind Blender/Premiere scope.'
    }
    $producerScope = 'blender_and_premiere'
    $expectedProducers = @('mondrian', 'blender', 'adobe_premiere_pro')
}
Assert-ExactStringSet $expectedProducers @($policy.required_producers) 'required producer set'
if ($runtimeProfile.schema_version -ne $policy.runtime_profile_schema_version) {
    throw "Runtime profile schema does not match the sealed policy."
}
Assert-ExactStringSet @($policy.required_producers) @($runtimeProfile.required_producers.producer) `
    "runtime profile producer set"
if ($evidence.schema_version -ne 1 -or $evidence.source_clean -ne $true) {
    throw "Evidence manifest must be schema 1 and bind a clean source checkout."
}
if ([string]$evidence.source_revision -ne $sourceSha) {
    throw "Evidence manifest source SHA does not match the requested source revision."
}
if ($machineReportHash -ne [string]$evidence.machine_report_sha256) {
    throw "Machine report digest does not match the evidence manifest."
}
$stimulusAbsolute = Resolve-RepositoryPath ([string]$policy.stimulus.path)
$stimulusHash = Get-LowerSha256 $stimulusAbsolute
if ($stimulusHash -ne [string]$policy.stimulus.sha256 -or
    $stimulusHash -ne [string]$runtimeProfile.stimulus_sha256) {
    throw "Runtime profile and policy do not bind the checked-in stimulus bytes."
}

$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha) {
    throw "Checked-out source SHA '$headSha' does not match '$sourceSha'."
}
if (@(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Sealed cross-application qualification requires a clean checkout."
}
if (Test-Path -LiteralPath $outputAbsolute) {
    throw "Cross-application qualification output directory already exists: $outputAbsolute"
}
New-Item -ItemType Directory -Path $outputAbsolute -ErrorAction Stop | Out-Null

$reportPath = Join-Path $outputAbsolute "qualification-report.json"
$stdoutPath = Join-Path $outputAbsolute "qualification.stdout.log"
$stderrPath = Join-Path $outputAbsolute "qualification.stderr.log"
$previousProfile = [Environment]::GetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_PROFILE", "Process")
$previousEvidence = [Environment]::GetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_EVIDENCE", "Process")
$previousReport = [Environment]::GetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_REPORT", "Process")
$previousIncremental = [Environment]::GetEnvironmentVariable("CARGO_INCREMENTAL", "Process")
try {
    [Environment]::SetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_PROFILE", $runtimeProfileAbsolute, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_EVIDENCE", $evidenceAbsolute, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_REPORT", $reportPath, "Process")
    [Environment]::SetEnvironmentVariable("CARGO_INCREMENTAL", "0", "Process")
    $arguments = @(
        "test", "-p", "mondrian-renderer", "--test", [string]$policy.runner.integration_test,
        "-j", [string]$policy.runner.serial_cargo_jobs,
        [string]$policy.runner.exact_test, "--", "--exact", "--ignored", "--nocapture"
    )
    $exitCode = Invoke-BoundedCargo $arguments ([int]$policy.runner.timeout_seconds) $stdoutPath $stderrPath
} finally {
    [Environment]::SetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_PROFILE", $previousProfile, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_EVIDENCE", $previousEvidence, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_CROSS_APPLICATION_REPORT", $previousReport, "Process")
    [Environment]::SetEnvironmentVariable("CARGO_INCREMENTAL", $previousIncremental, "Process")
}
if ($exitCode -ne 0) { throw "Cross-application qualification failed with exit code $exitCode." }
$endHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $endHeadSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Source changed or became dirty during cross-application qualification."
}
if ((Get-LowerSha256 $policyAbsolute) -ne $policyHash -or
    (Get-LowerSha256 $runtimeProfileAbsolute) -ne $runtimeProfileHash -or
    (Get-LowerSha256 $evidenceAbsolute) -ne $evidenceManifestHash -or
    (Get-LowerSha256 $machineAbsolute) -ne $machineReportHash -or
    (Get-LowerSha256 $stimulusAbsolute) -ne $stimulusHash) {
    throw "Qualification policy, profile, evidence, machine, or stimulus changed during execution."
}
$combinedOutput = (Get-Content -LiteralPath $stdoutPath -Raw) + "`n" + `
    (Get-Content -LiteralPath $stderrPath -Raw)
if ($combinedOutput -notmatch 'test result: ok\. 1 passed; 0 failed;') {
    throw "Cross-application qualification did not execute exactly one passing test."
}
if ($combinedOutput -match '(?i)skipping|0 passed') {
    throw "Cross-application qualification emitted a forbidden skip."
}
if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
    throw "Cross-application qualification emitted no report."
}
$qualification = Read-JsonObject $reportPath "qualification report"
if ($qualification.schema_version -ne $policy.runner.report_schema_version -or
    $qualification.status -ne "qualified" -or
    $qualification.producer_scope -cne $producerScope -or
    @($qualification.missing_artifacts).Count -ne 0 -or
    [string]$qualification.source_revision -ne $sourceSha -or
    [string]$qualification.machine_report_sha256 -ne $machineReportHash -or
    [string]::IsNullOrWhiteSpace([string]$qualification.profile_sha256) -or
    [string]::IsNullOrWhiteSpace([string]$qualification.evidence_sha256)) {
    throw "Cross-application qualification report is not complete, passing, and source-bound."
}
Assert-ExactStringSet @($policy.required_producers) @($qualification.artifact_evidence.producer) `
    "qualified artifact producer set"

$sealed = [ordered]@{
    schema_version = 1
    policy = [ordered]@{ id = [string]$policy.id; sha256 = $policyHash }
    runtime_profile_sha256 = $runtimeProfileHash
    stimulus_sha256 = $stimulusHash
    evidence_manifest_sha256 = $evidenceManifestHash
    machine_report_sha256 = $machineReportHash
    source_sha = $sourceSha
    status = "qualified"
    producer_scope = $producerScope
    qualification_report_sha256 = Get-LowerSha256 $reportPath
    stdout_sha256 = Get-LowerSha256 $stdoutPath
    stderr_sha256 = Get-LowerSha256 $stderrPath
}
$sealed | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath `
    (Join-Path $outputAbsolute "sealed-evidence.json") -Encoding utf8NoBOM
