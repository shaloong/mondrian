param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CellObservationPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ArtifactManifestPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$BuildManifestPath,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{40}$")][string]$ExpectedSourceSha,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$OutputDirectory,
    [string]$PolicyPath = "tests/validation/platform-driver-display-matrix.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$jsonAdmissions = @{}

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Read-BoundedJson([string]$Path, [long]$MaximumBytes, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $MaximumBytes) {
        throw "$Label size is outside the sealed admission bound: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length -or $stream.Length -gt [int]::MaxValue) {
            throw "$Label changed or is too large for bounded JSON parsing."
        }
        $bytes = [byte[]]::new([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -le 0) { throw "$Label ended before its declared length." }
            $offset += $read
        }
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try { $hash = (($sha256.ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join "") }
        finally { $sha256.Dispose() }
        $script:jsonAdmissions[$item.FullName] = $hash
        return ([Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff) | ConvertFrom-Json)
    } catch {
        throw "$Label is not valid sealed JSON: $($_.Exception.Message)"
    } finally {
        $stream.Dispose()
    }
}

function Get-LowerSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Write-CreateOnlyJson([string]$Path, [object]$Value, [int]$Depth) {
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth $Depth))
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
}

function Get-BoundedArtifactSha256([string]$Path, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $script:maximumArtifactBytes) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length) {
            throw "$Label changed while it was being admitted: $Path"
        }
        $script:admittedEvidenceBytes += [long]$stream.Length
        if ($script:admittedEvidenceBytes -gt $script:maximumTotalEvidenceBytes) {
            throw "Row evidence exceeds the aggregate byte admission bound."
        }
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try {
            return (($sha256.ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) -join "")
        } finally {
            $sha256.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

function Get-BoundedSourceEvidence([string]$Path, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    $maximumBytes = [long]$script:policy.limits.maximum_source_evidence_file_bytes
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $maximumBytes) {
        throw "$Label is not a bounded regular non-link source evidence file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length) { throw "$Label changed during admission." }
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try { $hash = (($sha256.ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) -join "") }
        finally { $sha256.Dispose() }
        $script:admittedEvidenceBytes += [long]$stream.Length
        if ($script:admittedEvidenceBytes -gt $script:maximumTotalEvidenceBytes) {
            throw "Row evidence exceeds the aggregate byte admission bound."
        }
        return [pscustomobject]@{ path = $item.FullName; sha256 = $hash; length = [long]$stream.Length }
    } finally {
        $stream.Dispose()
    }
}

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if (@(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label does not exactly match the sealed qualification contract."
    }
}

function Resolve-ManifestArtifact([string]$ManifestPath, [string]$RelativePath, [string]$Label) {
    if ([IO.Path]::IsPathRooted($RelativePath)) { throw "$Label path must be relative." }
    $root = [IO.Path]::GetFullPath((Split-Path -Parent $ManifestPath))
    $resolved = [IO.Path]::GetFullPath((Join-Path $root $RelativePath))
    $prefix = "$($root.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
    $comparison = if ([OperatingSystem]::IsWindows()) {
        [StringComparison]::OrdinalIgnoreCase
    } else {
        [StringComparison]::Ordinal
    }
    if (-not $resolved.StartsWith($prefix, $comparison)) {
        throw "$Label path escapes the artifact manifest directory."
    }
    $cursor = [IO.Path]::GetFullPath((Split-Path -Parent $resolved))
    while ($true) {
        $directory = Get-Item -LiteralPath $cursor -ErrorAction Stop
        if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "$Label path traverses a linked directory."
        }
        if ($cursor -eq $root) { break }
        $cursor = [IO.Path]::GetFullPath((Split-Path -Parent $cursor))
    }
    return $resolved
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$policyAbsolute = Resolve-RepositoryPath $PolicyPath
$runtimeProfileAbsolute = Resolve-RepositoryPath $RuntimeProfilePath
$cellAbsolute = Resolve-RepositoryPath $CellObservationPath
$artifactManifestAbsolute = Resolve-RepositoryPath $ArtifactManifestPath
$buildManifestAbsolute = Resolve-RepositoryPath $BuildManifestPath
$outputAbsolute = Resolve-RepositoryPath $OutputDirectory
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()

$initialHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $initialHeadSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Sealed row qualification requires the exact clean source revision."
}

$policy = Read-BoundedJson $policyAbsolute 1048576 "matrix policy"
if ($policy.runtime_cargo_replay_forbidden -ne $true) {
    throw "Row sealing requires separately approved replay tools and forbids runtime Cargo."
}
$maximumProfileBytes = [long]$policy.limits.maximum_profile_bytes
$maximumArtifactBytes = [long]$policy.limits.maximum_artifact_bytes
$maximumTotalEvidenceBytes = [long]$policy.limits.maximum_total_evidence_bytes
$admittedEvidenceBytes = 0L
$profile = Read-BoundedJson $runtimeProfileAbsolute $maximumProfileBytes "runtime profile"
$cell = Read-BoundedJson $cellAbsolute ([long]$policy.limits.maximum_campaign_bytes) "cell observation"
$manifest = Read-BoundedJson $artifactManifestAbsolute ([long]$policy.limits.maximum_campaign_bytes) "row artifact manifest"
$buildManifest = Read-BoundedJson $buildManifestAbsolute ([long]$policy.limits.maximum_campaign_bytes) "build manifest"
if ($policy.schema_version -ne 1 -or $policy.execution_policy -ne "sealed-required" -or
    $policy.atomic_row_execution_required -ne $true -or $policy.row_execution_is_serial -ne $true -or
    $policy.clean_source_required -ne $true -or $policy.exact_target_artifact_required -ne $true -or
    $policy.executed_runtime_image_required -ne $true -or
    $policy.common_release_candidate_required -ne $true -or
    $policy.common_build_manifest_required -ne $true -or
    $policy.exact_environment_before_after_required -ne $true -or
    $policy.owner_verified_leaf_receipts_required -ne $true -or
    $policy.source_evidence_required -ne $true -or
    $policy.environment_snapshots_required -ne $true -or
    $policy.capability_skip_forbidden -ne $true) {
    throw "Platform matrix policy weakened a mandatory row invariant."
}
if ($profile.schema_version -ne $policy.runtime_profile_schema_version) {
    throw "Runtime profile schema does not match the sealed policy."
}
if ($manifest.schema_version -ne 1 -or [string]$manifest.cell_id -ne [string]$cell.cell_id -or
    [string]$manifest.cell_run_id -ne [string]$cell.cell_run_id -or
    [string]$manifest.source_revision -ne $sourceSha -or [string]$cell.source_revision -ne $sourceSha -or
    [string]$manifest.release_candidate_id -ne [string]$cell.release_candidate_id) {
    throw "Row manifest, observation, source, or run identity is inconsistent."
}
$buildManifestHash = [string]$jsonAdmissions[$buildManifestAbsolute]
if ($buildManifest.schema_version -ne [int]$policy.build_manifest_schema_version -or
    [string]$buildManifest.source_revision -ne $sourceSha -or
    [string]$cell.release_candidate_id -ne [string]$buildManifest.release_candidate_id -or
    [string]$cell.build_manifest_sha256 -ne $buildManifestHash) {
    throw "Cell does not bind the exact release candidate and cross-target build manifest."
}
if ([string]$manifest.build_manifest_sha256 -ne $buildManifestHash) {
    throw "Row artifact manifest does not bind the exact cross-target build manifest."
}
$matchingProfileCells = @($profile.cells | Where-Object { [string]$_.cell_id -eq [string]$cell.cell_id })
if ($matchingProfileCells.Count -ne 1) { throw "Row cell is not declared exactly once by the runtime profile." }
if ([string]$cell.environment_before_sha256 -ne [string]$cell.environment_after_sha256) {
    throw "Platform environment changed during the row run."
}

