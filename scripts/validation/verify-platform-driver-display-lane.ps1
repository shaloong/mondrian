param(
    [Parameter(Mandatory = $true)][ValidateSet("platform_probe", "gpu_color", "viewer_display")][string]$ExpectedKind,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ReportPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RawEvidencePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CellObservationPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$BuildManifestPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineReportPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ArtifactManifestPath,
    [string]$EvidenceClosurePath = "",
    [string]$EvidenceBundleDirectory = "",
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$PolicyPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Read-BoundedJson([string]$Path, [long]$MaximumBytes, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $MaximumBytes) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length -or $stream.Length -gt [int]::MaxValue) {
            throw "$Label changed or is too large for bounded JSON verification."
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
        try { $value = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff) | ConvertFrom-Json }
        catch { throw "$Label is not valid sealed JSON: $($_.Exception.Message)" }
        return [pscustomobject]@{
            value = $value
            sha256 = $hash
            length = [long]$bytes.Length
            path = $item.FullName
        }
    } finally {
        $stream.Dispose()
    }
}

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if ($expectedValues.Count -ne @($Expected).Count -or $actualValues.Count -ne @($Actual).Count -or
        @(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label is not an exact unique set."
    }
}

function Assert-DriverMatches([object]$Expected, [object]$Actual, [string]$Label) {
    if ([string]$Actual.kind -ne [string]$Expected.kind) { throw "$Label driver kind mismatch." }
    $fields = switch ([string]$Expected.kind) {
        "explicit" { @("name", "version") }
        "os_bundled" { @("os_build") }
        "linux_stack" { @("kernel_version", "drm_driver", "vulkan_driver", "vulkan_driver_version", "mesa_version") }
        default { throw "$Label has an unknown driver kind." }
    }
    foreach ($field in $fields) {
        if ([string]$Actual.$field -ne [string]$Expected.$field) {
            throw "$Label driver field '$field' mismatch."
        }
    }
}

function Assert-EnvironmentMatches([object]$Expected, [object]$Actual, [string]$Label) {
    foreach ($field in @(
        "platform", "architecture", "os_version", "os_build", "window_system", "compositor",
        "graphics_backend", "adapter_name", "adapter_vendor", "adapter_device_id", "adapter_kind",
        "display_identity", "display_inventory_sha256"
    )) {
        if ([string]$Actual.$field -ne [string]$Expected.$field) {
            throw "$Label environment field '$field' mismatch."
        }
    }
    Assert-DriverMatches $Expected.driver $Actual.driver $Label
}

function Assert-ScenarioMatches([object]$Expected, [object]$Actual, [string]$Label) {
    foreach ($field in @(
        "scenario", "status", "report_sha256", "output_contract_sha256", "environment_sha256",
        "operator_attestation_sha256", "viewer_ready", "display_contract_valid",
        "external_texture_presented", "zero_readback_stages", "carrier_reuse_observed",
        "operator_observation_passed", "capability_skip_observed", "surface_color_space", "transfer",
        "bits_per_color_channel", "wide_color_supported", "wide_color_active", "hdr_supported",
        "hdr_enabled", "active_hdr_transfer", "peak_luminance_nits", "icc_profile_sha256",
        "icc_processor_sha256", "edr_headroom_ppm", "hdr_presentation"
    )) {
        if ([string]$Actual.$field -ne [string]$Expected.$field) {
            throw "$Label Viewer field '$field' mismatch."
        }
    }
}

$maximumBytes = 8388608L
$reportAdmission = Read-BoundedJson $ReportPath $maximumBytes "owner lane report"
$rawAdmission = Read-BoundedJson $RawEvidencePath $maximumBytes "owner raw evidence"
$cellAdmission = Read-BoundedJson $CellObservationPath $maximumBytes "cell observation"
$profileAdmission = Read-BoundedJson $RuntimeProfilePath 1048576 "runtime profile"
$buildManifestAdmission = Read-BoundedJson $BuildManifestPath $maximumBytes "build manifest"
$machineAdmission = Read-BoundedJson $MachineReportPath $maximumBytes "machine report"
$policyAdmission = Read-BoundedJson $PolicyPath 1048576 "matrix policy"
$report = $reportAdmission.value
$raw = $rawAdmission.value
$cell = $cellAdmission.value
$profile = $profileAdmission.value
$buildManifest = $buildManifestAdmission.value
$machine = $machineAdmission.value
$policy = $policyAdmission.value
$rawHash = [string]$rawAdmission.sha256
$reportHash = [string]$reportAdmission.sha256
$machineHash = [string]$machineAdmission.sha256
$buildManifestHash = [string]$buildManifestAdmission.sha256
$matchingReceipt = @($cell.reports | Where-Object { [string]$_.kind -eq [string]$report.kind })
$matchingCell = @($profile.cells | Where-Object { [string]$_.cell_id -eq [string]$cell.cell_id })
if ([string]$report.kind -ne $ExpectedKind -or [string]$raw.kind -ne $ExpectedKind -or
    $matchingReceipt.Count -ne 1 -or $matchingCell.Count -ne 1) {
    throw "Owner lane cannot be matched uniquely to its cell receipt and profile."
}
$receipt = $matchingReceipt[0]
$contract = @($matchingCell[0].required_evidence | Where-Object { [string]$_.kind -eq [string]$report.kind })
if ($contract.Count -ne 1) { throw "Owner lane has no unique verifier contract." }

