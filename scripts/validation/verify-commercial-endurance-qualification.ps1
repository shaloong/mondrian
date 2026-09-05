param(
    [string]$ProfilePath = "tests/validation/commercial-endurance-qualification.json",
    [Parameter(Mandatory = $true)][string]$RunManifestPath,
    [Parameter(Mandatory = $true)][string]$ChunkDirectory,
    [Parameter(Mandatory = $true)][string]$ReplayBinaryPath,
    [Parameter(Mandatory = $true)][string]$ReplayBinarySha256,
    [Parameter(Mandatory = $true)][string]$ExpectedProfileFileSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedCaptureAuthorityPath,
    [Parameter(Mandatory = $true)][string]$ExpectedCaptureAuthoritySha256,
    [Parameter(Mandatory = $true)][string]$ExpectedMachinePlanPath,
    [Parameter(Mandatory = $true)][string]$ExpectedMachinePlanSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedSourceRevision,
    [Parameter(Mandatory = $true)][string]$ExpectedReleaseCandidateId,
    [Parameter(Mandatory = $true)][string]$ExpectedProductArtifactSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedRuntimeImageSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedBuildProvenanceSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedMachineReportSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedPlatformCellSha256,
    [Parameter(Mandatory = $true)][string]$OutputPath,
    [ValidateRange(1, 3600)][int]$ReplayTimeoutSeconds = 120
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$script:ObservedJsonHashes = [System.Collections.Generic.Dictionary[string,string]]::new(
    [StringComparer]::Ordinal
)

function Assert-NoReparseAncestry([string]$Path, [string]$Description) {
    $current = Get-Item -LiteralPath ([IO.Path]::GetFullPath($Path)) -Force
    while ($null -ne $current) {
        if ($current.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "$Description crosses a reparse-point ancestor: $($current.FullName)"
        }
        $parentPath = Split-Path -Parent $current.FullName
        if ([string]::IsNullOrWhiteSpace($parentPath) -or $parentPath -ceq $current.FullName) {
            break
        }
        $current = Get-Item -LiteralPath $parentPath -Force
    }
}

function Resolve-ExistingLeaf([string]$Path, [string]$Description) {
    $resolved = [IO.Path]::GetFullPath($Path)
    $item = Get-Item -LiteralPath $resolved -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Description must be a regular non-reparse file: $resolved"
    }
    Assert-NoReparseAncestry $resolved $Description
    return $resolved
}

function Resolve-ExistingDirectory([string]$Path, [string]$Description) {
    $resolved = [IO.Path]::GetFullPath($Path)
    $item = Get-Item -LiteralPath $resolved -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Description must be a real non-reparse directory: $resolved"
    }
    Assert-NoReparseAncestry $resolved $Description
    return $resolved.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
}

function Read-BoundedJson([string]$Path, [string]$Description, [int64]$MaximumBytes = 8388608) {
    $absolute = [IO.Path]::GetFullPath($Path)
    $stream = [IO.FileStream]::new(
        $absolute,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    try {
        $length = $stream.Length
        if ($length -le 0 -or $length -gt $MaximumBytes -or $length -gt [int]::MaxValue) {
            throw "$Description exceeds its bounded JSON size: $length bytes"
        }
        $bytes = [byte[]]::new([int]$length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -eq 0) { throw "$Description changed while its bytes were read." }
            $offset += $read
        }
        if ($stream.ReadByte() -ne -1 -or $stream.Length -ne $length) {
            throw "$Description changed while its bytes were read."
        }
    }
    finally {
        $stream.Dispose()
    }
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = ([Convert]::ToHexString($algorithm.ComputeHash($bytes))).ToLowerInvariant()
    }
    finally {
        $algorithm.Dispose()
    }
    $script:ObservedJsonHashes[$absolute] = $digest
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
    return $text | ConvertFrom-Json
}

function Assert-LowerSha256([string]$Value, [string]$Description) {
    if ($Value -cnotmatch '^[0-9a-f]{64}$') { throw "$Description must be lowercase SHA-256." }
}

function Get-LowerSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-LowerUtf8Sha256([string]$Value) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($Value)
        return ([Convert]::ToHexString($algorithm.ComputeHash($bytes))).ToLowerInvariant()
    }
    finally {
        $algorithm.Dispose()
    }
}

function Assert-ExactJsonProperties([object]$Object, [string[]]$Expected, [string]$Description) {
    $actual = @($Object.PSObject.Properties.Name | Sort-Object)
    $expectedSorted = @($Expected | Sort-Object)
    if (@(Compare-Object $expectedSorted $actual).Count -ne 0) {
        throw "$Description has unknown or missing properties."
    }
}

function Get-ClosureSnapshot([string[]]$Paths) {
    $rows = [System.Collections.Generic.List[object]]::new()
    foreach ($path in $Paths) {
        [void]$rows.Add([pscustomobject]@{
            path = [IO.Path]::GetFullPath($path)
            length = (Get-Item -LiteralPath $path -Force).Length
            sha256 = Get-LowerSha256 $path
        })
    }
    return @($rows | Sort-Object path)
}

