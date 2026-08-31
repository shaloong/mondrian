param(
    [ValidateSet("Pr", "Nightly", "Release")][string]$Tier = "Pr",
    [ValidateSet("All", "Playback")][string]$Scope = "All",
    [string]$FixtureRoot = "tests/fixtures",
    [string]$ContractRoot = "tests/validation",
    [string]$OutputPath = "target/validation/reference-assets.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Add-Issue([string]$Severity, [string]$Code, [string]$Message) {
    $script:issues.Add([pscustomobject]@{ severity = $Severity; code = $Code; message = $Message })
}

function Resolve-ContainedPath([string]$Root, [string]$RelativePath, [string]$Description) {
    if ([IO.Path]::IsPathRooted($RelativePath)) {
        throw "$Description must be relative: $RelativePath"
    }
    $absoluteRoot = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $absolutePath = [IO.Path]::GetFullPath((Join-Path $absoluteRoot $RelativePath))
    $prefix = "$absoluteRoot$([IO.Path]::DirectorySeparatorChar)"
    if (-not $absolutePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description escapes its declared root: $RelativePath"
    }
    return $absolutePath
}

function Has-Property([object]$Value, [string]$Name) {
    return $null -ne $Value -and $null -ne $Value.PSObject.Properties[$Name]
}

$issues = [System.Collections.Generic.List[object]]::new()
$assetResults = [System.Collections.Generic.List[object]]::new()
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$fixtureRootAbsolute = if ([IO.Path]::IsPathRooted($FixtureRoot)) { [IO.Path]::GetFullPath($FixtureRoot) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $FixtureRoot)) }
$contractRootAbsolute = if ([IO.Path]::IsPathRooted($ContractRoot)) { [IO.Path]::GetFullPath($ContractRoot) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $ContractRoot)) }
$manifestPath = Join-Path $contractRootAbsolute "corpus-manifest.json"
$goldenPath = Join-Path $contractRootAbsolute "golden-project.json"
$stressPath = Join-Path $contractRootAbsolute "stress-project.json"
$machineProfilePath = Join-Path $contractRootAbsolute "windows-alpha-reference.json"
$playbackPlanPath = Join-Path $contractRootAbsolute "playback-reference-gates.json"
$commercialEnginePath = Join-Path $contractRootAbsolute "windows-commercial-engine.json"
$gpuColorProfilePath = Join-Path $contractRootAbsolute "gpu-color-qualification.json"
$platformGpuColorProfilePath = Join-Path $contractRootAbsolute "platform-gpu-color-gates.json"
$viewerDisplayProfilePath = Join-Path $contractRootAbsolute "viewer-display-qualification.json"
$realtimeMatrixPath = Join-Path $contractRootAbsolute "realtime-performance-matrix.json"
$crossApplicationProfilePath = Join-Path $contractRootAbsolute "cross-application-color-qualification.json"
$crossApplicationStimulusPath = Join-Path $contractRootAbsolute "cross-application-color-stimulus-v1.json"
$platformMatrixPath = Join-Path $contractRootAbsolute "platform-driver-display-matrix.json"
$contractPaths = @(
    $manifestPath,
    $goldenPath,
    $stressPath,
    $machineProfilePath,
    $playbackPlanPath,
    $commercialEnginePath,
    $gpuColorProfilePath,
    $platformGpuColorProfilePath,
    $viewerDisplayProfilePath
    $realtimeMatrixPath
    $crossApplicationProfilePath
    $crossApplicationStimulusPath
    $platformMatrixPath
)

