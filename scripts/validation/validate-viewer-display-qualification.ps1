param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$EvidenceDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{64}$")][string]$ExpectedRuntimeImageSha256,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedRunId,
    [string]$ProfilePath = "tests/validation/viewer-display-qualification.json",
    [string]$OutputPath = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$runtimeImageSha256 = $ExpectedRuntimeImageSha256.ToLowerInvariant()

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Path))
}

function Read-BoundedUtf8([string]$Path, [long]$MaximumBytes, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $MaximumBytes) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length -or $stream.Length -gt [int]::MaxValue) {
            throw "$Label changed or is too large for bounded parsing."
        }
        $bytes = [byte[]]::new([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -le 0) { throw "$Label ended before its admitted length." }
            $offset += $read
        }
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try { $hash = (($sha256.ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join "") }
        finally { $sha256.Dispose() }
        return [pscustomobject]@{
            text = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff)
            sha256 = $hash
        }
    } finally {
        $stream.Dispose()
    }
}

function Read-Json([string]$Path, [string]$Label) {
    $admission = Read-BoundedUtf8 $Path 8388608 $Label
    try { return [pscustomobject]@{ value = ($admission.text | ConvertFrom-Json); sha256 = $admission.sha256 } }
    catch { throw "$Label is invalid JSON: $($_.Exception.Message)" }
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$profile = (Read-Json (Resolve-RepositoryPath $ProfilePath) "Viewer display profile").value
$evidenceRoot = Resolve-RepositoryPath $EvidenceDirectory
Assert-True ($profile.schema_version -eq 1) "Viewer display profile schema must be 1."
Assert-True ($profile.execution_policy -eq "physical-display-hitl-required") "Viewer display qualification must remain HITL-required."
Assert-True (@($profile.scenarios).Count -eq 3) "Viewer display qualification requires P3, HDR-PQ, and ICC scenarios."

$operatorAdmission = Read-Json (Join-Path $evidenceRoot "operator-observation.json") "Operator observation"
$operator = $operatorAdmission.value
Assert-True ($operator.schema_version -eq 1) "Operator observation schema must be 1."
Assert-True (-not [string]::IsNullOrWhiteSpace([string]$operator.operator_id)) "Operator identity is required."
Assert-True (-not [string]::IsNullOrWhiteSpace([string]$operator.observed_at_utc)) "Operator timestamp is required."
Assert-True ([string]$operator.qualification_run_id -eq $ExpectedRunId) "Operator observation is bound to another qualification run."

$scenarioReports = [System.Collections.Generic.List[object]]::new()
$qualifiedProcessInstance = $null
$qualifiedProcessId = $null
$qualifiedAdapterIdentity = $null
$qualifiedDisplayTarget = $null
foreach ($scenario in @($profile.scenarios)) {
    $scenarioId = [string]$scenario.id
    $jsonlPath = Join-Path $evidenceRoot "$scenarioId.jsonl"
    if (-not (Test-Path -LiteralPath $jsonlPath -PathType Leaf)) {
        throw "Scenario '$scenarioId' has no product JSONL evidence: $jsonlPath"
    }
    $jsonlAdmission = Read-BoundedUtf8 $jsonlPath 67108864 "Scenario '$scenarioId' product JSONL"
    $records = @(
        $jsonlAdmission.text -split "`r?`n" |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            ForEach-Object { $_ | ConvertFrom-Json }
    )
    Assert-True ($records.Count -gt 0) "Scenario '$scenarioId' has no JSONL records."
    $processInstances = @($records | ForEach-Object { [string]$_.process_instance_id } | Sort-Object -Unique)
    $processIds = @($records | ForEach-Object { [string]$_.process_id } | Sort-Object -Unique)
    $runtimeImages = @($records | ForEach-Object { [string]$_.runtime_image_sha256 } | Sort-Object -Unique)
    $runIds = @($records | ForEach-Object { [string]$_.qualification_run_id } | Sort-Object -Unique)
    $adapterIdentities = @($records | ForEach-Object {
        "$($_.renderer_adapter.name)|$($_.renderer_adapter.vendor_id)|$($_.renderer_adapter.device_id)|$($_.renderer_adapter.device_type)|$($_.renderer_adapter.driver)|$($_.renderer_adapter.driver_info)|$($_.renderer_adapter.backend)"
    } | Sort-Object -Unique)
    $displayTargets = @($records | ForEach-Object {
        "$($_.display_target.name)|$($_.display_target.position[0])|$($_.display_target.position[1])|$($_.display_target.physical_size[0])|$($_.display_target.physical_size[1])|$($_.display_target.native_display_id)|$($_.display_target.native_display_path_id)|$($_.display_target.scale_factor_ppm)|$($_.display_target.refresh_rate_millihertz)"
    } | Sort-Object -Unique)
    $sequences = @($records | ForEach-Object { [uint64]$_.qualification_record_sequence })
    Assert-True ($processInstances.Count -eq 1 -and -not [string]::IsNullOrWhiteSpace($processInstances[0])) "Scenario '$scenarioId' splices process instances."
    Assert-True ($processIds.Count -eq 1 -and -not [string]::IsNullOrWhiteSpace($processIds[0])) "Scenario '$scenarioId' splices process IDs."
    Assert-True ($runtimeImages.Count -eq 1 -and $runtimeImages[0] -eq $runtimeImageSha256) "Scenario '$scenarioId' splices runtime images."
    Assert-True ($runIds.Count -eq 1 -and $runIds[0] -eq $ExpectedRunId) "Scenario '$scenarioId' is bound to another run nonce."
    Assert-True ($adapterIdentities.Count -eq 1 -and -not $adapterIdentities[0].Contains("||")) "Scenario '$scenarioId' splices or omits the active Renderer adapter."
    Assert-True ($displayTargets.Count -eq 1 -and -not [string]::IsNullOrWhiteSpace($displayTargets[0])) "Scenario '$scenarioId' splices the Window display target."
    foreach ($record in @($records)) {
        Assert-True (-not [string]::IsNullOrWhiteSpace([string]$record.renderer_adapter.name) -and
            -not [string]::IsNullOrWhiteSpace([string]$record.renderer_adapter.vendor_id) -and
            -not [string]::IsNullOrWhiteSpace([string]$record.renderer_adapter.device_id) -and
            -not [string]::IsNullOrWhiteSpace([string]$record.renderer_adapter.device_type) -and
            -not [string]::IsNullOrWhiteSpace([string]$record.renderer_adapter.driver) -and
            -not [string]::IsNullOrWhiteSpace([string]$record.renderer_adapter.backend) -and
            -not [string]::IsNullOrWhiteSpace([string]$record.display_target.native_display_path_id)) "Scenario '$scenarioId' omits active Renderer adapter or native display-path identity."
    }
    Assert-True (@($sequences | Sort-Object -Unique).Count -eq $sequences.Count) "Scenario '$scenarioId' repeats qualification record sequence numbers."
    for ($index = 1; $index -lt $sequences.Count; $index++) {
        Assert-True ($sequences[$index] -gt $sequences[$index - 1]) "Scenario '$scenarioId' has non-monotonic qualification records."
    }
    if ($null -eq $qualifiedProcessInstance) {
        $qualifiedProcessInstance = $processInstances[0]
        $qualifiedProcessId = $processIds[0]
        $qualifiedAdapterIdentity = $adapterIdentities[0]
        $qualifiedDisplayTarget = $displayTargets[0]
    } else {
        Assert-True ($qualifiedProcessInstance -eq $processInstances[0] -and
            $qualifiedProcessId -eq $processIds[0] -and
            $qualifiedAdapterIdentity -eq $adapterIdentities[0] -and
            $qualifiedDisplayTarget -eq $displayTargets[0]) "Scenario '$scenarioId' was spliced from another process, adapter, or display target."
    }
    $ready = @($records | Where-Object {
        $_.health.status -eq "Ready" -and
        $_.display_snapshot.validation_status -eq "Pass" -and
        $_.display_snapshot.surface_color_space -eq [string]$scenario.surface_color_space -and
        $_.last_frame_context.monitor_color_space -eq [string]$scenario.monitor_color_space
    })
    Assert-True ($ready.Count -gt 0) "Scenario '$scenarioId' never produced an exact Ready display record."
    $final = $ready[-1]
    Assert-True ([string]$final.runtime_image_sha256 -eq $runtimeImageSha256) "Scenario '$scenarioId' was captured from another runtime image."
    Assert-True ([string]$final.display_snapshot.contract_sha256 -match '^[0-9a-f]{64}$') "Scenario '$scenarioId' lacks the full Display Output Contract SHA-256."
    Assert-True ($null -ne $final.display_output_contract -and
        [string]$final.display_output_contract.surface_color_space -eq [string]$final.display_snapshot.surface_color_space -and
        [string]$final.display_output_contract.surface_format -eq [string]$final.display_snapshot.surface_format -and
        [string]$final.display_output_contract.surface_hdr_mode -eq [string]$final.display_snapshot.surface_hdr_mode -and
        [string]$final.display_output_contract.display_id.name -eq [string]$final.display_target.name -and
        [int]$final.display_output_contract.display_id.position[0] -eq [int]$final.display_target.position[0] -and
        [int]$final.display_output_contract.display_id.position[1] -eq [int]$final.display_target.position[1] -and
        [int]$final.display_output_contract.display_id.physical_size[0] -eq [int]$final.display_target.physical_size[0] -and
        [int]$final.display_output_contract.display_id.physical_size[1] -eq [int]$final.display_target.physical_size[1]) "Scenario '$scenarioId' lacks a replayable full Display Output Contract payload."
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
        Assert-True ([string]$final.display_calibration_identity_sha256 -match '^[0-9a-f]{64}$') "Scenario '$scenarioId' lacks the complete ICC processor identity."
    }
    $observation = @($operator.observations | Where-Object { $_.scenario_id -eq $scenarioId })
    Assert-True ($observation.Count -eq 1) "Scenario '$scenarioId' requires exactly one operator observation."
    Assert-True ($observation[0].passed -eq $true) "Scenario '$scenarioId' failed operator visual observation."
    Assert-True (-not [string]::IsNullOrWhiteSpace([string]$observation[0].display_identity)) "Scenario '$scenarioId' observation lacks display identity."
    Assert-True ([string]$observation[0].process_instance_id -eq $processInstances[0]) "Scenario '$scenarioId' operator attested another process instance."
    Assert-True ([string]$observation[0].output_contract_sha256 -eq [string]$final.display_snapshot.contract_sha256) "Scenario '$scenarioId' operator attested another Display Output Contract."
    $operatorDisplayTarget = "$($observation[0].display_target.name)|$($observation[0].display_target.position[0])|$($observation[0].display_target.position[1])|$($observation[0].display_target.physical_size[0])|$($observation[0].display_target.physical_size[1])|$($observation[0].display_target.native_display_id)|$($observation[0].display_target.native_display_path_id)|$($observation[0].display_target.scale_factor_ppm)|$($observation[0].display_target.refresh_rate_millihertz)"
    Assert-True ($operatorDisplayTarget -eq $displayTargets[0]) "Scenario '$scenarioId' operator attested another Window display target."
    $scenarioReports.Add([ordered]@{
        id = $scenarioId
        display_identity = [string]$observation[0].display_identity
        output_contract_sha256 = [string]$final.display_snapshot.contract_sha256
        contract_diagnostic_key = $final.display_snapshot.contract_diagnostic_key
        executed_runtime_image_sha256 = [string]$final.runtime_image_sha256
        qualification_run_id = $ExpectedRunId
        process_instance_id = $processInstances[0]
        process_id = $processIds[0]
        renderer_adapter = $final.renderer_adapter
        display_target = $final.display_target
        product_jsonl_sha256 = $jsonlAdmission.sha256
        carrier_active = [bool]$final.ui_surface_carrier_active
        passed = $true
    })
}

$report = [ordered]@{
    schema_version = 1
    profile = [string]$profile.id
    operator_id = [string]$operator.operator_id
    observed_at_utc = [string]$operator.observed_at_utc
    executed_runtime_image_sha256 = $runtimeImageSha256
    qualification_run_id = $ExpectedRunId
    operator_attestation_sha256 = $operatorAdmission.sha256
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