function Assert-SnapshotAnchor(
    [object[]]$Snapshot,
    [string]$Path,
    [string]$ExpectedSha256,
    [string]$Description
) {
    $absolute = [IO.Path]::GetFullPath($Path)
    $matches = @($Snapshot | Where-Object { [string]$_.path -ceq $absolute })
    if ($matches.Count -ne 1 -or [string]$matches[0].sha256 -cne $ExpectedSha256) {
        throw "$Description changed before the immutable replay snapshot."
    }
}

function Assert-EvidenceDirectoryClosure([string]$Root, [string[]]$DeclaredNames) {
    $items = @(Get-ChildItem -LiteralPath $Root -Force)
    if (@($items | Where-Object { $_.PSIsContainer -or ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) }).Count -ne 0) {
        throw "Endurance evidence directory contains a directory or reparse point."
    }
    $actualNames = @($items | ForEach-Object Name)
    if (@(Compare-Object ($DeclaredNames | Sort-Object) ($actualNames | Sort-Object)).Count -ne 0) {
        throw "Endurance evidence directory does not exactly match the manifest closure."
    }
}

$profile = Resolve-ExistingLeaf $ProfilePath "Endurance profile"
$manifestPath = Resolve-ExistingLeaf $RunManifestPath "Endurance run manifest"
$chunkRoot = Resolve-ExistingDirectory $ChunkDirectory "Endurance chunk directory"
$replayBinary = Resolve-ExistingLeaf $ReplayBinaryPath "Endurance replay binary"
$captureAuthorityPath = Resolve-ExistingLeaf $ExpectedCaptureAuthorityPath "Capture authority manifest"
$machinePlanPath = Resolve-ExistingLeaf $ExpectedMachinePlanPath "Commercial endurance machine plan"
$output = [IO.Path]::GetFullPath($OutputPath)
$outputParent = Resolve-ExistingDirectory (Split-Path -Parent $output) "Endurance report parent"
if (Test-Path -LiteralPath $output) { throw "Endurance report output is create-only: $output" }

foreach ($pair in @(
    @($ReplayBinarySha256, "Replay binary SHA-256"),
    @($ExpectedProfileFileSha256, "Profile file SHA-256"),
    @($ExpectedCaptureAuthoritySha256, "Capture authority manifest SHA-256"),
    @($ExpectedMachinePlanSha256, "Machine plan SHA-256"),
    @($ExpectedProductArtifactSha256, "Product artifact SHA-256"),
    @($ExpectedRuntimeImageSha256, "Runtime image SHA-256"),
    @($ExpectedBuildProvenanceSha256, "Build provenance SHA-256"),
    @($ExpectedMachineReportSha256, "Machine report SHA-256"),
    @($ExpectedPlatformCellSha256, "Platform cell SHA-256")
)) { Assert-LowerSha256 ([string]$pair[0]) ([string]$pair[1]) }
if ($ExpectedSourceRevision -cnotmatch '^[0-9a-f]{40}$') {
    throw "Expected source revision must be a lowercase 40-hex Git SHA."
}
if ((Get-LowerSha256 $replayBinary) -cne $ReplayBinarySha256) {
    throw "Endurance replay binary differs from the external approved hash."
}
if ((Get-LowerSha256 $profile) -cne $ExpectedProfileFileSha256) {
    throw "Endurance profile differs from the external approved hash."
}
if ((Get-LowerSha256 $captureAuthorityPath) -cne $ExpectedCaptureAuthoritySha256) {
    throw "Endurance capture authority differs from the external approved hash."
}
if ((Get-LowerSha256 $machinePlanPath) -cne $ExpectedMachinePlanSha256) {
    throw "Endurance machine plan differs from the external approved hash."
}

