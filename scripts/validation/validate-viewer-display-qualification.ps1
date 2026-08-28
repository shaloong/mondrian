param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$EvidenceDirectory,
    [string]$ProfilePath = "tests/validation/viewer-display-qualification.json",
    [string]$OutputPath = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Path))
}

function Read-Json([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Label is missing: $Path" }
    try { return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json }
    catch { throw "$Label is invalid JSON: $($_.Exception.Message)" }
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$profile = Read-Json (Resolve-RepositoryPath $ProfilePath) "Viewer display profile"
$evidenceRoot = Resolve-RepositoryPath $EvidenceDirectory
Assert-True ($profile.schema_version -eq 1) "Viewer display profile schema must be 1."
Assert-True ($profile.execution_policy -eq "physical-display-hitl-required") "Viewer display qualification must remain HITL-required."
Assert-True (@($profile.scenarios).Count -eq 3) "Viewer display qualification requires P3, HDR-PQ, and ICC scenarios."

$operator = Read-Json (Join-Path $evidenceRoot "operator-observation.json") "Operator observation"
Assert-True ($operator.schema_version -eq 1) "Operator observation schema must be 1."
Assert-True (-not [string]::IsNullOrWhiteSpace([string]$operator.operator_id)) "Operator identity is required."
Assert-True (-not [string]::IsNullOrWhiteSpace([string]$operator.observed_at_utc)) "Operator timestamp is required."

$scenarioReports = [System.Collections.Generic.List[object]]::new()
foreach ($scenario in @($profile.scenarios)) {
    $scenarioId = [string]$scenario.id
    $jsonlPath = Join-Path $evidenceRoot "$scenarioId.jsonl"
    if (-not (Test-Path -LiteralPath $jsonlPath -PathType Leaf)) {
        throw "Scenario '$scenarioId' has no product JSONL evidence: $jsonlPath"
    }
    $records = @(
        Get-Content -LiteralPath $jsonlPath |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            ForEach-Object { $_ | ConvertFrom-Json }
    )
    Assert-True ($records.Count -gt 0) "Scenario '$scenarioId' has no JSONL records."
    $ready = @($records | Where-Object {
        $_.health.status -eq "Ready" -and
        $_.display_snapshot.validation_status -eq "Pass" -and
        $_.display_snapshot.surface_color_space -eq [string]$scenario.surface_color_space -and
        $_.last_frame_context.monitor_color_space -eq [string]$scenario.monitor_color_space
    })
    Assert-True ($ready.Count -gt 0) "Scenario '$scenarioId' never produced an exact Ready display record."
    $final = $ready[-1]
    Assert-True ($final.display_snapshot.blocker_count -eq 0) "Scenario '$scenarioId' retained display blockers."
    Assert-True ($final.health.external_texture_registered -eq $true) "Scenario '$scenarioId' did not register the Viewer texture."
    Assert-True ($final.presented_external_texture_batches -gt 0) "Scenario '$scenarioId' did not present an external texture batch."
    Assert-True ($final.stage_readback_stages -eq 0) "Scenario '$scenarioId' crossed a GPU readback boundary."
    Assert-True ($final.ui_surface_carrier_active -eq [bool]$scenario.surface_carrier_active) "Scenario '$scenarioId' used the wrong UI surface carrier path."
    if ($scenario.transfer -eq "surface-code-values") {
        Assert-True ($final.presented_surface_code_value_batches -gt 0) "Scenario '$scenarioId' did not use target surface-code decoding."
        Assert-True ($final.presented_device_code_value_batches -eq 0) "Scenario '$scenarioId' incorrectly used ICC device-code transfer."
        Assert-True (@($records | Where-Object { $_.ui_surface_carrier_target_rebuilt -eq $true }).Count -gt 0) "Scenario '$scenarioId' lacks carrier allocation evidence."
        Assert-True ($final.ui_surface_carrier_target_rebuilt -eq $false) "Scenario '$scenarioId' lacks stable carrier reuse evidence."
    } else {
        Assert-True ($final.presented_device_code_value_batches -gt 0) "Scenario '$scenarioId' did not use ICC device-code transfer."
        Assert-True ([string]$final.display_snapshot.monitor_profile_status -like "Managed ICC calibration*") "Scenario '$scenarioId' lacks managed ICC processor proof."
    }
    $observation = @($operator.observations | Where-Object { $_.scenario_id -eq $scenarioId })
    Assert-True ($observation.Count -eq 1) "Scenario '$scenarioId' requires exactly one operator observation."
    Assert-True ($observation[0].passed -eq $true) "Scenario '$scenarioId' failed operator visual observation."
    Assert-True (-not [string]::IsNullOrWhiteSpace([string]$observation[0].display_identity)) "Scenario '$scenarioId' observation lacks display identity."
    $scenarioReports.Add([ordered]@{
        id = $scenarioId
        display_identity = [string]$observation[0].display_identity
        contract_diagnostic_key = $final.display_snapshot.contract_diagnostic_key
        carrier_active = [bool]$final.ui_surface_carrier_active
        passed = $true
    })
}

$report = [ordered]@{
    schema_version = 1
    profile = [string]$profile.id
    operator_id = [string]$operator.operator_id
    observed_at_utc = [string]$operator.observed_at_utc
    scenarios = $scenarioReports
    passed = $true
}
if (-not [string]::IsNullOrWhiteSpace($OutputPath)) {
    $outputAbsolute = Resolve-RepositoryPath $OutputPath
    $parent = Split-Path -Parent $outputAbsolute
    if (-not [string]::IsNullOrWhiteSpace($parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputAbsolute -Encoding utf8
}
$report | ConvertTo-Json -Depth 8