$matchingBuildArtifacts = @($buildManifest.artifacts | Where-Object {
    [string]$_.platform -eq [string]$cell.product_artifact.platform -and
    [string]$_.target_triple -eq [string]$cell.product_artifact.target_triple -and
    [string]$_.package_kind -eq [string]$cell.product_artifact.package_kind
})
if ($matchingBuildArtifacts.Count -ne 1) {
    throw "Build manifest does not contain exactly one artifact for this row target."
}
$buildArtifact = $matchingBuildArtifacts[0]
$productAbsolute = Resolve-ManifestArtifact $buildManifestAbsolute ([string]$buildArtifact.path) "product artifact"
$runtimeImageAbsolute = Resolve-ManifestArtifact $buildManifestAbsolute ([string]$buildArtifact.runtime_image.path) "product runtime image"
$provenanceAbsolute = Resolve-ManifestArtifact $buildManifestAbsolute ([string]$buildArtifact.build_provenance.path) "build provenance"
$productHash = Get-BoundedArtifactSha256 $productAbsolute "product artifact"
$runtimeImageHash = Get-BoundedArtifactSha256 $runtimeImageAbsolute "product runtime image"
$provenanceHash = Get-BoundedArtifactSha256 $provenanceAbsolute "build provenance"
$provenance = Read-BoundedJson $provenanceAbsolute ([long]$policy.limits.maximum_campaign_bytes) "build provenance"
if ($productHash -ne [string]$manifest.product_artifact.sha256 -or
    $productHash -ne [string]$cell.product_artifact.sha256 -or
    $productHash -ne [string]$buildArtifact.sha256 -or
    $runtimeImageHash -ne [string]$manifest.product_artifact.runtime_image_sha256 -or
    $runtimeImageHash -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    $runtimeImageHash -ne [string]$buildArtifact.runtime_image.sha256 -or
    $provenanceHash -ne [string]$manifest.product_artifact.build_provenance_sha256 -or
    $provenanceHash -ne [string]$cell.product_artifact.build_provenance_sha256 -or
    $provenanceHash -ne [string]$buildArtifact.build_provenance.sha256 -or
    [string]$manifest.product_artifact.target_triple -ne [string]$cell.product_artifact.target_triple -or
    [string]$manifest.product_artifact.package_kind -ne [string]$cell.product_artifact.package_kind -or
    [string]$manifest.product_artifact.platform -ne [string]$cell.product_artifact.platform -or
    $provenance.schema_version -ne 1 -or
    [string]$provenance.source_revision -ne $sourceSha -or
    [string]$provenance.release_candidate_id -ne [string]$cell.release_candidate_id -or
    [string]$provenance.target_triple -ne [string]$cell.product_artifact.target_triple -or
    [string]$provenance.package_kind -ne [string]$cell.product_artifact.package_kind -or
    [string]$provenance.product_artifact_sha256 -ne $productHash -or
    [string]$provenance.runtime_image_sha256 -ne $runtimeImageHash) {
    throw "Product artifact hash does not match the row manifest."
}
$requiredKinds = @($policy.required_evidence_per_cell | ForEach-Object { [string]$_ })
Assert-ExactStringSet $requiredKinds @($cell.reports | ForEach-Object { [string]$_.kind }) "cell receipt set"
Assert-ExactStringSet $requiredKinds @($manifest.artifacts | ForEach-Object { [string]$_.kind }) "row artifact set"
$requiredOwnerContracts = @($policy.required_evidence_owners | ForEach-Object {
    "$($_.kind)/$($_.owner)/$($_.verifier_id)/$($_.report_schema_version)"
})
Assert-ExactStringSet $requiredOwnerContracts @($cell.reports | ForEach-Object {
    "$($_.kind)/$($_.owner)/$($_.verifier_id)/$($_.report_schema_version)"
}) "row evidence owner contract"
Assert-ExactStringSet $requiredOwnerContracts @($matchingProfileCells[0].required_evidence | ForEach-Object {
    "$($_.kind)/$($_.owner)/$($_.verifier_id)/$($_.report_schema_version)"
}) "runtime profile evidence owner contract"
$machinePath = Resolve-ManifestArtifact $artifactManifestAbsolute ([string]$manifest.machine_report.path) "machine report"
if ((Get-BoundedArtifactSha256 $machinePath "machine report") -ne [string]$manifest.machine_report.sha256 -or
    [string]$manifest.machine_report.sha256 -ne [string]$cell.machine_report_sha256) {
    throw "Machine report bytes are not bound to the row observation."
}
$sourceContracts = $policy.source_evidence_contracts
if ($sourceContracts.schema_version -ne 1 -or
    [string]$sourceContracts.source_role_pattern -ne '^[a-z0-9][a-z0-9._-]{0,95}$') {
    throw "Source-evidence policy is not the supported closed schema."
}
$environmentEntries = @(
    [pscustomobject]@{ phase = "before"; value = $manifest.environment_snapshots.before }
    [pscustomobject]@{ phase = "after"; value = $manifest.environment_snapshots.after }
)
$sourcePaths = [System.Collections.Generic.HashSet[string]]::new(
    $(if ([OperatingSystem]::IsWindows()) { [StringComparer]::OrdinalIgnoreCase } else { [StringComparer]::Ordinal })
)
$admittedSourceFiles = [System.Collections.Generic.List[object]]::new()
foreach ($snapshotEntry in $environmentEntries) {
    $snapshotPath = Resolve-ManifestArtifact $artifactManifestAbsolute ([string]$snapshotEntry.value.path) `
        "environment $($snapshotEntry.phase) snapshot"
    $snapshotFile = Get-BoundedSourceEvidence $snapshotPath "environment $($snapshotEntry.phase) snapshot"
    if ($snapshotFile.sha256 -ne [string]$snapshotEntry.value.sha256 -or
        $snapshotFile.length -ne [long]$snapshotEntry.value.byte_length -or
        -not $sourcePaths.Add($snapshotFile.path)) {
        throw "Environment snapshot '$($snapshotEntry.phase)' is not uniquely hash-bound."
    }
    $admittedSourceFiles.Add($snapshotFile)
}
foreach ($receipt in @($cell.reports)) {
    $matches = @($manifest.artifacts | Where-Object { [string]$_.kind -eq [string]$receipt.kind })
    if ($matches.Count -ne 1) { throw "Evidence kind '$($receipt.kind)' is not unique." }
    $entry = $matches[0]
    $sourceOwner = @($sourceContracts.owners | Where-Object { [string]$_.kind -eq [string]$receipt.kind })
    if ($sourceOwner.Count -ne 1 -or $entry.source_evidence.schema_version -ne 1 -or
        [string]$entry.source_evidence.source_verifier_id -ne [string]$sourceOwner[0].source_verifier_id -or
        [string]::IsNullOrWhiteSpace([string]$entry.source_evidence.capture_id)) {
        throw "Evidence lane '$($receipt.kind)' has no exact source verifier contract."
    }
    $sourceBindings = $entry.source_evidence.bindings
    if ([string]$sourceBindings.cell_id -ne [string]$cell.cell_id -or
        [string]$sourceBindings.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$sourceBindings.source_revision -ne $sourceSha -or
        [string]$sourceBindings.release_candidate_id -ne [string]$cell.release_candidate_id -or
        [string]$sourceBindings.build_manifest_sha256 -ne $buildManifestHash -or
        [string]$sourceBindings.build_provenance_sha256 -ne $provenanceHash -or
        [string]$sourceBindings.product_artifact_sha256 -ne $productHash -or
        [string]$sourceBindings.runtime_image_sha256 -ne $runtimeImageHash -or
        [string]$sourceBindings.machine_report_sha256 -ne [string]$cell.machine_report_sha256 -or
        [string]$sourceBindings.environment_sha256 -ne [string]$receipt.environment_sha256) {
        throw "Evidence lane '$($receipt.kind)' source capture is not atomically bound to this row."
    }
    $sourceEntries = @($entry.source_evidence.entries)
    if ($sourceEntries.Count -eq 0 -or
        @($sourceEntries | ForEach-Object { [string]$_.role } | Sort-Object -Unique).Count -ne $sourceEntries.Count) {
        throw "Evidence lane '$($receipt.kind)' source roles are empty or duplicated."
    }
    $laneSourceBytes = 0L
    foreach ($sourceEntry in $sourceEntries) {
        if ([string]$sourceEntry.role -notmatch [string]$sourceContracts.source_role_pattern -or
            [string]$sourceEntry.format -notin @("json", "jsonl", "text", "binary")) {
            throw "Evidence lane '$($receipt.kind)' has an invalid source role or format."
        }
        $sourcePath = Resolve-ManifestArtifact $artifactManifestAbsolute ([string]$sourceEntry.path) `
            "lane '$($receipt.kind)' source '$($sourceEntry.role)'"
        $sourceFile = Get-BoundedSourceEvidence $sourcePath "lane '$($receipt.kind)' source '$($sourceEntry.role)'"
        $laneSourceBytes += $sourceFile.length
        if ($sourceFile.sha256 -ne [string]$sourceEntry.sha256 -or
            $sourceFile.length -ne [long]$sourceEntry.byte_length -or
            -not $sourcePaths.Add($sourceFile.path)) {
            throw "Evidence lane '$($receipt.kind)' source '$($sourceEntry.role)' is not uniquely hash-bound."
        }
        $admittedSourceFiles.Add($sourceFile)
    }
    if ($laneSourceBytes -gt [long]$policy.limits.maximum_source_evidence_lane_bytes) {
        throw "Evidence lane '$($receipt.kind)' exceeds its source-evidence byte bound."
    }
    $reportPath = Resolve-ManifestArtifact $artifactManifestAbsolute ([string]$entry.report_path) "lane report"
    $rawPath = Resolve-ManifestArtifact $artifactManifestAbsolute ([string]$entry.raw_evidence_path) "raw evidence"
    $laneReport = Read-BoundedJson $reportPath ([long]$policy.limits.maximum_campaign_bytes) "owner-verified lane report"
    if ((Get-BoundedArtifactSha256 $reportPath "lane report") -ne [string]$receipt.report_sha256 -or
        (Get-BoundedArtifactSha256 $rawPath "raw evidence") -ne [string]$receipt.raw_evidence_sha256 -or
        [string]$entry.report_sha256 -ne [string]$receipt.report_sha256 -or
        [string]$entry.raw_evidence_sha256 -ne [string]$receipt.raw_evidence_sha256 -or
        [int]$laneReport.schema_version -ne [int]$receipt.report_schema_version -or
        [string]$laneReport.kind -ne [string]$receipt.kind -or
        [string]$laneReport.owner -ne [string]$receipt.owner -or
        [string]$laneReport.verifier_id -ne [string]$receipt.verifier_id -or
        [string]$laneReport.status -ne [string]$receipt.status -or
        [string]$laneReport.profile_sha256 -ne [string]$receipt.profile_sha256 -or
        [string]$laneReport.raw_evidence_sha256 -ne [string]$receipt.raw_evidence_sha256 -or
        [string]$laneReport.environment_sha256 -ne [string]$receipt.environment_sha256 -or
        [string]$receipt.source_revision -ne $sourceSha -or
        [string]$laneReport.source_revision -ne $sourceSha -or
        [string]$receipt.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$laneReport.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$receipt.machine_report_sha256 -ne [string]$cell.machine_report_sha256 -or
        [string]$laneReport.machine_report_sha256 -ne [string]$cell.machine_report_sha256 -or
        [string]$laneReport.product_artifact_sha256 -ne $productHash -or
        [string]$receipt.product_artifact_sha256 -ne $productHash -or
        [string]$laneReport.runtime_image_sha256 -ne $runtimeImageHash -or
        [string]$receipt.runtime_image_sha256 -ne $runtimeImageHash -or
        [string]$laneReport.release_candidate_id -ne [string]$cell.release_candidate_id -or
        [string]$receipt.release_candidate_id -ne [string]$cell.release_candidate_id -or
        [string]$laneReport.build_manifest_sha256 -ne $buildManifestHash -or
        [string]$receipt.build_manifest_sha256 -ne $buildManifestHash -or
        [string]$laneReport.build_provenance_sha256 -ne $provenanceHash -or
        [string]$receipt.build_provenance_sha256 -ne $provenanceHash) {
        throw "Evidence lane '$($receipt.kind)' is not atomically bound to this row."
    }
    $ownerVerifier = Resolve-RepositoryPath ([string]$policy.owner_verifier_script)
    & $ownerVerifier `
        -ExpectedKind ([string]$receipt.kind) `
        -ReportPath $reportPath -RawEvidencePath $rawPath `
        -CellObservationPath $cellAbsolute -RuntimeProfilePath $runtimeProfileAbsolute `
        -BuildManifestPath $buildManifestAbsolute -MachineReportPath $machinePath `
        -ArtifactManifestPath $artifactManifestAbsolute -PolicyPath $policyAbsolute
}

$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha) { throw "Checked-out source does not match the row source SHA." }
if (@(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Sealed row qualification requires a clean checkout."
}
$repositoryPrefix = "$($repositoryRoot.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
$pathComparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
if ($outputAbsolute -eq $repositoryRoot -or $outputAbsolute.StartsWith($repositoryPrefix, $pathComparison)) {
    throw "Sealed row output must be outside the source repository."
}
if (Test-Path -LiteralPath $outputAbsolute) { throw "Row output directory already exists: $outputAbsolute" }
New-Item -ItemType Directory -Path $outputAbsolute -ErrorAction Stop | Out-Null

$sealed = [ordered]@{
    schema_version = 1
    status = "sealed_for_matrix_resolution"
    source_sha = $sourceSha
    cell_id = [string]$cell.cell_id
    cell_run_id = [string]$cell.cell_run_id
    policy_sha256 = [string]$jsonAdmissions[$policyAbsolute]
    runtime_profile_file_sha256 = [string]$jsonAdmissions[$runtimeProfileAbsolute]
    cell_observation_sha256 = [string]$jsonAdmissions[$cellAbsolute]
    artifact_manifest_sha256 = [string]$jsonAdmissions[$artifactManifestAbsolute]
    product_artifact_sha256 = $productHash
    runtime_image_sha256 = $runtimeImageHash
    release_candidate_id = [string]$cell.release_candidate_id
    build_manifest_sha256 = $buildManifestHash
    target_triple = [string]$cell.product_artifact.target_triple
    package_kind = [string]$cell.product_artifact.package_kind
    platform = [string]$cell.product_artifact.platform
    build_provenance_sha256 = $provenanceHash
    machine_report_sha256 = [string]$cell.machine_report_sha256
}
$endHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $endHeadSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Source changed or became dirty while sealing the row."
}
foreach ($path in @($policyAbsolute, $runtimeProfileAbsolute, $cellAbsolute, $artifactManifestAbsolute, $buildManifestAbsolute, $provenanceAbsolute)) {
    if ((Get-LowerSha256 $path) -ne [string]$jsonAdmissions[$path]) {
        throw "A sealed row authority or provenance JSON changed during execution."
    }
}
if ((Get-LowerSha256 $productAbsolute) -ne $productHash -or
    (Get-LowerSha256 $runtimeImageAbsolute) -ne $runtimeImageHash) {
    throw "The product package or executed runtime image changed during row execution."
}
foreach ($sourceFile in $admittedSourceFiles) {
    $item = Get-Item -LiteralPath ([string]$sourceFile.path) -ErrorAction Stop
    if ($item.Length -ne [long]$sourceFile.length -or
        (Get-LowerSha256 ([string]$sourceFile.path)) -ne [string]$sourceFile.sha256) {
        throw "A source-evidence file changed during row execution."
    }
}
Write-CreateOnlyJson (Join-Path $outputAbsolute "sealed-row.json") $sealed 8