if ($policy.owner_verifier_contracts.raw_json_must_be_owner_evaluated -ne $true -or
    $raw.schema_version -ne [int]$policy.owner_verifier_contracts.raw_evidence_schema_version -or
    $machine.schema_version -ne [int]$policy.owner_verifier_contracts.machine_report_schema_version -or
    [string]$raw.kind -ne [string]$report.kind -or [string]$raw.owner -ne [string]$report.owner -or
    [string]$raw.verifier_id -ne [string]$report.verifier_id -or
    [string]$raw.source_revision -ne [string]$cell.source_revision -or
    [string]$raw.cell_run_id -ne [string]$cell.cell_run_id -or
    [string]$raw.machine_report_sha256 -ne $machineHash -or
    [string]$raw.product_artifact_sha256 -ne [string]$cell.product_artifact.sha256 -or
    [string]$raw.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    [string]$raw.release_candidate_id -ne [string]$cell.release_candidate_id -or
    [string]$raw.build_manifest_sha256 -ne [string]$cell.build_manifest_sha256 -or
    [string]$raw.build_provenance_sha256 -ne [string]$cell.product_artifact.build_provenance_sha256 -or
    [string]$raw.environment_sha256 -ne [string]$receipt.environment_sha256 -or
    [string]$machine.source_revision -ne [string]$cell.source_revision -or
    [string]$machine.cell_run_id -ne [string]$cell.cell_run_id) {
    throw "Owner raw evidence or machine report is not atomically bound to the row."
}
Assert-EnvironmentMatches $cell.environment $machine.environment "machine report"
Assert-EnvironmentMatches $cell.environment $raw.environment "owner raw evidence"

$expectedOwner = switch ([string]$report.kind) {
    "platform_probe" { "mondrian-platform/platform-display-probe-v1" }
    "gpu_color" { "mondrian-renderer/gpu-color-qualification-v1" }
    "viewer_display" { "mondrian-app/viewer-display-qualification-v1" }
    default { throw "Unknown owner lane kind '$($report.kind)'." }
}
$actualOwner = "$($report.owner)/$($report.verifier_id)"
if ($report.schema_version -ne 1 -or $actualOwner -ne $expectedOwner -or
    [string]$receipt.kind -ne $ExpectedKind -or
    [string]$contract[0].owner -ne [string]$report.owner -or
    [string]$contract[0].verifier_id -ne [string]$report.verifier_id -or
    [int]$contract[0].report_schema_version -ne [int]$report.schema_version -or
    [string]$receipt.owner -ne [string]$report.owner -or
    [string]$receipt.verifier_id -ne [string]$report.verifier_id -or
    [string]$receipt.report_sha256 -ne $reportHash -or
    [string]$receipt.status -ne [string]$report.status -or
    [string]$receipt.profile_sha256 -ne [string]$report.profile_sha256 -or
    [int]$receipt.report_schema_version -ne [int]$report.schema_version -or
    [string]$receipt.raw_evidence_sha256 -ne $rawHash -or
    [string]$report.raw_evidence_sha256 -ne $rawHash -or
    [string]$report.source_revision -ne [string]$cell.source_revision -or
    [string]$receipt.source_revision -ne [string]$report.source_revision -or
    [string]$report.cell_run_id -ne [string]$cell.cell_run_id -or
    [string]$receipt.cell_run_id -ne [string]$report.cell_run_id -or
    [string]$report.machine_report_sha256 -ne $machineHash -or
    [string]$receipt.machine_report_sha256 -ne [string]$report.machine_report_sha256 -or
    [string]$cell.machine_report_sha256 -ne $machineHash -or
    [string]$report.environment_sha256 -ne [string]$receipt.environment_sha256 -or
    [string]$report.product_artifact_sha256 -ne [string]$cell.product_artifact.sha256 -or
    [string]$receipt.product_artifact_sha256 -ne [string]$report.product_artifact_sha256 -or
    [string]$report.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    [string]$receipt.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    [string]$report.release_candidate_id -ne [string]$buildManifest.release_candidate_id -or
    [string]$receipt.release_candidate_id -ne [string]$report.release_candidate_id -or
    [string]$report.build_manifest_sha256 -ne $buildManifestHash -or
    [string]$receipt.build_manifest_sha256 -ne [string]$report.build_manifest_sha256 -or
    [string]$report.build_provenance_sha256 -ne [string]$cell.product_artifact.build_provenance_sha256 -or
    [string]$receipt.build_provenance_sha256 -ne [string]$report.build_provenance_sha256 -or
    [string]$receipt.environment_sha256 -ne [string]$report.environment_sha256 -or
    [string]$report.status -notin @("qualified", "failed")) {
    throw "Owner lane common envelope is not atomically bound to its row."
}