$profileObject = Read-BoundedJson $profile "Endurance profile"
$manifest = Read-BoundedJson $manifestPath "Endurance run manifest"
$captureAuthority = Read-BoundedJson $captureAuthorityPath "Endurance capture authority"
$machinePlan = Read-BoundedJson $machinePlanPath "Commercial endurance machine plan" 262144
if ([int]$profileObject.schema_version -ne 1 -or [int]$manifest.schema_version -ne 2 -or
    [int]$captureAuthority.schema_version -ne 2 -or [int]$machinePlan.schema_version -ne 1) {
    throw "Endurance profile/machine plan must use schema 1 and run/authority must use schema 2."
}
$bindings = @(
    @([string]$manifest.source_revision, $ExpectedSourceRevision, "source revision"),
    @([string]$manifest.release_candidate_id, $ExpectedReleaseCandidateId, "release candidate"),
    @([string]$manifest.product_artifact_sha256, $ExpectedProductArtifactSha256, "product artifact"),
    @([string]$manifest.runtime_image_sha256, $ExpectedRuntimeImageSha256, "runtime image"),
    @([string]$manifest.build_provenance_sha256, $ExpectedBuildProvenanceSha256, "build provenance"),
    @([string]$manifest.machine_report_sha256, $ExpectedMachineReportSha256, "machine report"),
    @([string]$manifest.platform_cell_sha256, $ExpectedPlatformCellSha256, "platform cell"),
    @([string]$manifest.machine_plan_sha256, $ExpectedMachinePlanSha256, "machine plan"),
    @([string]$manifest.capture_authority_sha256, $ExpectedCaptureAuthoritySha256, "capture authority")
)
foreach ($binding in $bindings) {
    if ([string]$binding[0] -cne [string]$binding[1]) {
        throw "Endurance $($binding[2]) differs from the external trust anchor."
    }
}
foreach ($binding in @(
    @([string]$captureAuthority.run_id, [string]$manifest.run_id, "authority run id"),
    @([string]$captureAuthority.profile_file_sha256, $ExpectedProfileFileSha256, "authority profile"),
    @([string]$captureAuthority.source_revision, $ExpectedSourceRevision, "authority source revision"),
    @([string]$captureAuthority.release_candidate_id, $ExpectedReleaseCandidateId, "authority release candidate"),
    @([string]$captureAuthority.product_artifact_sha256, $ExpectedProductArtifactSha256, "authority product artifact"),
    @([string]$captureAuthority.runtime_image_sha256, $ExpectedRuntimeImageSha256, "authority runtime image"),
    @([string]$captureAuthority.build_provenance_sha256, $ExpectedBuildProvenanceSha256, "authority build provenance"),
    @([string]$captureAuthority.machine_report_sha256, $ExpectedMachineReportSha256, "authority machine report"),
    @([string]$captureAuthority.platform_cell_sha256, $ExpectedPlatformCellSha256, "authority platform cell"),
    @([string]$captureAuthority.machine_plan_sha256, $ExpectedMachinePlanSha256, "authority machine plan")
)) {
    if ([string]$binding[0] -cne [string]$binding[1]) {
        throw "Endurance $($binding[2]) differs from the sealed run."
    }
}
if ([string]::IsNullOrWhiteSpace([string]$captureAuthority.single_use_challenge) -or
    [string]$captureAuthority.single_use_challenge -match 'placeholder') {
    throw "Endurance capture authority must carry a non-placeholder single-use challenge."
}
if ([string]$captureAuthority.authority_id -cne "external-commercial-endurance-authority-v2") {
    throw "Endurance capture authority uses an unapproved authority implementation."
}
if (@($captureAuthority.phases).Count -ne @($profileObject.phases).Count -or
    @($manifest.phases).Count -ne @($profileObject.phases).Count) {
    throw "Endurance capture authority, profile, and manifest phase counts differ."
}
if ([string]$manifest.environment_before_sha256 -cne [string]$manifest.environment_after_sha256) {
    throw "Endurance environment changed during the serial run."
}

