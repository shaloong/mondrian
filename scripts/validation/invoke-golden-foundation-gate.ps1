param(
    [switch]$RegenerateGeneratedFixture,
    [ValidateRange(60, 3600)][int]$ProcessTimeoutSeconds = 600,
    [string]$RunRoot = "target/validation/runs"
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

function Invoke-ScriptChecked([string]$Path, [hashtable]$Parameters) {
    $global:LASTEXITCODE = 0
    & $Path @Parameters
    if ($LASTEXITCODE -ne 0) {
        throw "Validation script failed with exit code ${LASTEXITCODE}: $Path"
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$timestamp = [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ")
$runId = "$timestamp-golden-foundation-$(([guid]::NewGuid().ToString('N')).Substring(0, 8))"
$runDirectory = Resolve-RepositoryPath (Join-Path $RunRoot $runId)
$gateWorkRoot = Join-Path $runDirectory "work"
$assetReportPath = Join-Path $runDirectory "reference-assets.json"
$gateReportPath = Join-Path $runDirectory "golden-foundation-report.json"
$cargoLogPath = Join-Path $runDirectory "golden-foundation-cargo.log"
$runReportPath = Join-Path $runDirectory "golden-foundation-run.json"
$cargoArguments = @(
    "test",
    "-p", "mondrian-app",
    "--lib",
    "golden_project_foundation_audio_authoring_gate",
    "-j1",
    "--",
    "--ignored",
    "--nocapture",
    "--test-threads=1"
)
New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null

$oldOutput = [Environment]::GetEnvironmentVariable(
    "MONDRIAN_GOLDEN_FOUNDATION_OUTPUT",
    "Process"
)
$oldRunRoot = [Environment]::GetEnvironmentVariable(
    "MONDRIAN_GOLDEN_FOUNDATION_RUN_ROOT",
    "Process"
)
$failurePhase = $null
$failureMessage = $null
$processResult = $null
$gateReport = $null
try {
    if ($RegenerateGeneratedFixture) {
        $failurePhase = "fixture-generation"
        Invoke-ScriptChecked (Join-Path $PSScriptRoot "generate-golden-project-media.ps1") @{
            Force = $true
        }
    }

    $failurePhase = "contract-and-fixture-validation"
    Invoke-ScriptChecked (Join-Path $PSScriptRoot "validate-reference-assets.ps1") @{
        Tier = "Pr"
        Scope = "All"
        OutputPath = $assetReportPath
    }

    $failurePhase = "foundation-product-workflow"
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_FOUNDATION_OUTPUT",
        $gateReportPath,
        "Process"
    )
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_FOUNDATION_RUN_ROOT",
        $gateWorkRoot,
        "Process"
    )
    $processResult = Invoke-BoundedPlaybackGateProcess `
        "cargo" `
        $cargoArguments `
        $repositoryRoot `
        $ProcessTimeoutSeconds `
        $cargoLogPath

    if (Test-Path -LiteralPath $gateReportPath -PathType Leaf) {
        $gateReport = Get-Content -LiteralPath $gateReportPath -Raw | ConvertFrom-Json
    }
    if ($processResult.timed_out) {
        throw "Golden foundation gate exceeded its external process deadline."
    }
    if ($processResult.exit_code -ne 0) {
        throw "Golden foundation gate process failed with exit code $($processResult.exit_code)."
    }
    if ($null -eq $gateReport -or $gateReport.status -ne "passed" -or $gateReport.profile -ne "foundation-audio-authoring-v1") {
        throw "Golden foundation gate did not produce the expected passing structured report."
    }
    $failurePhase = $null
} catch {
    $failureMessage = $_.Exception.Message
} finally {
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_FOUNDATION_OUTPUT",
        $oldOutput,
        "Process"
    )
    [Environment]::SetEnvironmentVariable(
        "MONDRIAN_GOLDEN_FOUNDATION_RUN_ROOT",
        $oldRunRoot,
        "Process"
    )
}

$passed = $null -eq $failureMessage
$runReport = [ordered]@{
    schema_version = 1
    profile = "foundation-audio-authoring-v1"
    scope = "partial-golden-execution-slice"
    complete_golden_project = $false
    status = if ($passed) { "passed" } else { "failed" }
    started_at_utc = $timestamp
    process = [ordered]@{
        command = "cargo $($cargoArguments -join ' ')"
        timeout_seconds = $ProcessTimeoutSeconds
        elapsed_ms = if ($null -eq $processResult) { $null } else { [int64]$processResult.elapsed_ms }
        timed_out = if ($null -eq $processResult) { $false } else { [bool]$processResult.timed_out }
        exit_code = if ($null -eq $processResult) { $null } else { $processResult.exit_code }
        log_path = $cargoLogPath
    }
    evidence = [ordered]@{
        asset_validation_path = $assetReportPath
        gate_report_path = $gateReportPath
        gate_report_sha256 = if (Test-Path -LiteralPath $gateReportPath -PathType Leaf) {
            (Get-FileHash -LiteralPath $gateReportPath -Algorithm SHA256).Hash.ToLowerInvariant()
        } else {
            $null
        }
    }
    failure_phase = $failurePhase
    failure_message = $failureMessage
}
$runReport | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $runReportPath -Encoding utf8

Write-Host "Golden foundation gate: $($runReport.status)"
Write-Host "  run: $runReportPath"
if (-not $passed) {
    Write-Host "  failed phase: $failurePhase"
    Write-Host "  reason: $failureMessage"
    exit 1
}