$sourceVerifierVariable = Get-Variable -Scope Global -Name "MondrianQualificationSourceVerifierScriptBlock" -ErrorAction SilentlyContinue
$sourceVerifier = if ($null -ne $sourceVerifierVariable) {
    $global:MondrianQualificationSourceVerifierScriptBlock
} else { Join-Path $PSScriptRoot "verify-platform-driver-display-source.ps1" }
& $sourceVerifier `
    -ExpectedKind $ExpectedKind -ReportPath $ReportPath -RawEvidencePath $RawEvidencePath `
    -CellObservationPath $CellObservationPath -RuntimeProfilePath $RuntimeProfilePath `
    -MachineReportPath $MachineReportPath -ArtifactManifestPath $ArtifactManifestPath `
    -EvidenceClosurePath $EvidenceClosurePath -EvidenceBundleDirectory $EvidenceBundleDirectory `
    -PolicyPath $PolicyPath

if ([string]$report.status -eq "failed") {
    if ([string]::IsNullOrWhiteSpace([string]$report.payload.failure_reason)) {
        throw "Failed owner lane must carry a stable failure reason."
    }
    return
}

switch ([string]$report.kind) {
    "platform_probe" {
        if ($report.payload.native_probe_succeeded -ne $true -or
            [string]$report.payload.display_identity -ne [string]$cell.environment.display_identity -or
            [string]$report.payload.display_inventory_sha256 -ne [string]$cell.environment.display_inventory_sha256) {
            throw "Platform probe verifier did not observe a successful native probe."
        }
        Assert-ExactStringSet @($cell.probe_backends) @($report.payload.probe_backends) `
            "platform probe backend closure"
        Assert-ExactStringSet @($cell.probe_backends) @($raw.probe_results | ForEach-Object { [string]$_.backend }) `
            "native platform probe result closure"
        foreach ($probe in @($raw.probe_results)) {
            if ([string]$probe.status -ne "qualified" -or
                [string]$probe.display_identity -ne [string]$cell.environment.display_identity -or
                [string]$probe.environment_sha256 -ne [string]$receipt.environment_sha256) {
                throw "Native platform probe '$($probe.backend)' did not qualify this display environment."
            }
        }
    }
    "gpu_color" {
        if ($report.payload.hardware_adapter -ne $true -or
            [string]$report.payload.graphics_backend -ne [string]$cell.environment.graphics_backend -or
            [string]$report.payload.adapter_name -ne [string]$cell.environment.adapter_name -or
            [string]$report.payload.adapter_vendor -ne [string]$cell.environment.adapter_vendor -or
            [string]$report.payload.adapter_device_id -ne [string]$cell.environment.adapter_device_id -or
            [string]$report.payload.driver_kind -ne [string]$cell.environment.driver.kind -or
            [int]$report.payload.executed_gate_count -le 0 -or
            [int]$report.payload.executed_gate_count -ne [int]$report.payload.passed_gate_count -or
            [int]$report.payload.skipped_gate_count -ne 0) {
            throw "GPU color verifier did not prove complete physical-adapter execution."
        }
        switch ([string]$cell.environment.driver.kind) {
            "explicit" {
                if ([string]$report.payload.driver_name -ne [string]$cell.environment.driver.name -or
                    [string]$report.payload.driver_version -ne [string]$cell.environment.driver.version) {
                    throw "GPU color verifier observed another explicit driver identity."
                }
            }
            "os_bundled" {
                if ([string]$report.payload.driver_os_build -ne [string]$cell.environment.driver.os_build) {
                    throw "GPU color verifier observed another OS-bundled driver identity."
                }
            }
            "linux_stack" {
                foreach ($field in @("kernel_version", "drm_driver", "vulkan_driver", "vulkan_driver_version", "mesa_version")) {
                    if ([string]$report.payload.$field -ne [string]$cell.environment.driver.$field) {
                        throw "GPU color verifier observed another Linux driver stack field '$field'."
                    }
                }
            }
        }
        Assert-ExactStringSet @($policy.owner_verifier_contracts.gpu_color_required_gates) `
            @($raw.gates | ForEach-Object { [string]$_.id }) "GPU color gate closure"
        Assert-ExactStringSet @($policy.owner_verifier_contracts.gpu_color_required_gates) `
            @($report.payload.gate_ids) "GPU color report gate closure"
        foreach ($gate in @($raw.gates)) {
            if ([string]$gate.status -ne [string]$policy.owner_verifier_contracts.qualified_gate_status -or
                [string]$gate.report_sha256 -notmatch '^[0-9a-f]{64}$' -or
                [string]$gate.adapter_name -ne [string]$cell.environment.adapter_name -or
                [string]$gate.adapter_vendor -ne [string]$cell.environment.adapter_vendor -or
                [string]$gate.adapter_device_id -ne [string]$cell.environment.adapter_device_id -or
                [string]$gate.renderer_driver -ne [string]$cell.environment.renderer_driver -or
                [string]$gate.renderer_driver_info -ne [string]$cell.environment.renderer_driver_info) {
                throw "GPU color gate '$($gate.id)' is not qualified on the exact row adapter."
            }
        }
    }
    "viewer_display" {
        if ([string]$report.payload.display_identity -ne [string]$cell.environment.display_identity -or
            [string]$report.payload.executed_runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
            [string]$raw.executed_runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
            [string]$raw.renderer_adapter.name -ne [string]$cell.environment.adapter_name -or
            [string]$raw.renderer_adapter.vendor_id -ne [string]$cell.environment.adapter_vendor -or
            [string]$raw.renderer_adapter.device_id -ne [string]$cell.environment.adapter_device_id -or
            [string]$raw.renderer_adapter.driver -ne [string]$cell.environment.renderer_driver -or
            [string]$raw.renderer_adapter.driver_info -ne [string]$cell.environment.renderer_driver_info -or
            ([string]$raw.renderer_adapter.backend).ToLowerInvariant() -ne ([string]$cell.environment.graphics_backend).ToLowerInvariant()) {
            throw "Viewer evidence was captured on another display, active Renderer adapter, or product runtime image."
        }
        Assert-ExactStringSet @($cell.scenarios | ForEach-Object { [string]$_.scenario }) `
            @($report.payload.scenarios | ForEach-Object { [string]$_.scenario }) `
            "Viewer scenario closure"
        Assert-ExactStringSet @($cell.scenarios | ForEach-Object { [string]$_.scenario }) `
            @($raw.scenarios | ForEach-Object { [string]$_.scenario }) `
            "raw Viewer scenario closure"
        foreach ($scenario in @($report.payload.scenarios)) {
            $matches = @($cell.scenarios | Where-Object { [string]$_.scenario -eq [string]$scenario.scenario })
            if ($matches.Count -ne 1 -or $scenario.viewer_ready -ne $true -or
                $scenario.display_contract_valid -ne $true -or
                $scenario.external_texture_presented -ne $true -or
                $scenario.zero_readback_stages -ne $true -or
                $scenario.operator_observation_passed -ne $true -or
                $scenario.capability_skip_observed -ne $false -or
                [string]$scenario.output_contract_sha256 -ne [string]$matches[0].output_contract_sha256 -or
                [string]$scenario.operator_attestation_sha256 -ne [string]$matches[0].operator_attestation_sha256) {
                throw "Viewer owner verifier rejected scenario '$($scenario.scenario)'."
            }
            Assert-ScenarioMatches $matches[0] $scenario "normalized report"
            $rawMatches = @($raw.scenarios | Where-Object { [string]$_.scenario -eq [string]$scenario.scenario })
            if ($rawMatches.Count -ne 1) { throw "Raw Viewer scenario is not unique." }
            Assert-ScenarioMatches $matches[0] $rawMatches[0] "raw evidence"
        }
    }
}