$declaredNames = [System.Collections.Generic.List[string]]::new()
$evidencePaths = [System.Collections.Generic.List[string]]::new()
foreach ($phase in @($manifest.phases)) {
    $profileMatches = @($profileObject.phases | Where-Object { [string]$_.phase_id -ceq [string]$phase.phase_id })
    $authorityMatches = @($captureAuthority.phases | Where-Object { [string]$_.phase_id -ceq [string]$phase.phase_id })
    if ($profileMatches.Count -ne 1 -or $authorityMatches.Count -ne 1) {
        throw "Endurance phase is not uniquely bound by profile and capture authority: $($phase.phase_id)"
    }
    $profilePhase = $profileMatches[0]
    $authorityPhase = $authorityMatches[0]
    foreach ($binding in @(
        @([string]$phase.workload_sha256, [string]$profilePhase.workload_sha256, "profile workload"),
        @([string]$phase.workload_sha256, [string]$authorityPhase.workload_sha256, "authority workload"),
        @([string]$phase.producer.owner, [string]$profilePhase.producer_owner, "producer owner"),
        @([string]$phase.producer.verifier_id, [string]$profilePhase.producer_verifier_id, "producer verifier"),
        @([string]$phase.producer.report_schema_version, [string]$profilePhase.producer_report_schema_version, "producer schema"),
        @([string]$phase.producer.owner, [string]$authorityPhase.producer_owner, "authority producer owner"),
        @([string]$phase.producer.verifier_id, [string]$authorityPhase.producer_verifier_id, "authority producer verifier")
    )) {
        if ([string]$binding[0] -cne [string]$binding[1]) {
            throw "Endurance phase $($phase.phase_id) differs from its $($binding[2]) binding."
        }
    }
    $producerPaths = @{}
    foreach ($producerEntry in @(
        @([string]$phase.producer.report_file_name, [string]$phase.producer.report_sha256, "producer report"),
        @([string]$phase.producer.raw_evidence_file_name, [string]$phase.producer.raw_evidence_sha256, "producer raw evidence")
    )) {
        $name = [string]$producerEntry[0]
        if ($name -notmatch '^[A-Za-z0-9._-]{1,128}$' -or $name -in @('.', '..')) {
            throw "Invalid link-free endurance evidence file name: $name"
        }
        if ($declaredNames.Contains($name)) { throw "Duplicate endurance evidence file: $name" }
        Assert-LowerSha256 ([string]$producerEntry[1]) "Endurance $($producerEntry[2]) digest"
        [void]$declaredNames.Add($name)
        $path = Resolve-ExistingLeaf (Join-Path $chunkRoot $name) "Endurance $($producerEntry[2])"
        if ((Get-LowerSha256 $path) -cne [string]$producerEntry[1]) {
            throw "Endurance $($producerEntry[2]) file differs from its manifest digest: $name"
        }
        $producerPaths[[string]$producerEntry[2]] = $path
        [void]$evidencePaths.Add($path)
    }
    $producerReport = Read-BoundedJson $producerPaths["producer report"] "Endurance producer report"
    $rawEvidence = Read-BoundedJson $producerPaths["producer raw evidence"] "Endurance producer raw evidence"
    foreach ($binding in @(
        @([string]$producerReport.schema_version, [string]$phase.producer.report_schema_version, "owner report schema"),
        @([string]$producerReport.phase_id, [string]$phase.phase_id, "owner report phase"),
        @([string]$producerReport.run_id, [string]$manifest.run_id, "owner report run"),
        @([string]$producerReport.authority_challenge, [string]$captureAuthority.single_use_challenge, "owner report challenge"),
        @([string]$producerReport.workload_sha256, [string]$phase.workload_sha256, "owner report workload"),
        @([string]$producerReport.producer_owner, [string]$phase.producer.owner, "owner report producer"),
        @([string]$producerReport.producer_verifier_id, [string]$phase.producer.verifier_id, "owner report verifier"),
        @([string]$producerReport.raw_evidence_sha256, [string]$phase.producer.raw_evidence_sha256, "owner report raw evidence")
    )) {
        if ([string]$binding[0] -cne [string]$binding[1]) {
            throw "Endurance phase $($phase.phase_id) differs from its $($binding[2]) binding."
        }
    }
    foreach ($binding in @(
        @([string]$rawEvidence.schema_version, "2", "raw evidence schema"),
        @([string]$rawEvidence.phase_id, [string]$phase.phase_id, "raw evidence phase"),
        @([string]$rawEvidence.run_id, [string]$manifest.run_id, "raw evidence run"),
        @([string]$rawEvidence.authority_challenge, [string]$captureAuthority.single_use_challenge, "raw evidence challenge"),
        @([string]$rawEvidence.workload_sha256, [string]$phase.workload_sha256, "raw evidence workload"),
        @([string]$rawEvidence.producer_owner, [string]$phase.producer.owner, "raw evidence producer"),
        @([string]$rawEvidence.producer_verifier_id, [string]$phase.producer.verifier_id, "raw evidence verifier")
    )) {
        if ([string]$binding[0] -cne [string]$binding[1]) {
            throw "Endurance phase $($phase.phase_id) differs from its $($binding[2]) binding."
        }
    }
    $events = @($rawEvidence.events)
    if ($events.Count -gt [int]$profileObject.maximum_producer_events_per_phase) {
        throw "Endurance phase $($phase.phase_id) exceeds its producer-event bound."
    }
    $expectedSequence = 0
    [int64]$lastEventTime = -1
    [int64]$verifiedExports = 0
    [int64]$recoveryStepCount = 0
    [int64]$completedRecoveryCycles = 0
    $recoverySteps = @("seek", "surface_device_reopen", "export_cancel_retry", "cache_pressure")
    $recoveryOperationIds = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($event in $events) {
        if ([int]$event.sequence -ne $expectedSequence) {
            throw "Endurance producer event sequence is not contiguous for phase $($phase.phase_id)."
        }
        [int64]$eventTime = [int64]$event.completed_at_us
        [int64]$phaseDuration = [int64]$phase.completed_at_run_us - [int64]$phase.started_at_run_us
        if ($eventTime -lt $lastEventTime -or $eventTime -gt $phaseDuration) {
            throw "Endurance producer event time is invalid for phase $($phase.phase_id)."
        }
        switch ([string]$event.kind) {
            "export_artifact_verified" {
                if ([string]$profilePhase.kind -ceq "playback_reference" -or
                    [string]$event.artifact_id -cnotmatch '^[A-Za-z0-9._-]{1,128}$' -or
                    [string]$event.validator_id -cnotmatch '^[A-Za-z0-9._-]{1,128}$') {
                    throw "Endurance Export verification event is invalid for phase $($phase.phase_id)."
                }
                Assert-LowerSha256 ([string]$event.artifact_sha256) "Export artifact digest"
                Assert-LowerSha256 ([string]$event.validation_report_sha256) "Export validation report digest"
                $verifiedExports += 1
            }
            "recovery_step_completed" {
                if ([string]$profilePhase.kind -cne "concurrent_recovery") {
                    throw "Endurance recovery event appeared outside the recovery phase."
                }
                [int64]$expectedCycle = [Math]::Floor($recoveryStepCount / 4)
                $expectedStep = $recoverySteps[$recoveryStepCount % 4]
                if ([int64]$event.cycle_index -ne $expectedCycle -or [string]$event.step -cne $expectedStep) {
                    throw "Endurance recovery event order is invalid for phase $($phase.phase_id)."
                }
                Assert-LowerSha256 ([string]$event.operation_receipt_sha256) "Recovery operation receipt digest"
                $receiptJson = [string]$event.operation_receipt_json
                $receiptBytes = [Text.Encoding]::UTF8.GetByteCount($receiptJson)
                if ($receiptBytes -le 0 -or $receiptBytes -gt 4096 -or
                    (Get-LowerUtf8Sha256 $receiptJson) -cne [string]$event.operation_receipt_sha256) {
                    throw "Endurance recovery receipt bytes do not match their bounded digest."
                }
                $receipt = $receiptJson | ConvertFrom-Json
                if ([int]$receipt.schema_version -ne 3 -or
                    [int64]$receipt.cycle_index -ne $expectedCycle -or
                    [string]$receipt.step -cne $expectedStep -or
                    [string]$receipt.operation_id -cnotmatch '^[A-Za-z0-9._-]{1,128}$' -or
                    -not $recoveryOperationIds.Add([string]$receipt.operation_id)) {
                    throw "Endurance recovery receipt common evidence is invalid or replayed."
                }
                switch ($expectedStep) {
                    "seek" {
                        if ([string]$event.window_run_receipt_json -ne "" -or
                            [string]$event.window_run_receipt_sha256 -ne "") {
                            throw "Window-run evidence appeared on a non-Window recovery step."
                        }
                        Assert-ExactJsonProperties $receipt @(
                            "step", "schema_version", "cycle_index", "operation_id",
                            "sequence_binding_sha256", "from_frame", "target_frame",
                            "before_epoch", "after_epoch", "exact_picture_ready"
                        ) "Seek recovery receipt"
                        Assert-LowerSha256 ([string]$receipt.sequence_binding_sha256) "Seek Sequence binding"
                        if ([int64]$receipt.from_frame -lt 0 -or [int64]$receipt.target_frame -lt 0 -or
                            [int64]$receipt.from_frame -eq [int64]$receipt.target_frame -or
                            [uint64]$receipt.after_epoch -le [uint64]$receipt.before_epoch -or
                            -not [bool]$receipt.exact_picture_ready) {
                            throw "Seek recovery receipt does not prove an exact completed seek."
                        }
                    }
                    "surface_device_reopen" {
                        $windowRunJson = [string]$event.window_run_receipt_json
                        $windowRunSha256 = [string]$event.window_run_receipt_sha256
                        if ($windowRunJson -eq "" -or $windowRunSha256 -eq "") {
                            throw "Surface recovery requires one Window-run receipt JSON/hash pair."
                        }
                        if ($windowRunJson -ne "") {
                            Assert-LowerSha256 $windowRunSha256 "Window-run receipt digest"
                            if ([Text.Encoding]::UTF8.GetByteCount($windowRunJson) -gt 131072 -or
                                (Get-LowerUtf8Sha256 $windowRunJson) -cne $windowRunSha256) {
                                throw "Window-run receipt bytes do not match their bounded digest."
                            }
                            $windowRun = $windowRunJson | ConvertFrom-Json
                            Assert-ExactJsonProperties $windowRun @(
                                "schema_version", "outcome", "recovery_receipt_json",
                                "recovery_receipt_sha256", "runtime_shutdown_json",
                                "runtime_shutdown_sha256", "host_shutdown_json",
                                "host_shutdown_sha256", "gpu_shutdown_json",
                                "gpu_shutdown_sha256", "native_return_json",
                                "native_return_sha256"
                            ) "Window-run receipt"
                            if ([int]$windowRun.schema_version -ne 1 -or
                                [string]$windowRun.outcome -cne "active_exited" -or
                                [string]$windowRun.recovery_receipt_json -cne $receiptJson -or
                                [string]$windowRun.recovery_receipt_sha256 -cne [string]$event.operation_receipt_sha256) {
                                throw "Window-run receipt is not bound to the Surface recovery operation."
                            }
                            foreach ($leaf in @(
                                [pscustomobject]@{ Json = [string]$windowRun.runtime_shutdown_json; Sha256 = [string]$windowRun.runtime_shutdown_sha256 },
                                [pscustomobject]@{ Json = [string]$windowRun.host_shutdown_json; Sha256 = [string]$windowRun.host_shutdown_sha256 },
                                [pscustomobject]@{ Json = [string]$windowRun.gpu_shutdown_json; Sha256 = [string]$windowRun.gpu_shutdown_sha256 },
                                [pscustomobject]@{ Json = [string]$windowRun.native_return_json; Sha256 = [string]$windowRun.native_return_sha256 }
                            )) {
                                Assert-LowerSha256 $leaf.Sha256 "Window-run embedded receipt digest"
                                if ([Text.Encoding]::UTF8.GetByteCount($leaf.Json) -le 0 -or
                                    (Get-LowerUtf8Sha256 $leaf.Json) -cne $leaf.Sha256) {
                                    throw "Window-run embedded receipt bytes do not match their digest."
                                }
                                $null = $leaf.Json | ConvertFrom-Json
                            }
                            $nativeReturn = ([string]$windowRun.native_return_json) | ConvertFrom-Json
                            Assert-ExactJsonProperties $nativeReturn @(
                                "event_loop_borrow_returned", "window_owner_scope_exited",
                                "physical_native_termination"
                            ) "Window native-return evidence"
                            if (-not [bool]$nativeReturn.event_loop_borrow_returned -or
                                -not [bool]$nativeReturn.window_owner_scope_exited -or
                                [string]$nativeReturn.physical_native_termination -cne "unverified") {
                                throw "Window native-return evidence overclaims or omits authority release."
                            }
                        }
                        Assert-ExactJsonProperties $receipt @(
                            "step", "schema_version", "cycle_index", "operation_id",
                            "sequence_binding_sha256", "surface_generation_before",
                            "surface_generation_after", "device_generation_before",
                            "device_generation_after", "shutdown_receipt_json",
                            "shutdown_receipt_sha256", "reopened_contract_json",
                            "reopened_contract_sha256"
                        ) "Surface/device recovery receipt"
                        Assert-LowerSha256 ([string]$receipt.sequence_binding_sha256) "Reopen Sequence binding"
                        Assert-LowerSha256 ([string]$receipt.shutdown_receipt_sha256) "Reopen shutdown receipt"
                        Assert-LowerSha256 ([string]$receipt.reopened_contract_sha256) "Reopened contract"
                        $shutdownJson = [string]$receipt.shutdown_receipt_json
                        $reopenedJson = [string]$receipt.reopened_contract_json
                        if ([Text.Encoding]::UTF8.GetByteCount($shutdownJson) -le 0 -or
                            [Text.Encoding]::UTF8.GetByteCount($shutdownJson) -gt 4096 -or
                            (Get-LowerUtf8Sha256 $shutdownJson) -cne [string]$receipt.shutdown_receipt_sha256 -or
                            [Text.Encoding]::UTF8.GetByteCount($reopenedJson) -le 0 -or
                            [Text.Encoding]::UTF8.GetByteCount($reopenedJson) -gt 4096 -or
                            (Get-LowerUtf8Sha256 $reopenedJson) -cne [string]$receipt.reopened_contract_sha256) {
                            throw "Surface/device nested evidence does not match its bounded digest."
                        }
                        $shutdown = $shutdownJson | ConvertFrom-Json
                        $reopened = $reopenedJson | ConvertFrom-Json
                        Assert-ExactJsonProperties $shutdown @(
                            "schema_version", "worker_started", "worker_terminated",
                            "worker_panicked", "timed_out", "retirement_requested",
                            "retirement_handoff_accepted", "retirement_completed",
                            "generation_terminal_kind"
                        ) "Surface/device shutdown evidence"
                        Assert-ExactJsonProperties $reopened @(
                            "schema_version", "surface_generation", "device_generation",
                            "actual_surface_presented", "original_picture_sha256",
                            "reopened_picture_json", "reopened_picture_sha256"
                        ) "Reopened Surface contract"
                        Assert-LowerSha256 ([string]$reopened.original_picture_sha256) "Original Surface picture"
                        Assert-LowerSha256 ([string]$reopened.reopened_picture_sha256) "Reopened Surface picture"
                        $pictureJson = [string]$reopened.reopened_picture_json
                        if ([Text.Encoding]::UTF8.GetByteCount($pictureJson) -le 0 -or
                            [Text.Encoding]::UTF8.GetByteCount($pictureJson) -gt 4096 -or
                            (Get-LowerUtf8Sha256 $pictureJson) -cne [string]$reopened.reopened_picture_sha256 -or
                            [string]$reopened.original_picture_sha256 -cne [string]$reopened.reopened_picture_sha256) {
                            throw "Reopened Surface picture does not match the original presented picture digest."
                        }
                        $picture = $pictureJson | ConvertFrom-Json
                        Assert-ExactJsonProperties $picture @(
                            "sequence_id", "frame", "width", "height", "output_target",
                            "output_color_space", "monitor_color_space", "tone_map",
                            "display_view", "frame_residency", "display_contract_sha256"
                        ) "Reopened Surface picture contract"
                        Assert-LowerSha256 ([string]$picture.display_contract_sha256) "Reopened display contract"
                        if ([uint64]$receipt.surface_generation_before -eq 0 -or
                            [uint64]$receipt.surface_generation_after -eq 0 -or
                            [uint64]$receipt.surface_generation_before -eq [uint64]$receipt.surface_generation_after -or
                            [uint64]$receipt.device_generation_before -eq 0 -or
                            [uint64]$receipt.device_generation_after -eq 0 -or
                            [uint64]$receipt.device_generation_before -eq [uint64]$receipt.device_generation_after -or
                            [int]$shutdown.schema_version -ne 1 -or -not [bool]$shutdown.worker_started -or
                            -not [bool]$shutdown.worker_terminated -or [bool]$shutdown.worker_panicked -or
                            [bool]$shutdown.timed_out -or -not [bool]$shutdown.retirement_requested -or
                            -not [bool]$shutdown.retirement_handoff_accepted -or
                            -not [bool]$shutdown.retirement_completed -or $null -ne $shutdown.generation_terminal_kind -or
                            [int]$reopened.schema_version -ne 2 -or
                            [uint64]$reopened.surface_generation -ne [uint64]$receipt.surface_generation_after -or
                            [uint64]$reopened.device_generation -ne [uint64]$receipt.device_generation_after -or
                            -not [bool]$reopened.actual_surface_presented -or
                            [string]$picture.sequence_id -eq "" -or
                            [int64]$picture.frame -lt 0 -or
                            [uint64]$picture.width -eq 0 -or [uint64]$picture.height -eq 0 -or
                            [string]$picture.output_target -cne "Display" -or
                            -not [bool]$picture.frame_residency.execution_observed -or
                            [string]$picture.frame_residency.working_residency -cne "GpuWorkingCompositeExecuted") {
                            throw "Surface/device recovery receipt does not prove replacement generations."
                        }
                    }
                    "export_cancel_retry" {
                        if ([string]$event.window_run_receipt_json -ne "" -or
                            [string]$event.window_run_receipt_sha256 -ne "") {
                            throw "Window-run evidence appeared on a non-Window recovery step."
                        }
                        Assert-ExactJsonProperties $receipt @(
                            "step", "schema_version", "cycle_index", "operation_id",
                            "cancelled_job_id", "retry_job_id", "cancellation_count_before",
                            "cancellation_count_after", "cancelled_terminal_sha256",
                            "retry_artifact_sha256", "retry_validation_report_sha256"
                        ) "Export cancel/retry receipt"
                        foreach ($token in @([string]$receipt.cancelled_job_id, [string]$receipt.retry_job_id)) {
                            if ($token -cnotmatch '^[A-Za-z0-9._-]{1,128}$') {
                                throw "Export cancel/retry receipt contains an invalid Job identity."
                            }
                        }
                        foreach ($digest in @(
                            [string]$receipt.cancelled_terminal_sha256,
                            [string]$receipt.retry_artifact_sha256,
                            [string]$receipt.retry_validation_report_sha256
                        )) { Assert-LowerSha256 $digest "Export cancel/retry evidence" }
                        if ([string]$receipt.cancelled_job_id -ceq [string]$receipt.retry_job_id -or
                            [uint64]$receipt.cancellation_count_after -ne
                                ([uint64]$receipt.cancellation_count_before + 1)) {
                            throw "Export cancel/retry receipt does not prove one cancellation and a distinct retry."
                        }
                    }
                    "cache_pressure" {
                        if ([string]$event.window_run_receipt_json -ne "" -or
                            [string]$event.window_run_receipt_sha256 -ne "") {
                            throw "Window-run evidence appeared on a non-Window recovery step."
                        }
                        Assert-ExactJsonProperties $receipt @(
                            "step", "schema_version", "cycle_index", "operation_id",
                            "decision_generation_before", "pressure_decision_generation",
                            "recovered_decision_generation", "cache_bytes_before_pressure",
                            "cache_bytes_after_pressure", "pressure_trimmed_bytes",
                            "residual_owned_resources", "recovered_nominal",
                            "exact_picture_ready", "gpu_device_losses_before",
                            "gpu_device_losses_after", "fatal_errors_before",
                            "fatal_errors_after", "export_failures_before",
                            "export_failures_after",
                            "pressure_decision_sha256", "recovered_decision_sha256"
                        ) "Cache-pressure receipt"
                        Assert-LowerSha256 ([string]$receipt.pressure_decision_sha256) "Pressure decision"
                        Assert-LowerSha256 ([string]$receipt.recovered_decision_sha256) "Recovered decision"
                        $trimmed = [uint64]$receipt.cache_bytes_before_pressure - [uint64]$receipt.cache_bytes_after_pressure
                        if ([uint64]$receipt.decision_generation_before -ge [uint64]$receipt.pressure_decision_generation -or
                            [uint64]$receipt.pressure_decision_generation -ge [uint64]$receipt.recovered_decision_generation -or
                            [uint64]$receipt.cache_bytes_after_pressure -ge [uint64]$receipt.cache_bytes_before_pressure -or
                            $trimmed -ne [uint64]$receipt.pressure_trimmed_bytes -or $trimmed -eq 0 -or
                            [uint64]$receipt.residual_owned_resources -ne 0 -or
                            -not [bool]$receipt.recovered_nominal -or
                            -not [bool]$receipt.exact_picture_ready -or
                            [uint64]$receipt.gpu_device_losses_before -ne [uint64]$receipt.gpu_device_losses_after -or
                            [uint64]$receipt.fatal_errors_before -ne [uint64]$receipt.fatal_errors_after -or
                            [uint64]$receipt.export_failures_before -ne [uint64]$receipt.export_failures_after) {
                            throw "Cache-pressure receipt does not prove bounded trim and nominal recovery."
                        }
                    }
                }
                $recoveryStepCount += 1
                if (($recoveryStepCount % 4) -eq 0) { $completedRecoveryCycles += 1 }
            }
            default { throw "Unknown endurance producer event kind for phase $($phase.phase_id): $($event.kind)" }
        }
        $lastEventTime = $eventTime
        $expectedSequence += 1
    }
    if (($recoveryStepCount % 4) -ne 0) {
        throw "Endurance recovery evidence ends with a partial cycle."
    }
    foreach ($binding in @(
        @([string]$producerReport.terminal_status, [string]$phase.terminal.status, "owner report terminal status"),
        @([string]$producerReport.event_count, [string]$events.Count, "owner report event count"),
        @([string]$producerReport.verified_export_artifacts, [string]$verifiedExports, "owner report verified exports"),
        @([string]$producerReport.recovery_cycles, [string]$completedRecoveryCycles, "owner report recovery cycles"),
        @([string]$phase.terminal.counters.export_artifacts_verified, [string]$verifiedExports, "terminal verified exports"),
        @([string]$phase.terminal.counters.recovery_cycles, [string]$completedRecoveryCycles, "terminal recovery cycles")
    )) {
        if ([string]$binding[0] -cne [string]$binding[1]) {
            throw "Endurance phase $($phase.phase_id) differs from its $($binding[2]) semantic evidence."
        }
    }
    if ([string]$profilePhase.kind -ceq "concurrent_recovery" -and
        [int64]$phase.terminal.counters.export_cancellations -ne $completedRecoveryCycles) {
        throw "Endurance recovery cycles do not close against Export cancel/retry counters."
    }
    foreach ($receipt in @($phase.chunks)) {
        $name = [string]$receipt.file_name
        if ($name -notmatch '^[A-Za-z0-9._-]{1,128}$' -or $name -in @('.', '..')) {
            throw "Invalid link-free endurance chunk file name: $name"
        }
        if ($declaredNames.Contains($name)) { throw "Duplicate endurance chunk file: $name" }
        [void]$declaredNames.Add($name)
    }
}
Assert-EvidenceDirectoryClosure $chunkRoot @($declaredNames)
$chunkPaths = [System.Collections.Generic.List[string]]::new()
foreach ($phase in @($manifest.phases)) {
    foreach ($receipt in @($phase.chunks)) {
        $path = Resolve-ExistingLeaf (Join-Path $chunkRoot ([string]$receipt.file_name)) "Endurance chunk"
        Assert-LowerSha256 ([string]$receipt.chunk_sha256) "Endurance semantic chunk digest"
        [void]$chunkPaths.Add($path)
        [void]$evidencePaths.Add($path)
    }
}
$replayInputs = @($profile, $manifestPath, $replayBinary, $captureAuthorityPath, $machinePlanPath) + @($evidencePaths)
$before = Get-ClosureSnapshot $replayInputs
foreach ($observed in $script:ObservedJsonHashes.GetEnumerator()) {
    Assert-SnapshotAnchor $before $observed.Key $observed.Value "Parsed endurance JSON input"
}
Assert-SnapshotAnchor $before $replayBinary $ReplayBinarySha256 "Endurance replay binary"
Assert-SnapshotAnchor $before $profile $ExpectedProfileFileSha256 "Endurance profile"
Assert-SnapshotAnchor $before $captureAuthorityPath $ExpectedCaptureAuthoritySha256 "Endurance capture authority"
Assert-SnapshotAnchor $before $machinePlanPath $ExpectedMachinePlanSha256 "Endurance machine plan"