foreach ($path in $contractPaths) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Add-Issue "error" "contract.missing" "Required contract is missing: $path"
    }
}
if ($issues.Count -gt 0) { throw "Validation contracts are incomplete." }

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$golden = Get-Content -LiteralPath $goldenPath -Raw | ConvertFrom-Json
$stress = Get-Content -LiteralPath $stressPath -Raw | ConvertFrom-Json
$playbackPlan = Get-Content -LiteralPath $playbackPlanPath -Raw | ConvertFrom-Json
$machineProfile = Get-Content -LiteralPath $machineProfilePath -Raw | ConvertFrom-Json
$commercialEngine = Get-Content -LiteralPath $commercialEnginePath -Raw | ConvertFrom-Json
$gpuColorProfile = Get-Content -LiteralPath $gpuColorProfilePath -Raw | ConvertFrom-Json
$platformGpuColorProfile = Get-Content -LiteralPath $platformGpuColorProfilePath -Raw | ConvertFrom-Json
$viewerDisplayProfile = Get-Content -LiteralPath $viewerDisplayProfilePath -Raw | ConvertFrom-Json
$realtimeMatrix = Get-Content -LiteralPath $realtimeMatrixPath -Raw | ConvertFrom-Json
$crossApplicationProfile = Get-Content -LiteralPath $crossApplicationProfilePath -Raw | ConvertFrom-Json
$crossApplicationStimulus = Get-Content -LiteralPath $crossApplicationStimulusPath -Raw | ConvertFrom-Json
$platformMatrix = Get-Content -LiteralPath $platformMatrixPath -Raw | ConvertFrom-Json
if ($manifest.schema_version -ne 2) { Add-Issue "error" "schema.unsupported" "Unsupported corpus schema version: $($manifest.schema_version)" }
if ($playbackPlan.schema_version -ne 4) { Add-Issue "error" "playback-plan.schema-unsupported" "Unsupported playback gate-plan schema: $($playbackPlan.schema_version)" }
if ($machineProfile.schema_version -ne 3) { Add-Issue "error" "machine-profile.schema-unsupported" "Unsupported Windows machine-profile schema: $($machineProfile.schema_version)" }
if ($commercialEngine.schema_version -ne 2) { Add-Issue "error" "commercial-engine.schema-unsupported" "Unsupported commercial engine schema: $($commercialEngine.schema_version)" }
if ($gpuColorProfile.schema_version -ne 1) { Add-Issue "error" "gpu-color.schema-unsupported" "Unsupported GPU color profile schema: $($gpuColorProfile.schema_version)" }
if ($gpuColorProfile.execution_policy -ne "sealed-required") { Add-Issue "error" "gpu-color.policy" "GPU color qualification must use sealed-required execution" }
if ($platformGpuColorProfile.schema_version -ne 1 -or
    $platformGpuColorProfile.single_test_result_required -ne $true -or
    $platformGpuColorProfile.diagnostic_skip_forbidden -ne $true -or
    @($platformGpuColorProfile.gates).Count -ne 8) {
    Add-Issue "error" "platform-gpu-color.contract" "Platform GPU color profile must retain eight measured, non-skipped single-test gates"
}
$viewerDisplayScenarioIds = @($viewerDisplayProfile.scenarios | ForEach-Object { [string]$_.id })
if ($viewerDisplayProfile.schema_version -ne 1) { Add-Issue "error" "viewer-display.schema-unsupported" "Unsupported Viewer display profile schema: $($viewerDisplayProfile.schema_version)" }
if ($viewerDisplayProfile.execution_policy -ne "physical-display-hitl-required") { Add-Issue "error" "viewer-display.policy" "Viewer display qualification must remain physical-display HITL-required" }
$realtimeGateIds = @($realtimeMatrix.gates | ForEach-Object { [string]$_.id })
$realtimeDimensionIds = @($realtimeMatrix.required_dimensions | ForEach-Object { [string]$_ })
if ($realtimeMatrix.schema_version -ne 1) { Add-Issue "error" "realtime-matrix.schema-unsupported" "Unsupported realtime performance matrix schema: $($realtimeMatrix.schema_version)" }
if ($realtimeMatrix.execution_policy -ne "sealed-required") { Add-Issue "error" "realtime-matrix.policy" "Realtime performance qualification must use sealed-required execution" }
$crossApplicationProducers = @($crossApplicationProfile.required_producers | ForEach-Object { [string]$_ })
if ($crossApplicationProfile.schema_version -ne 1 -or $crossApplicationStimulus.schema_version -ne 1) {
    Add-Issue "error" "cross-application.schema-unsupported" "Cross-application policy and stimulus must both use schema 1"
}
if ($crossApplicationProfile.execution_policy -ne "sealed-required") {
    Add-Issue "error" "cross-application.policy" "Cross-application qualification must use sealed-required execution"
}
if (@(Compare-Object @("adobe_premiere_pro", "blender", "davinci_resolve", "mondrian") ($crossApplicationProducers | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "cross-application.producers" "Cross-application qualification must require the exact commercial producer set"
}
$platformBackendPairs = @($platformMatrix.required_platform_backends | ForEach-Object { "$($_.platform)/$($_.backend)" })
$platformScenarios = @($platformMatrix.required_scenarios_per_platform | ForEach-Object { [string]$_ })
$platformEvidence = @($platformMatrix.required_evidence_per_cell | ForEach-Object { [string]$_ })
$platformEvidenceOwners = @($platformMatrix.required_evidence_owners | ForEach-Object {
    "$($_.kind)/$($_.owner)/$($_.verifier_id)/$($_.report_schema_version)"
})
if ($platformMatrix.schema_version -ne 1 -or $platformMatrix.execution_policy -ne "sealed-required") {
    Add-Issue "error" "platform-matrix.schema-policy" "Platform/driver/display matrix must use schema 1 and sealed-required execution"
}
if ([string]$platformMatrix.runner.verifier_tool_id -ne "platform_qualification_replay" -or
    [int]$platformMatrix.runner.timeout_seconds -le 0 -or
    [int]$platformMatrix.runner.report_schema_version -ne 1) {
    Add-Issue "error" "platform-matrix.runner" "Platform matrix must bind the approved standalone replay tool and bounded schema-1 report"
}
if ($platformMatrix.build_manifest_schema_version -ne 1 -or
    [string]$platformMatrix.owner_verifier_script -ne "scripts/validation/verify-platform-driver-display-lane.ps1" -or
    [string]$platformMatrix.source_verifier_script -ne "scripts/validation/verify-platform-driver-display-source.ps1" -or
    [string]$platformMatrix.bundle_verifier_script -ne "scripts/validation/verify-platform-driver-display-matrix.ps1") {
    Add-Issue "error" "platform-matrix.verifiers" "Platform matrix must bind the exact build-manifest schema and checked-in owner/bundle verifiers"
}
if (@(Compare-Object @("linux/vulkan", "mac_os/metal", "windows/dx12") ($platformBackendPairs | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "platform-matrix.backends" "Platform matrix must require exact Windows/DX12, macOS/Metal, and Linux/Vulkan coverage"
}
if (@(Compare-Object @("display_p3", "hdr_pq", "managed_icc", "sdr_srgb") ($platformScenarios | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "platform-matrix.scenarios" "Every platform must cover the exact SDR, P3, PQ, and ICC scenario union"
}
if (@(Compare-Object @("gpu_color", "platform_probe", "viewer_display") ($platformEvidence | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "platform-matrix.evidence" "Every platform matrix cell must require exact platform, GPU, and Viewer evidence"
}
if (@(Compare-Object @(
    "gpu_color/mondrian-renderer/gpu-color-qualification-v1/1",
    "platform_probe/mondrian-platform/platform-display-probe-v1/1",
    "viewer_display/mondrian-app/viewer-display-qualification-v1/1"
) ($platformEvidenceOwners | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "platform-matrix.evidence-owners" "Platform matrix evidence must retain exact owner Module and report schema contracts"
}
foreach ($field in @(
    "cross_machine_row_aggregation_required",
    "atomic_row_execution_required",
    "row_execution_is_serial",
    "clean_source_required",
    "exact_target_artifact_required",
    "executed_runtime_image_required",
    "common_release_candidate_required",
    "common_build_manifest_required",
    "exact_environment_before_after_required",
    "owner_verified_leaf_receipts_required",
    "source_evidence_required",
    "environment_snapshots_required",
    "capture_authority_manifest_required",
    "separately_approved_verifier_tools_required",
    "runtime_cargo_replay_forbidden",
    "hosted_ci_is_not_physical_evidence",
    "capability_skip_forbidden"
)) {
    if (-not (Has-Property $platformMatrix $field) -or $platformMatrix.$field -ne $true) {
        Add-Issue "error" "platform-matrix.invariant" "Platform matrix must require '$field'"
    }
}
foreach ($field in @(
    "ready_health_required",
    "valid_display_contract_required",
    "external_texture_presentation_required",
    "zero_readback_stages_required",
    "carrier_reuse_required_for_wide_color_and_hdr",
    "operator_observation_required",
    "capability_skip_forbidden",
    "full_output_contract_sha256_required",
    "exact_environment_sha256_required",
    "executed_runtime_image_sha256_required"
)) {
    if (-not (Has-Property $platformMatrix.viewer_scenario_acceptance $field) -or
        $platformMatrix.viewer_scenario_acceptance.$field -ne $true) {
        Add-Issue "error" "platform-matrix.viewer-acceptance" "Platform matrix Viewer acceptance must require '$field'"
    }
}
$platformOwnerGateIds = @($platformMatrix.owner_verifier_contracts.gpu_color_required_gates | ForEach-Object { [string]$_ })
if ($platformMatrix.owner_verifier_contracts.raw_evidence_schema_version -ne 1 -or
    $platformMatrix.owner_verifier_contracts.machine_report_schema_version -ne 1 -or
    $platformMatrix.owner_verifier_contracts.raw_json_must_be_owner_evaluated -ne $true -or
    [string]$platformMatrix.owner_verifier_contracts.qualified_gate_status -ne "qualified" -or
    @(Compare-Object @(
        "aces-pq-delta-e-itp",
        "native-yuv-color-accuracy",
        "output-smoke",
        "standard-all-views-accuracy",
        "standard-input-4k-performance",
        "standard-pq-delta-e-itp",
        "standard-rec709-accuracy",
        "standard-view-4k-performance"
    ) ($platformOwnerGateIds | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "platform-matrix.owner-verifier-contracts" "Platform matrix must retain the exact owner-evaluated raw evidence and GPU gate contract"
}
$platformSourceOwners = @($platformMatrix.source_evidence_contracts.owners | ForEach-Object {
    "$($_.kind)/$($_.source_verifier_id)"
})
if ($platformMatrix.capture_authority_manifest_schema_version -ne 1 -or
    $platformMatrix.source_evidence_contracts.schema_version -ne 1 -or
    $platformMatrix.source_evidence_contracts.environment_snapshot_schema_version -ne 1 -or
    [string]$platformMatrix.source_evidence_contracts.platform_probe_producer_script -ne "scripts/validation/invoke-platform-display-probe-source.ps1" -or
    [string]$platformMatrix.source_evidence_contracts.gpu_color_profile_path -ne "tests/validation/platform-gpu-color-gates.json" -or
    [string]$platformMatrix.source_evidence_contracts.gpu_color_producer_script -ne "scripts/validation/invoke-platform-gpu-color-source.ps1" -or
    @(Compare-Object @(
        "authority-challenge.json", "build.stderr", "build.stdout", "capture-plan.json",
        "producer-binary", "supervisor-summary.json"
    ) @($platformMatrix.source_evidence_contracts.platform_probe_static_roles | Sort-Object)).Count -ne 0 -or
    @(Compare-Object @(
        "authority-challenge.json", "build.stderr", "build.stdout", "profile.json", "supervisor-summary.json"
    ) @($platformMatrix.source_evidence_contracts.gpu_color_static_roles | Sort-Object)).Count -ne 0 -or
    @(Compare-Object @("measurement.json", "report.json", "stderr", "stdout") `
        @($platformMatrix.source_evidence_contracts.gpu_color_gate_roles | Sort-Object)).Count -ne 0 -or
    [string]$platformMatrix.source_evidence_contracts.source_role_pattern -ne '^[a-z0-9][a-z0-9._-]{0,95}$' -or
    @(Compare-Object @(
        "gpu_color/gpu-color-source-replay-v1",
        "platform_probe/platform-display-probe-source-replay-v1",
        "viewer_display/viewer-display-source-replay-v1"
    ) ($platformSourceOwners | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "platform-matrix.source-evidence" "Platform matrix must retain the exact source-replay owner contracts"
}
foreach ($producerPath in @(
    [string]$platformMatrix.source_verifier_script,
    [string]$platformMatrix.source_evidence_contracts.platform_probe_producer_script,
    [string]$platformMatrix.source_evidence_contracts.gpu_color_producer_script
)) {
    try {
        $producerAbsolute = Resolve-ContainedPath $repositoryRoot $producerPath "Platform qualification producer"
        if (-not (Test-Path -LiteralPath $producerAbsolute -PathType Leaf)) {
            throw "Platform qualification producer is missing: $producerPath"
        }
    }
    catch { Add-Issue "error" "platform-matrix.producer-missing" $_.Exception.Message }
}
if (@($platformMatrix.native_probe_rules.mac_os.managed_icc).Count -eq 0 -or
    @($platformMatrix.native_probe_rules.mac_os.wide_color_hdr_edr).Count -eq 0) {
    Add-Issue "error" "platform-matrix.macos-native-rules" "macOS qualification must separate CoreGraphics ICC from AppKit wide-color/EDR evidence"
}
foreach ($field in @(
    "qualified_status_required",
    "exact_cell_closure_required",
    "unique_row_run_ids_required",
    "same_source_revision_required",
    "same_release_candidate_required",
    "build_manifest_hash_required",
    "per_cell_product_artifact_required",
    "profile_hash_required",
    "machine_report_hash_required",
    "raw_evidence_hash_required",
    "environment_drift_forbidden",
    "software_adapter_forbidden",
    "missing_is_incomplete_not_pass",
    "report_self_verification_required"
)) {
    if (-not (Has-Property $platformMatrix.acceptance $field) -or $platformMatrix.acceptance.$field -ne $true) {
        Add-Issue "error" "platform-matrix.acceptance" "Platform matrix acceptance must require '$field'"
    }
}
$declaredStimulusPath = Resolve-ContainedPath $repositoryRoot ([string]$crossApplicationProfile.stimulus.path) "Cross-application stimulus"
$declaredStimulusHash = (Get-FileHash -LiteralPath $declaredStimulusPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($declaredStimulusPath -ne [IO.Path]::GetFullPath($crossApplicationStimulusPath) -or
    $declaredStimulusHash -ne [string]$crossApplicationProfile.stimulus.sha256) {
    Add-Issue "error" "cross-application.stimulus" "Cross-application policy does not bind the exact checked-in stimulus bytes"
}
foreach ($field in @("qualified_status_required", "missing_artifacts_forbidden", "skipped_execution_forbidden", "profile_hash_required", "report_hash_required", "source_sha_required", "machine_report_hash_required")) {
    if (-not (Has-Property $crossApplicationProfile.acceptance $field) -or $crossApplicationProfile.acceptance.$field -ne $true) {
        Add-Issue "error" "cross-application.acceptance" "Cross-application qualification must require '$field'"
    }
}
if ($realtimeMatrix.machine_profile -ne $machineProfile.id) { Add-Issue "error" "realtime-matrix.machine-profile-mismatch" "Realtime performance matrix references a different machine profile" }
if (@(Compare-Object @("long-authoring", "playback-reference", "real-4k60-dual-layer", "renderer-visual") ($realtimeGateIds | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "realtime-matrix.gates" "Realtime performance matrix must define the exact four sealed gates"
}
if (@(Compare-Object @("120-minute-authoring", "30-minute-audio-recovery", "30-minute-video-playback", "4k60-hdr-multilayer-multieffect-scopes", "8k30-hdr-multilayer-multieffect-scopes", "real-4k60-main10-dual-layer-decode-publish") ($realtimeDimensionIds | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "realtime-matrix.dimensions" "Realtime performance matrix coverage differs from the commercial contract"
}
if (@(Compare-Object @("display-p3", "hdr-pq", "icc-sdr") ($viewerDisplayScenarioIds | Sort-Object)).Count -ne 0) {
    Add-Issue "error" "viewer-display.scenarios" "Viewer display qualification must require exact P3, HDR-PQ, and ICC scenarios"
}
foreach ($field in @("ready_health_required", "valid_display_snapshot_required", "external_texture_presentation_required", "zero_readback_stages_required", "carrier_reuse_evidence_required", "full_output_contract_sha256_required", "full_output_contract_payload_required", "executed_runtime_image_sha256_required", "operator_visual_observation_required", "capability_skip_forbidden")) {
    if (-not (Has-Property $viewerDisplayProfile.acceptance $field) -or $viewerDisplayProfile.acceptance.$field -ne $true) {
        Add-Issue "error" "viewer-display.acceptance" "Viewer display qualification must require '$field'"
    }
}
$gpuColorGateIds = @($gpuColorProfile.gates | ForEach-Object { [string]$_.id })
if ($gpuColorGateIds.Count -lt 1 -or @($gpuColorGateIds | Sort-Object -Unique).Count -ne $gpuColorGateIds.Count) {
    Add-Issue "error" "gpu-color.gates" "GPU color qualification gates must be non-empty and unique"
}
if (-not (Has-Property $commercialEngine "gpu_color")) {
    Add-Issue "error" "commercial-engine.gpu-color-missing" "Commercial engine contract must define sealed GPU color qualification"
} else {
    if ($commercialEngine.gpu_color.profile_id -ne $gpuColorProfile.id) {
        Add-Issue "error" "commercial-engine.gpu-color-profile-mismatch" "Commercial engine contract must reference the current GPU color profile"
    }
    $commercialGpuGateIds = @($commercialEngine.gpu_color.required_gate_ids | ForEach-Object { [string]$_ })
    if (@(Compare-Object ($gpuColorGateIds | Sort-Object) ($commercialGpuGateIds | Sort-Object)).Count -ne 0) {
        Add-Issue "error" "commercial-engine.gpu-color-gates-mismatch" "Commercial engine GPU color gate set must exactly match the profile"
    }
}
if (-not (Has-Property $commercialEngine "complete_golden")) {
    Add-Issue "error" "commercial-engine.complete-golden-missing" "Commercial engine contract must define Complete Golden qualification"
} else {
    if ($commercialEngine.complete_golden.contract_id -ne $golden.id) {
        Add-Issue "error" "commercial-engine.complete-golden-contract-mismatch" "Commercial engine contract must reference the current Golden Project contract"
    }
    if ($commercialEngine.complete_golden.report_schema_version -ne 3) {
        Add-Issue "error" "commercial-engine.complete-golden-report-schema-unsupported" "Commercial engine contract must require Complete Golden report schema 3"
    }
    foreach ($field in @("build_timeout_seconds", "process_timeout_seconds")) {
        if (-not (Has-Property $commercialEngine.complete_golden $field)) {
            Add-Issue "error" "commercial-engine.complete-golden-timeout-missing" "Complete Golden qualification must independently declare '$field'"
            continue
        }
        $timeoutSeconds = 0
        if (
            -not [int]::TryParse([string]$commercialEngine.complete_golden.$field, [ref]$timeoutSeconds) -or
            $timeoutSeconds -lt 60 -or
            $timeoutSeconds -gt 3600
        ) {
            Add-Issue "error" "commercial-engine.complete-golden-timeout-invalid" "Complete Golden '$field' must be an integer from 60 through 3600 seconds"
        }
    }
}
if ($playbackPlan.machine_profile -ne $machineProfile.id) { Add-Issue "error" "playback-plan.machine-profile-mismatch" "Playback plan references '$($playbackPlan.machine_profile)' but the configured profile is '$($machineProfile.id)'" }
$diagnosticExecution = if (Has-Property $playbackPlan "diagnostic_execution") { $playbackPlan.diagnostic_execution } else { $null }
$safeDiagnosticMachineIssueCodes = @("machine.logical-cpu", "machine.memory-class")
if ($null -eq $diagnosticExecution) {
    Add-Issue "error" "playback-plan.diagnostic-execution-missing" "Playback plan must define its diagnostic machine-qualification policy"
} else {
    if (-not (Has-Property $diagnosticExecution "explicit_unqualified_machine_opt_in_required") -or $diagnosticExecution.explicit_unqualified_machine_opt_in_required -ne $true) {
        Add-Issue "error" "playback-plan.diagnostic-opt-in-required" "Unqualified-machine diagnostics must require explicit operator opt-in"
    }
    if (-not (Has-Property $diagnosticExecution "waivable_machine_issue_codes")) {
        Add-Issue "error" "playback-plan.diagnostic-waivers-missing" "Playback plan must explicitly list diagnostic-waivable machine issues"
    } else {
        $diagnosticWaivers = @($diagnosticExecution.waivable_machine_issue_codes | ForEach-Object { [string]$_ })
        foreach ($code in $diagnosticWaivers) {
            if ([string]::IsNullOrWhiteSpace($code)) {
                Add-Issue "error" "playback-plan.diagnostic-waiver-empty" "Diagnostic machine issue codes must be non-empty"
            } elseif ($code -notin $safeDiagnosticMachineIssueCodes) {
                Add-Issue "error" "playback-plan.diagnostic-waiver-unsafe" "Machine issue '$code' is an execution prerequisite and cannot be waived for diagnostics"
            }
        }
        if (@($diagnosticWaivers | Select-Object -Unique).Count -ne $diagnosticWaivers.Count) {
            Add-Issue "error" "playback-plan.diagnostic-waiver-duplicate" "Diagnostic machine issue codes must be unique"
        }
    }
}
$supportedMemoryClasses = @("minimum-supported", "standard-playback", "professional-large-project")
if (-not (Has-Property $playbackPlan "baseline_machine_requirements") -or -not (Has-Property $playbackPlan.baseline_machine_requirements "memory_class")) {
    Add-Issue "error" "playback-plan.baseline-memory-class-missing" "Playback plan must declare the memory class required for baseline evidence"
} elseif ([string]$playbackPlan.baseline_machine_requirements.memory_class -notin $supportedMemoryClasses) {
    Add-Issue "error" "playback-plan.baseline-memory-class-unknown" "Playback plan declares an unknown baseline memory class"
}
$memoryClassContract = if (Has-Property $machineProfile.hardware "memory_classes_gib") { $machineProfile.hardware.memory_classes_gib } else { $null }
if ($null -eq $memoryClassContract) {
    Add-Issue "error" "machine-profile.memory-classes-missing" "Windows machine profile must define memory support classes"
} else {
    $minimumMemory = [int]$memoryClassContract.'minimum-supported'
    $standardMemory = [int]$memoryClassContract.'standard-playback'
    $professionalMemory = [int]$memoryClassContract.'professional-large-project'
    if ($minimumMemory -ne 8 -or $minimumMemory -ge $standardMemory -or $standardMemory -ge $professionalMemory) {
        Add-Issue "error" "machine-profile.memory-classes-invalid" "Memory classes must start at the 8 GiB support floor and increase from minimum to standard to professional"
    }
}
$playbackFixtureIds = @($playbackPlan.gates | ForEach-Object { [string]$_.fixture_id })
foreach ($gate in $playbackPlan.gates) {
    if ([string]$gate.id -eq "video" -and (-not (Has-Property $gate "packaged_demux_worker_required") -or $gate.packaged_demux_worker_required -ne $true)) {
        Add-Issue "error" "playback-plan.video-demux-worker-missing" "The production Video gate must build, hash, and execute the packaged demux worker"
    }
}

$ids = @{}
$requiredFields = @("id", "path", "availability", "sha256", "size_bytes", "provenance", "media", "purposes")
foreach ($entry in $manifest.entries) {
    $entryId = if (Has-Property $entry "id") { [string]$entry.id } else { "<unknown>" }
    $missingEntryFields = $false
    foreach ($field in $requiredFields) {
        if (-not (Has-Property $entry $field)) {
            Add-Issue "error" "entry.field-missing" "${entryId}: missing $field"
            $missingEntryFields = $true
        }
    }
    if ($missingEntryFields) { continue }
    if ($ids.ContainsKey($entryId)) { Add-Issue "error" "entry.duplicate-id" "Duplicate fixture id: $entryId" } else { $ids[$entryId] = $entry }
    if ($entry.availability -notin @("committed", "local-restricted", "generated")) { Add-Issue "error" "entry.bad-availability" "${entryId}: invalid availability" }
    if (-not (Has-Property $entry.provenance "source") -or [string]::IsNullOrWhiteSpace([string]$entry.provenance.source)) { Add-Issue "error" "entry.provenance-source-missing" "${entryId}: provenance source is required" }
    if (-not (Has-Property $entry.provenance "rights_basis") -or [string]::IsNullOrWhiteSpace([string]$entry.provenance.rights_basis)) { Add-Issue "error" "entry.rights-basis-missing" "${entryId}: an explicit rights basis is required" }
    if (-not (Has-Property $entry.provenance "redistribution") -or $entry.provenance.redistribution -notin @("permitted", "prohibited")) { Add-Issue "error" "entry.unverified-provenance" "${entryId}: usage and redistribution rights are not explicit" }
    if ((Has-Property $entry.media "color_reference_eligible") -and $entry.media.color_reference_eligible -eq $false -and @($entry.purposes | Where-Object { $_ -match "color" }).Count -gt 0) { Add-Issue "error" "entry.ineligible-color-purpose" "${entryId}: a color-ineligible fixture declares a color purpose" }

    $artifactPath = $null
    try {
        $artifactPath = Resolve-ContainedPath $fixtureRootAbsolute ([string]$entry.path) "Fixture path"
    } catch {
        Add-Issue "error" "entry.path-invalid" "${entryId}: $($_.Exception.Message)"
    }

    $fixedIdentity = $entry.availability -in @("committed", "local-restricted")
    $generationContractUsable = $false
    if ($fixedIdentity) {
        if ($entry.sha256 -notmatch '^[0-9a-f]{64}$') { Add-Issue "error" "entry.bad-hash" "${entryId}: fixed artifacts require lowercase SHA-256" }
        if ($null -eq $entry.size_bytes -or [int64]$entry.size_bytes -lt 0) { Add-Issue "error" "entry.bad-size" "${entryId}: fixed artifacts require a non-negative byte size" }
        if (Has-Property $entry "generation") { Add-Issue "error" "entry.unexpected-generation" "${entryId}: fixed artifacts cannot declare a generation recipe" }
    } elseif ($entry.availability -eq "generated") {
        if ($null -ne $entry.sha256 -or $null -ne $entry.size_bytes) { Add-Issue "error" "entry.generated-global-identity" "${entryId}: generated output hash and size belong to each run, not the corpus manifest" }
        if (-not (Has-Property $entry "generation")) {
            Add-Issue "error" "entry.generation-missing" "${entryId}: generated fixture has no recipe contract"
        } else {
            $generation = $entry.generation
            $missingGenerationFields = @("recipe_path", "recipe_sha256", "profile", "attestation_suffix", "artifact_hash_scope") | Where-Object { -not (Has-Property $generation $_) }
            foreach ($field in $missingGenerationFields) { Add-Issue "error" "entry.generation-field-missing" "${entryId}: generation contract is missing $field" }
            if (@($missingGenerationFields).Count -eq 0) {
                $generationContractUsable = $true
                if ($generation.recipe_sha256 -notmatch '^[0-9a-f]{64}$') { Add-Issue "error" "entry.recipe-bad-hash" "${entryId}: recipe SHA-256 must be lowercase hexadecimal" }
                if ($generation.artifact_hash_scope -ne "reference-run") { Add-Issue "error" "entry.artifact-hash-scope" "${entryId}: generated artifacts must be pinned by each reference run" }
                try {
                    $recipePath = Resolve-ContainedPath $repositoryRoot ([string]$generation.recipe_path) "Recipe path"
                    if (-not (Test-Path -LiteralPath $recipePath -PathType Leaf)) {
                        Add-Issue "error" "entry.recipe-missing" "${entryId}: recipe is absent at $recipePath"
                    } else {
                        $recipeHash = (Get-FileHash -LiteralPath $recipePath -Algorithm SHA256).Hash.ToLowerInvariant()
                        if ($recipeHash -ne $generation.recipe_sha256) { Add-Issue "error" "entry.recipe-hash-mismatch" "${entryId}: recipe SHA-256 differs from the manifest" }
                    }
                } catch {
                    Add-Issue "error" "entry.recipe-path-invalid" "${entryId}: $($_.Exception.Message)"
                }
            }
        }
    }

    $mustExist = $entry.availability -eq "committed" -or ($Tier -in @("Nightly", "Release") -and ($Scope -eq "All" -or $entryId -in $playbackFixtureIds))
    $artifactResult = [ordered]@{
        fixture_id = $entryId
        availability = $entry.availability
        path = if ($null -eq $artifactPath) { $null } else { $artifactPath }
        present = $false
        size_bytes = $null
        sha256 = $null
        attested = $false
    }
    if ($null -ne $artifactPath -and (Test-Path -LiteralPath $artifactPath -PathType Leaf)) {
        $item = Get-Item -LiteralPath $artifactPath
        $actualHash = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $artifactResult.present = $true
        $artifactResult.size_bytes = [int64]$item.Length
        $artifactResult.sha256 = $actualHash
        if ($fixedIdentity) {
            if ($item.Length -ne [int64]$entry.size_bytes) { Add-Issue "error" "asset.size-mismatch" "${entryId}: expected $($entry.size_bytes) bytes, got $($item.Length)" }
            if ($actualHash -ne $entry.sha256) { Add-Issue "error" "asset.hash-mismatch" "${entryId}: SHA-256 differs from the manifest" }
        } elseif ($generationContractUsable) {
            $attestationPath = "$artifactPath$($entry.generation.attestation_suffix)"
            if (-not (Test-Path -LiteralPath $attestationPath -PathType Leaf)) {
                Add-Issue "error" "asset.attestation-missing" "${entryId}: generated artifact has no attestation at $attestationPath"
            } else {
                try {
                    $attestation = Get-Content -LiteralPath $attestationPath -Raw | ConvertFrom-Json
                    $attestationValid = $attestation.schema_version -eq 1 -and $attestation.fixture_id -eq $entryId -and $attestation.recipe.sha256 -eq $entry.generation.recipe_sha256 -and $attestation.artifact.sha256 -eq $actualHash -and [int64]$attestation.artifact.size_bytes -eq [int64]$item.Length
                    if (-not $attestationValid) { Add-Issue "error" "asset.attestation-mismatch" "${entryId}: generated attestation does not bind this recipe and artifact" } else { $artifactResult.attested = $true }
                } catch {
                    Add-Issue "error" "asset.attestation-invalid" "${entryId}: cannot read generated attestation: $($_.Exception.Message)"
                }
            }
        }
    } elseif ($mustExist) {
        Add-Issue "blocked" "asset.missing" "${entryId}: required asset is absent at $artifactPath"
    }
    $assetResults.Add([pscustomobject]$artifactResult)
}

if ($golden.schema_version -ne 4) {
    Add-Issue "error" "golden.schema-unsupported" "$($golden.id) has an unsupported Golden Project schema version"
}
if ($golden.kind -ne "golden") {
    Add-Issue "error" "golden.bad-kind" "$($golden.id) is not a Golden Project contract"
}
$hasGoldenHeroSequence = Has-Property $golden "hero_sequence"
if (-not $hasGoldenHeroSequence) {
    Add-Issue "error" "golden.hero-sequence-invalid" "Golden Project must declare one Hero Sequence contract"
} elseif (
    [string]::IsNullOrWhiteSpace([string]$golden.hero_sequence.role) -or
    [int64]$golden.hero_sequence.duration_frames -ne [int64]$golden.timeline.duration_frames -or
    $golden.hero_sequence.requires_all_obligations -ne $true
) {
    Add-Issue "error" "golden.hero-sequence-invalid" "Golden Project must bind every obligation to one full-duration Hero Sequence role"
}
if ($stress.schema_version -ne 1) {
    Add-Issue "error" "stress.schema-unsupported" "$($stress.id) has an unsupported Stress Project schema version"
}
if ($stress.kind -ne "stress") {
    Add-Issue "error" "stress.bad-kind" "$($stress.id) is not a Stress Project contract"
}

$goldenRoleNames = @($golden.required_fixture_roles | ForEach-Object { [string]$_.role })
if (@($goldenRoleNames | Where-Object { [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) {
    Add-Issue "error" "golden.fixture-role-empty" "Golden Project fixture roles must have non-empty stable names"
}
if (@($goldenRoleNames | Select-Object -Unique).Count -ne $goldenRoleNames.Count) {
    Add-Issue "error" "golden.fixture-role-duplicate" "Golden Project fixture role names must be unique"
}
foreach ($role in $golden.required_fixture_roles) {
    $roleName = [string]$role.role
    if (-not (Has-Property $role "required_purpose") -or [string]::IsNullOrWhiteSpace([string]$role.required_purpose)) {
        Add-Issue "error" "golden.fixture-purpose-missing" "Golden Project role '$roleName' must declare one required corpus purpose"
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$role.fixture_id)) {
        if (-not $ids.ContainsKey([string]$role.fixture_id)) {
            Add-Issue "error" "golden.fixture-unknown" "Golden Project role '$roleName' references unknown fixture '$($role.fixture_id)'"
        } elseif ((Has-Property $role "required_purpose") -and [string]$role.required_purpose -notin @($ids[[string]$role.fixture_id].purposes)) {
            Add-Issue "error" "golden.fixture-purpose-mismatch" "Golden Project role '$roleName' requires purpose '$($role.required_purpose)', but fixture '$($role.fixture_id)' does not declare it"
        }
    }
}

$requiredOperationIds = @($golden.required_operations | ForEach-Object { [string]$_ })
$requiredContentIds = @($golden.required_content | ForEach-Object { [string]$_ })
$exportIds = @($golden.exports | ForEach-Object { [string]$_.id })
foreach ($requirementSet in @(
    [pscustomobject]@{ Name = "operation"; Values = $requiredOperationIds },
    [pscustomobject]@{ Name = "content"; Values = $requiredContentIds }
)) {
    if (@($requirementSet.Values | Where-Object { [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) {
        Add-Issue "error" "golden.requirement-empty" "Golden Project $($requirementSet.Name) requirements must be non-empty"
    }
    if (@($requirementSet.Values | Select-Object -Unique).Count -ne $requirementSet.Values.Count) {
        Add-Issue "error" "golden.requirement-duplicate" "Golden Project $($requirementSet.Name) requirements must be unique"
    }
}

if (-not (Has-Property $golden "execution_slices") -or @($golden.execution_slices).Count -eq 0) {
    Add-Issue "error" "golden.execution-slices-missing" "Golden Project must define at least one independently executable evidence slice"
} else {
    $sliceIds = @($golden.execution_slices | ForEach-Object { [string]$_.id })
    if (@($sliceIds | Where-Object { [string]::IsNullOrWhiteSpace($_) }).Count -gt 0 -or @($sliceIds | Select-Object -Unique).Count -ne $sliceIds.Count) {
        Add-Issue "error" "golden.execution-slice-id-invalid" "Golden Project execution-slice ids must be non-empty and unique"
    }
    foreach ($slice in $golden.execution_slices) {
        $sliceId = [string]$slice.id
        foreach ($field in @("sequence_role", "required_fixture_roles", "required_operations", "required_content", "required_exports")) {
            if (-not (Has-Property $slice $field)) {
                Add-Issue "error" "golden.execution-slice-field-missing" "Golden Project slice '$sliceId' is missing '$field'"
            }
        }
        if (
            -not (Has-Property $slice "sequence_role") -or
            [string]::IsNullOrWhiteSpace([string]$slice.sequence_role)
        ) {
            Add-Issue "error" "golden.execution-slice-sequence-role-invalid" "Golden Project slice '$sliceId' must declare a non-empty Sequence role"
        }
        $sliceRoleNames = if (Has-Property $slice "required_fixture_roles") {
            @($slice.required_fixture_roles | ForEach-Object { [string]$_ })
        } else {
            @()
        }
        $sliceOperationIds = if (Has-Property $slice "required_operations") {
            @($slice.required_operations | ForEach-Object { [string]$_ })
        } else {
            @()
        }
        $sliceContentIds = if (Has-Property $slice "required_content") {
            @($slice.required_content | ForEach-Object { [string]$_ })
        } else {
            @()
        }
        $sliceExportIds = if (Has-Property $slice "required_exports") {
            @($slice.required_exports | ForEach-Object { [string]$_ })
        } else {
            @()
        }
        foreach ($roleName in $sliceRoleNames) {
            if ($roleName -notin $goldenRoleNames) {
                Add-Issue "error" "golden.execution-slice-role-unknown" "Golden Project slice '$sliceId' references unknown fixture role '$roleName'"
            }
        }
        foreach ($operationId in $sliceOperationIds) {
            if ($operationId -notin $requiredOperationIds) {
                Add-Issue "error" "golden.execution-slice-operation-unknown" "Golden Project slice '$sliceId' references unknown operation '$operationId'"
            }
        }
        foreach ($contentId in $sliceContentIds) {
            if ($contentId -notin $requiredContentIds) {
                Add-Issue "error" "golden.execution-slice-content-unknown" "Golden Project slice '$sliceId' references unknown content '$contentId'"
            }
        }
        foreach ($exportId in $sliceExportIds) {
            if ($exportId -notin $exportIds) {
                Add-Issue "error" "golden.execution-slice-export-unknown" "Golden Project slice '$sliceId' references unknown export '$exportId'"
            }
        }
    }
    if ($hasGoldenHeroSequence) {
        $heroSliceCount = @(
            $golden.execution_slices |
                Where-Object { [string]$_.sequence_role -eq [string]$golden.hero_sequence.role }
        ).Count
        if ($heroSliceCount -eq 0) {
            Add-Issue "error" "golden.hero-sequence-unassigned" "Golden Project has no slice assigned to its Hero Sequence role"
        }
    }
}

if (-not (Has-Property $golden.acceptance "unexecuted_requirement_may_pass") -or $golden.acceptance.unexecuted_requirement_may_pass -ne $false) {
    Add-Issue "error" "golden.unexecuted-requirement-policy-invalid" "Golden Project acceptance must explicitly forbid unexecuted requirements from passing"
}
if (-not (Has-Property $golden.acceptance "consecutive_passes") -or [int]$golden.acceptance.consecutive_passes -ne 3) {
    Add-Issue "error" "golden.consecutive-pass-policy-invalid" "Golden Project acceptance must require exactly three consecutive passes"
}
if (-not (Has-Property $golden.acceptance "duration_error_max_frames") -or [int]$golden.acceptance.duration_error_max_frames -ne 1) {
    Add-Issue "error" "golden.duration-tolerance-invalid" "Golden Project duration tolerance must be exactly one frame"
}
if (-not (Has-Property $golden.acceptance "av_boundary_error_max_ms") -or [int]$golden.acceptance.av_boundary_error_max_ms -ne 20) {
    Add-Issue "error" "golden.av-boundary-tolerance-invalid" "Golden Project A/V boundary tolerance must be exactly 20 ms"
}
if (-not (Has-Property $golden.acceptance "silent_fallback_allowed") -or $golden.acceptance.silent_fallback_allowed -ne $false) {
    Add-Issue "error" "golden.silent-fallback-policy-invalid" "Golden Project acceptance must explicitly forbid silent fallback"
}

if (@($exportIds | Select-Object -Unique).Count -ne $exportIds.Count) {
    Add-Issue "error" "golden.export-id-duplicate" "Golden Project export ids must be unique"
}
foreach ($export in $golden.exports) {
    foreach ($field in @("id", "builtin_preset_id", "expected_delivery", "required_probe_fields")) {
        if (-not (Has-Property $export $field)) {
            Add-Issue "error" "golden.export-field-missing" "Golden Project export '$($export.id)' is missing '$field'"
        }
    }
    if (Has-Property $export "expected_delivery") {
        foreach ($field in @("container", "video_codec", "video_profile", "width", "height", "bit_depth", "chroma_sampling", "pixel_format", "range", "color_primaries", "color_transfer", "color_matrix", "static_hdr_metadata", "alpha", "audio_codec", "audio_bitrate_kbps")) {
            if (-not (Has-Property $export.expected_delivery $field)) {
                Add-Issue "error" "golden.export-delivery-field-missing" "Golden Project export '$($export.id)' expected delivery is missing '$field'"
            }
        }
    }
    if (Has-Property $export "required_probe_fields") {
        foreach ($field in @("bit_depth", "primaries", "transfer", "matrix", "range", "hdr_static_metadata")) {
            if ($field -notin @($export.required_probe_fields)) {
                Add-Issue "error" "golden.export-probe-field-missing" "Golden Project export '$($export.id)' probe contract is missing '$field'"
            }
        }
    }
}

$gateIds = @{}
foreach ($gate in $playbackPlan.gates) {
    $missingGateFields = @("id", "fixture_id", "required_purposes", "cargo_test", "media_environment", "build_timeout_seconds", "process_timeout_seconds", "expected_report_profile", "expected_report_path") | Where-Object { -not (Has-Property $gate $_) }
    foreach ($field in $missingGateFields) { Add-Issue "error" "playback-plan.gate-field-missing" "Playback gate is missing '$field'" }
    if (@($missingGateFields).Count -gt 0) { continue }
    if ($gateIds.ContainsKey($gate.id)) { Add-Issue "error" "playback-plan.duplicate-gate" "Duplicate playback gate id: $($gate.id)" } else { $gateIds[$gate.id] = $true }
    if (-not $ids.ContainsKey($gate.fixture_id)) {
        Add-Issue "error" "playback-plan.fixture-unknown" "Gate '$($gate.id)' references unknown fixture '$($gate.fixture_id)'"
        continue
    }
    $fixture = $ids[$gate.fixture_id]
    foreach ($field in @("cargo_test", "media_environment", "expected_report_profile", "expected_report_path")) {
        if (-not (Has-Property $gate $field) -or [string]::IsNullOrWhiteSpace([string]$gate.$field)) { Add-Issue "error" "playback-plan.gate-field-missing" "Gate '$($gate.id)' requires non-empty '$field'" }
    }
    $buildTimeoutSeconds = 0
    if (-not [int]::TryParse([string]$gate.build_timeout_seconds, [ref]$buildTimeoutSeconds) -or $buildTimeoutSeconds -le 0) {
        Add-Issue "error" "playback-plan.gate-build-timeout-invalid" "Gate '$($gate.id)' requires a positive build_timeout_seconds"
    }
    $processTimeoutSeconds = 0
    if (-not [int]::TryParse([string]$gate.process_timeout_seconds, [ref]$processTimeoutSeconds) -or $processTimeoutSeconds -le 0) {
        Add-Issue "error" "playback-plan.gate-timeout-invalid" "Gate '$($gate.id)' requires a positive process_timeout_seconds"
    }
    $decodeProgressRequired = (Has-Property $gate "decode_progress_required") -and $gate.decode_progress_required -eq $true
    $hasDecodeProgressEnvironment = (Has-Property $gate "decode_progress_environment") -and -not [string]::IsNullOrWhiteSpace([string]$gate.decode_progress_environment)
    if ($decodeProgressRequired -and -not $hasDecodeProgressEnvironment) {
        Add-Issue "error" "playback-plan.decode-progress-environment-missing" "Gate '$($gate.id)' requires a decode progress journal but declares no environment binding"
    }
    foreach ($purpose in $gate.required_purposes) {
        if ($purpose -notin @($fixture.purposes)) { Add-Issue "error" "playback-plan.purpose-missing" "Gate '$($gate.id)' requires purpose '$purpose' on fixture '$($fixture.id)'" }
    }
}
$baselineEvidence = if (Has-Property $playbackPlan "baseline_acceptance") { $playbackPlan.baseline_acceptance } else { $null }
if ($null -eq $baselineEvidence -or -not (Has-Property $baselineEvidence "bounded_gate_build_required") -or $baselineEvidence.bounded_gate_build_required -ne $true) {
    Add-Issue "error" "playback-plan.bounded-gate-build-requirement-missing" "Baseline evidence must require a separately bounded gate test build"
}
if ($null -eq $baselineEvidence -or -not (Has-Property $baselineEvidence "external_process_timeout_required") -or $baselineEvidence.external_process_timeout_required -ne $true) {
    Add-Issue "error" "playback-plan.external-timeout-requirement-missing" "Baseline evidence must require the external gate process timeout"
}
if ($null -eq $baselineEvidence -or -not (Has-Property $baselineEvidence "video_decode_progress_journal_required") -or $baselineEvidence.video_decode_progress_journal_required -ne $true) {
    Add-Issue "error" "playback-plan.decode-progress-requirement-missing" "Baseline evidence must require the Video decode progress journal"
}
$videoGate = @($playbackPlan.gates | Where-Object { $_.id -eq "video" }) | Select-Object -First 1
if ($null -ne $baselineEvidence -and $baselineEvidence.video_decode_progress_journal_required -eq $true) {
    if ($null -eq $videoGate -or -not (Has-Property $videoGate "decode_progress_required") -or $videoGate.decode_progress_required -ne $true -or -not (Has-Property $videoGate "decode_progress_environment") -or [string]$videoGate.decode_progress_environment -ne "MONDRIAN_PREVIEW_DECODE_EXECUTION_OUTPUT") {
        Add-Issue "error" "playback-plan.video-decode-progress-binding-invalid" "The Video baseline gate must require the production decode progress journal environment binding"
    }
}
foreach ($gateId in $playbackPlan.baseline_acceptance.required_gate_ids) {
    if (-not $gateIds.ContainsKey($gateId)) { Add-Issue "error" "playback-plan.required-gate-missing" "Baseline requires unknown gate '$gateId'" }
}

if ($Tier -in @("Nightly", "Release") -and $Scope -eq "All") {
    foreach ($role in $golden.required_fixture_roles) {
        if ($role.required -and [string]::IsNullOrWhiteSpace($role.fixture_id)) {
            Add-Issue "blocked" "golden.fixture-unassigned" "Golden Project role '$($role.role)' has no verified fixture assigned"
        }
    }
    foreach ($purpose in $stress.required_fixture_purposes) {
        $matchingFixtures = @($manifest.entries | Where-Object { $purpose -in @($_.purposes) })
        if ($matchingFixtures.Count -eq 0) { Add-Issue "blocked" "stress.fixture-purpose-unassigned" "Stress Project purpose '$purpose' has no verified fixture" }
    }
}

$errors = @($issues | Where-Object severity -eq "error")
$blocked = @($issues | Where-Object severity -eq "blocked")
$status = if ($errors.Count -gt 0) { "failed" } elseif ($blocked.Count -gt 0) { "blocked" } else { "passed" }
$report = [ordered]@{
    schema_version = 2
    tier = $Tier
    scope = $Scope
    status = $status
    corpus_revision = $manifest.corpus_revision
    checked_at_utc = [DateTime]::UtcNow.ToString("o")
    entry_count = @($manifest.entries).Count
    assets = @($assetResults)
    issues = @($issues)
}

$absoluteOutputPath = if ([IO.Path]::IsPathRooted($OutputPath)) { [IO.Path]::GetFullPath($OutputPath) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $OutputPath)) }
$parent = Split-Path -Parent $absoluteOutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $absoluteOutputPath -Encoding utf8
Write-Host "Reference asset validation: $status ($Tier/$Scope); report: $absoluteOutputPath"
foreach ($issue in $issues) { Write-Host "[$($issue.severity)] $($issue.code): $($issue.message)" }
if ($status -ne "passed") { exit 1 }