$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $replayBinary
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
[void]$startInfo.ArgumentList.Add($profile)
[void]$startInfo.ArgumentList.Add($manifestPath)
[void]$startInfo.ArgumentList.Add($chunkRoot)
[void]$startInfo.ArgumentList.Add($output)
$process = [Diagnostics.Process]::new()
$process.StartInfo = $startInfo
if (-not $process.Start()) { throw "Could not start the endurance replay binary." }
$stdoutTask = $process.StandardOutput.ReadToEndAsync()
$stderrTask = $process.StandardError.ReadToEndAsync()
if (-not $process.WaitForExit($ReplayTimeoutSeconds * 1000)) {
    try { $process.Kill($true) } catch {}
    throw "Endurance replay exceeded its bounded timeout."
}
$stdout = $stdoutTask.GetAwaiter().GetResult()
$stderr = $stderrTask.GetAwaiter().GetResult()
if ($stdout.Length -gt 65536 -or $stderr.Length -gt 65536) {
    throw "Endurance replay output exceeded its diagnostic bound."
}
if ($process.ExitCode -ne 0) {
    throw "Endurance replay failed with exit code $($process.ExitCode): $stderr"
}

$after = Get-ClosureSnapshot $replayInputs
if (($before | ConvertTo-Json -Depth 4 -Compress) -cne ($after | ConvertTo-Json -Depth 4 -Compress)) {
    throw "Endurance evidence changed while it was replayed."
}
Assert-EvidenceDirectoryClosure $chunkRoot @($declaredNames)
$resolvedOutput = Resolve-ExistingLeaf $output "Endurance qualification report"
$outputHashBefore = Get-LowerSha256 $resolvedOutput
$report = Read-BoundedJson $resolvedOutput "Endurance qualification report"
if ([int]$report.schema_version -ne 2 -or [string]$report.status -cne "qualified" -or
    @($report.missing_phases).Count -ne 0 -or [string]$report.evidence_sha256 -notmatch '^[0-9a-f]{64}$') {
    throw "Endurance replay output is not a complete qualified schema-2 report."
}
foreach ($binding in @(
    @([string]$report.run_id, [string]$manifest.run_id, "report run id"),
    @([string]$report.profile_sha256, [string]$manifest.profile_sha256, "report profile"),
    @([string]$report.source_revision, $ExpectedSourceRevision, "report source revision"),
    @([string]$report.release_candidate_id, $ExpectedReleaseCandidateId, "report release candidate"),
    @([string]$report.product_artifact_sha256, $ExpectedProductArtifactSha256, "report product artifact"),
    @([string]$report.runtime_image_sha256, $ExpectedRuntimeImageSha256, "report runtime image"),
    @([string]$report.build_provenance_sha256, $ExpectedBuildProvenanceSha256, "report build provenance"),
    @([string]$report.machine_report_sha256, $ExpectedMachineReportSha256, "report machine report"),
    @([string]$report.platform_cell_sha256, $ExpectedPlatformCellSha256, "report platform cell"),
    @([string]$report.machine_plan_sha256, $ExpectedMachinePlanSha256, "report machine plan"),
    @([string]$report.capture_authority_sha256, $ExpectedCaptureAuthoritySha256, "report capture authority")
)) {
    if ([string]$binding[0] -cne [string]$binding[1]) {
        throw "Endurance $($binding[2]) differs from the sealed run."
    }
}
if ((Get-LowerSha256 $resolvedOutput) -cne $outputHashBefore) {
    throw "Endurance qualification report changed while it was verified."
}
Write-Host "Commercial endurance qualification verified: $output"
