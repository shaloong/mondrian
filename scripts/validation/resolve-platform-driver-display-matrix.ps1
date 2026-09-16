param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CampaignPath,
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

function Resolve-ContainedPath([string]$ManifestPath, [string]$RelativePath, [string]$Label) {
    if ([IO.Path]::IsPathRooted($RelativePath)) { throw "$Label path must be relative." }
    $root = [IO.Path]::GetFullPath((Split-Path -Parent $ManifestPath))
    $resolved = [IO.Path]::GetFullPath((Join-Path $root $RelativePath))
    $prefix = "$($root.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
    $comparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if (-not $resolved.StartsWith($prefix, $comparison)) { throw "$Label path escapes its manifest directory." }
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

function Get-BoundedFile([string]$Path, [long]$MaximumBytes, [string]$Label, [bool]$AllowEmpty = $false) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        (-not $AllowEmpty -and $item.Length -le 0) -or $item.Length -gt $MaximumBytes) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length) { throw "$Label changed during admission." }
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try { $hash = (($sha256.ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) -join "") }
        finally { $sha256.Dispose() }
        return [pscustomobject]@{ path = $item.FullName; sha256 = $hash; length = [long]$stream.Length }
    } finally {
        $stream.Dispose()
    }
}

function Read-BoundedJson([string]$Path, [long]$MaximumBytes, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $MaximumBytes) {
        throw "$Label is not a bounded regular non-link file: $Path"
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

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if ($expectedValues.Count -ne @($Expected).Count -or $actualValues.Count -ne @($Actual).Count -or
        @(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label does not exactly match the sealed qualification contract."
    }
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

function Add-Evidence([string]$Path, [string]$Role, [long]$MaximumBytes) {
    $file = Get-BoundedFile $Path $MaximumBytes $Role
    if (-not $script:evidenceByHash.ContainsKey($file.sha256)) {
        $script:admittedEvidenceBytes += $file.length
        if ($script:admittedEvidenceBytes -gt $script:maximumTotalEvidenceBytes) {
            throw "Matrix evidence exceeds the aggregate byte admission bound."
        }
        $safeName = [IO.Path]::GetFileName($file.path) -replace '[^A-Za-z0-9._-]', '_'
        $script:evidenceByHash[$file.sha256] = [pscustomobject]@{
            source_path = $file.path
            sha256 = $file.sha256
            byte_length = $file.length
            bundle_path = "evidence/$($file.sha256)-$safeName"
            roles = [System.Collections.Generic.List[string]]::new()
        }
    }
    $entry = $script:evidenceByHash[$file.sha256]
    if (-not $entry.roles.Contains($Role)) { $entry.roles.Add($Role) }
    return $file
}

function Invoke-BoundedReplay([string[]]$Arguments, [int]$TimeoutSeconds, [string]$StdoutPath, [string]$StderrPath) {
    $executable = [Environment]::GetEnvironmentVariable("MONDRIAN_PLATFORM_QUALIFICATION_REPLAY_EXECUTABLE", "Process")
    $expectedSha = [Environment]::GetEnvironmentVariable("MONDRIAN_PLATFORM_QUALIFICATION_REPLAY_SHA256", "Process")
    if ([string]::IsNullOrWhiteSpace($executable) -or $expectedSha -notmatch '^[0-9a-f]{64}$') {
        throw "Platform Matrix replay has no separately approved executable identity."
    }
    $tool = Get-BoundedFile $executable 8589934592 "approved platform Matrix replay"
    if ($tool.sha256 -ne $expectedSha) { throw "Platform Matrix replay differs from its approved SHA-256." }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $tool.path
    $start.WorkingDirectory = $script:repositoryRoot
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    foreach ($argument in $Arguments) { $null = $start.ArgumentList.Add($argument) }
    $stdout = [IO.File]::Open($StdoutPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $stderr = [IO.File]::Open($StderrPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { throw "Could not start platform Matrix replay." }
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderr)
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { $null = $process.WaitForExit(1000) }
        if (-not $process.HasExited) {
            try { $process.Kill($true); $process.WaitForExit() } catch { }
            throw "Platform matrix evaluation exceeded its $TimeoutSeconds second deadline."
        }
        $process.WaitForExit()
        $stdoutCopy.GetAwaiter().GetResult()
        $stderrCopy.GetAwaiter().GetResult()
        $stdout.Flush($true); $stderr.Flush($true)
        return $process.ExitCode
    } finally {
        $process.Dispose(); $stdout.Dispose(); $stderr.Dispose()
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$policyAbsolute = Resolve-RepositoryPath $PolicyPath
$profileAbsolute = Resolve-RepositoryPath $RuntimeProfilePath
$campaignAbsolute = Resolve-RepositoryPath $CampaignPath
$matrixManifestAbsolute = Resolve-RepositoryPath $ArtifactManifestPath
$buildManifestAbsolute = Resolve-RepositoryPath $BuildManifestPath
$outputAbsolute = Resolve-RepositoryPath $OutputDirectory
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()

$policy = Read-BoundedJson $policyAbsolute 1048576 "matrix policy"
if ($policy.runtime_cargo_replay_forbidden -ne $true -or
    [string]$policy.runner.verifier_tool_id -ne "platform_qualification_replay") {
    throw "Matrix resolution requires the standalone approved replay tool and forbids runtime Cargo."
}
$maximumProfileBytes = [long]$policy.limits.maximum_profile_bytes
$maximumCampaignBytes = [long]$policy.limits.maximum_campaign_bytes
$maximumArtifactBytes = [long]$policy.limits.maximum_artifact_bytes
$maximumTotalEvidenceBytes = [long]$policy.limits.maximum_total_evidence_bytes
$admittedEvidenceBytes = 0L
$evidenceByHash = @{}
$profile = Read-BoundedJson $profileAbsolute $maximumProfileBytes "runtime profile"
$campaign = Read-BoundedJson $campaignAbsolute $maximumCampaignBytes "campaign envelope"
$matrixManifest = Read-BoundedJson $matrixManifestAbsolute $maximumCampaignBytes "matrix artifact manifest"
$buildManifest = Read-BoundedJson $buildManifestAbsolute $maximumCampaignBytes "build manifest"
$policyFile = Add-Evidence $policyAbsolute "policy" 1048576
$profileFile = Add-Evidence $profileAbsolute "runtime-profile" $maximumProfileBytes
$campaignFile = Add-Evidence $campaignAbsolute "campaign-envelope" $maximumCampaignBytes
$matrixManifestFile = Add-Evidence $matrixManifestAbsolute "matrix-artifact-manifest" $maximumCampaignBytes
$buildManifestFile = Add-Evidence $buildManifestAbsolute "build-manifest" $maximumCampaignBytes
foreach ($authority in @(
    [pscustomobject]@{ path = $policyAbsolute; file = $policyFile }
    [pscustomobject]@{ path = $profileAbsolute; file = $profileFile }
    [pscustomobject]@{ path = $campaignAbsolute; file = $campaignFile }
    [pscustomobject]@{ path = $matrixManifestAbsolute; file = $matrixManifestFile }
    [pscustomobject]@{ path = $buildManifestAbsolute; file = $buildManifestFile }
)) {
    if ([string]$jsonAdmissions[[string]$authority.path] -ne [string]$authority.file.sha256) {
        throw "A matrix authority JSON changed during admission."
    }
}

if ($policy.schema_version -ne 1 -or $policy.execution_policy -ne "sealed-required" -or
    $policy.cross_machine_row_aggregation_required -ne $true -or
    $policy.atomic_row_execution_required -ne $true -or $policy.row_execution_is_serial -ne $true -or
    $policy.clean_source_required -ne $true -or $policy.exact_target_artifact_required -ne $true -or
    $policy.executed_runtime_image_required -ne $true -or
    $policy.common_release_candidate_required -ne $true -or $policy.common_build_manifest_required -ne $true -or
    $policy.owner_verified_leaf_receipts_required -ne $true -or $policy.source_evidence_required -ne $true -or
    $policy.environment_snapshots_required -ne $true -or $policy.capability_skip_forbidden -ne $true) {
    throw "Platform matrix policy weakened a mandatory aggregate invariant."
}
if ($profile.schema_version -ne $policy.runtime_profile_schema_version -or
    $campaign.schema_version -ne 1 -or $matrixManifest.schema_version -ne 1 -or
    $buildManifest.schema_version -ne [int]$policy.build_manifest_schema_version -or [string]$campaign.source_revision -ne $sourceSha -or
    [string]$matrixManifest.source_revision -ne $sourceSha -or [string]$buildManifest.source_revision -ne $sourceSha -or
    [string]$campaign.release_candidate_id -ne [string]$matrixManifest.release_candidate_id -or
    [string]$campaign.release_candidate_id -ne [string]$buildManifest.release_candidate_id -or
    [string]$campaign.build_manifest_sha256 -ne $buildManifestFile.sha256 -or
    [string]$matrixManifest.build_manifest_sha256 -ne $buildManifestFile.sha256) {
    throw "Campaign, profile, matrix manifest, and build manifest are not one exact release candidate."
}
$initialHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $initialHeadSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Sealed platform matrix qualification requires the exact clean source revision."
}

Assert-ExactStringSet @($policy.required_platform_backends | ForEach-Object { "$($_.platform)/$($_.backend)" }) `
    @($profile.cells | ForEach-Object { "$($_.environment.platform)/$($_.environment.graphics_backend)" } | Sort-Object -Unique) `
    "platform/backend union"
foreach ($platform in @("windows", "mac_os", "linux")) {
    $scenarioUnion = @($profile.cells | Where-Object { [string]$_.environment.platform -eq $platform } |
        ForEach-Object { $_.required_scenarios } | ForEach-Object { [string]$_.scenario } | Sort-Object -Unique)
    Assert-ExactStringSet @($policy.required_scenarios_per_platform) $scenarioUnion "$platform scenario union"
}
$profileCellIds = @($profile.cells | ForEach-Object { [string]$_.cell_id })
$sealedCellIds = @($matrixManifest.sealed_rows | ForEach-Object { [string]$_.cell_id })
if ($sealedCellIds.Count -gt [int]$policy.limits.maximum_cells) { throw "Matrix exceeds its cell admission bound." }
Assert-ExactStringSet $profileCellIds $sealedCellIds "matrix cell closure"

$resolvedCells = [System.Collections.Generic.List[object]]::new()
$usedBuildArtifactKeys = [System.Collections.Generic.List[string]]::new()
$sourceEvidencePaths = [System.Collections.Generic.HashSet[string]]::new(
    $(if ([OperatingSystem]::IsWindows()) { [StringComparer]::OrdinalIgnoreCase } else { [StringComparer]::Ordinal })
)
foreach ($row in @($matrixManifest.sealed_rows)) {
    $cellId = [string]$row.cell_id
    $sealedPath = Resolve-ContainedPath $matrixManifestAbsolute ([string]$row.sealed_row_path) "sealed row"
    $cellPath = Resolve-ContainedPath $matrixManifestAbsolute ([string]$row.cell_observation_path) "cell observation"
    $rowManifestPath = Resolve-ContainedPath $matrixManifestAbsolute ([string]$row.row_artifact_manifest_path) "row artifact manifest"
    $sealedFile = Add-Evidence $sealedPath "row:$cellId:seal" $maximumArtifactBytes
    $cellFile = Add-Evidence $cellPath "row:$cellId:observation" $maximumArtifactBytes
    $rowManifestFile = Add-Evidence $rowManifestPath "row:$cellId:artifact-manifest" $maximumArtifactBytes
    if ($sealedFile.sha256 -ne [string]$row.sealed_row_sha256 -or
        $cellFile.sha256 -ne [string]$row.cell_observation_sha256 -or
        $rowManifestFile.sha256 -ne [string]$row.row_artifact_manifest_sha256) {
        throw "Matrix manifest hash mismatch for row '$cellId'."
    }
    $sealed = Read-BoundedJson $sealedPath $maximumArtifactBytes "sealed row"
    $cell = Read-BoundedJson $cellPath $maximumArtifactBytes "cell observation"
    $rowManifest = Read-BoundedJson $rowManifestPath $maximumArtifactBytes "row artifact manifest"
    if ([string]$jsonAdmissions[$sealedPath] -ne $sealedFile.sha256 -or
        [string]$jsonAdmissions[$cellPath] -ne $cellFile.sha256 -or
        [string]$jsonAdmissions[$rowManifestPath] -ne $rowManifestFile.sha256) {
        throw "Row '$cellId' JSON changed during admission."
    }
    if ($sealed.schema_version -ne 1 -or [string]$sealed.status -ne "sealed_for_matrix_resolution" -or
        [string]$sealed.cell_id -ne $cellId -or [string]$cell.cell_id -ne $cellId -or
        [string]$rowManifest.cell_id -ne $cellId -or [string]$sealed.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$rowManifest.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$sealed.source_sha -ne $sourceSha -or [string]$cell.source_revision -ne $sourceSha -or
        [string]$sealed.cell_observation_sha256 -ne $cellFile.sha256 -or
        [string]$sealed.artifact_manifest_sha256 -ne $rowManifestFile.sha256 -or
        [string]$sealed.release_candidate_id -ne [string]$campaign.release_candidate_id -or
        [string]$cell.release_candidate_id -ne [string]$campaign.release_candidate_id -or
        [string]$sealed.build_manifest_sha256 -ne $buildManifestFile.sha256 -or
        [string]$cell.build_manifest_sha256 -ne $buildManifestFile.sha256 -or
        [string]$rowManifest.build_manifest_sha256 -ne $buildManifestFile.sha256 -or
        [string]$sealed.platform -ne [string]$cell.product_artifact.platform -or
        [string]$sealed.target_triple -ne [string]$cell.product_artifact.target_triple -or
        [string]$sealed.package_kind -ne [string]$cell.product_artifact.package_kind) {
        throw "Sealed row '$cellId' is not atomically bound to the campaign."
    }

    $buildArtifacts = @($buildManifest.artifacts | Where-Object {
        [string]$_.platform -eq [string]$cell.product_artifact.platform -and
        [string]$_.target_triple -eq [string]$cell.product_artifact.target_triple -and
        [string]$_.package_kind -eq [string]$cell.product_artifact.package_kind
    })
    if ($buildArtifacts.Count -ne 1) { throw "Row '$cellId' has no unique target artifact in the build manifest." }
    $buildArtifact = $buildArtifacts[0]
    $usedBuildArtifactKeys.Add("$($buildArtifact.platform)/$($buildArtifact.target_triple)/$($buildArtifact.package_kind)")
    $productPath = Resolve-ContainedPath $buildManifestAbsolute ([string]$buildArtifact.path) "product artifact"
    $runtimeImagePath = Resolve-ContainedPath $buildManifestAbsolute ([string]$buildArtifact.runtime_image.path) "product runtime image"
    $provenancePath = Resolve-ContainedPath $buildManifestAbsolute ([string]$buildArtifact.build_provenance.path) "build provenance"
    $productFile = Add-Evidence $productPath "row:$cellId:product-artifact" $maximumArtifactBytes
    $runtimeImageFile = Add-Evidence $runtimeImagePath "row:$cellId:runtime-image" $maximumArtifactBytes
    $provenanceFile = Add-Evidence $provenancePath "row:$cellId:build-provenance" $maximumArtifactBytes
    $provenance = Read-BoundedJson $provenancePath $maximumArtifactBytes "build provenance"
    if ([string]$jsonAdmissions[$provenancePath] -ne $provenanceFile.sha256) {
        throw "Build provenance changed during admission for row '$cellId'."
    }
    if ($productFile.sha256 -ne [string]$cell.product_artifact.sha256 -or
        $productFile.sha256 -ne [string]$sealed.product_artifact_sha256 -or
        $productFile.sha256 -ne [string]$buildArtifact.sha256 -or
        $runtimeImageFile.sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
        $runtimeImageFile.sha256 -ne [string]$sealed.runtime_image_sha256 -or
        $runtimeImageFile.sha256 -ne [string]$buildArtifact.runtime_image.sha256 -or
        $provenanceFile.sha256 -ne [string]$cell.product_artifact.build_provenance_sha256 -or
        $provenanceFile.sha256 -ne [string]$sealed.build_provenance_sha256 -or
        $provenanceFile.sha256 -ne [string]$buildArtifact.build_provenance.sha256 -or
        $provenance.schema_version -ne 1 -or [string]$provenance.source_revision -ne $sourceSha -or
        [string]$provenance.release_candidate_id -ne [string]$campaign.release_candidate_id -or
        [string]$provenance.target_triple -ne [string]$cell.product_artifact.target_triple -or
        [string]$provenance.package_kind -ne [string]$cell.product_artifact.package_kind -or
        [string]$provenance.product_artifact_sha256 -ne $productFile.sha256 -or
        [string]$provenance.runtime_image_sha256 -ne $runtimeImageFile.sha256) {
        throw "Target artifact or build provenance mismatch for row '$cellId'."
    }

    $requiredContracts = @($policy.required_evidence_owners | ForEach-Object {
        "$($_.kind)/$($_.owner)/$($_.verifier_id)/$($_.report_schema_version)"
    })
    Assert-ExactStringSet $requiredContracts @($cell.reports | ForEach-Object {
        "$($_.kind)/$($_.owner)/$($_.verifier_id)/$($_.report_schema_version)"
    }) "row '$cellId' evidence contracts"
    $machinePath = Resolve-ContainedPath $rowManifestPath ([string]$rowManifest.machine_report.path) "machine report"
    $machineFile = Add-Evidence $machinePath "row:$cellId:machine-report" $maximumArtifactBytes
    if ($machineFile.sha256 -ne [string]$cell.machine_report_sha256 -or
        $machineFile.sha256 -ne [string]$rowManifest.machine_report.sha256) {
        throw "Machine report mismatch for row '$cellId'."
    }
    foreach ($phase in @("before", "after")) {
        $snapshotEntry = $rowManifest.environment_snapshots.$phase
        $snapshotPath = Resolve-ContainedPath $rowManifestPath ([string]$snapshotEntry.path) `
            "environment $phase snapshot"
        $snapshotFile = Add-Evidence $snapshotPath "row:${cellId}:environment-$phase" `
            ([long]$policy.limits.maximum_source_evidence_file_bytes)
        if ($snapshotFile.sha256 -ne [string]$snapshotEntry.sha256 -or
            $snapshotFile.length -ne [long]$snapshotEntry.byte_length -or
            -not $sourceEvidencePaths.Add($snapshotFile.path)) {
            throw "Environment '$phase' snapshot is not uniquely bound for row '$cellId'."
        }
    }
    foreach ($receipt in @($cell.reports)) {
        $artifact = @($rowManifest.artifacts | Where-Object { [string]$_.kind -eq [string]$receipt.kind })
        if ($artifact.Count -ne 1) { throw "Row '$cellId' lane '$($receipt.kind)' is not unique." }
        $reportPath = Resolve-ContainedPath $rowManifestPath ([string]$artifact[0].report_path) "lane report"
        $rawPath = Resolve-ContainedPath $rowManifestPath ([string]$artifact[0].raw_evidence_path) "raw evidence"
        $reportFile = Add-Evidence $reportPath "row:${cellId}:$($receipt.kind):report" $maximumArtifactBytes
        $rawFile = Add-Evidence $rawPath "row:${cellId}:$($receipt.kind):raw" $maximumArtifactBytes
        if ($reportFile.sha256 -ne [string]$receipt.report_sha256 -or
            $rawFile.sha256 -ne [string]$receipt.raw_evidence_sha256 -or
            [string]$artifact[0].report_sha256 -ne [string]$receipt.report_sha256 -or
            [string]$artifact[0].raw_evidence_sha256 -ne [string]$receipt.raw_evidence_sha256) {
            throw "Row '$cellId' lane '$($receipt.kind)' hash mismatch."
        }
        $sourceEntries = @($artifact[0].source_evidence.entries)
        $laneSourceBytes = 0L
        foreach ($sourceEntry in $sourceEntries) {
            $sourcePath = Resolve-ContainedPath $rowManifestPath ([string]$sourceEntry.path) `
                "row '$cellId' lane '$($receipt.kind)' source '$($sourceEntry.role)'"
            $sourceFile = Add-Evidence $sourcePath `
                "row:${cellId}:$($receipt.kind):source:$($sourceEntry.role)" `
                ([long]$policy.limits.maximum_source_evidence_file_bytes)
            $laneSourceBytes += $sourceFile.length
            if ([string]$sourceEntry.role -notmatch [string]$policy.source_evidence_contracts.source_role_pattern -or
                $sourceFile.sha256 -ne [string]$sourceEntry.sha256 -or
                $sourceFile.length -ne [long]$sourceEntry.byte_length -or
                -not $sourceEvidencePaths.Add($sourceFile.path)) {
                throw "Row '$cellId' lane '$($receipt.kind)' source evidence is not uniquely bound."
            }
        }
        if ($laneSourceBytes -gt [long]$policy.limits.maximum_source_evidence_lane_bytes) {
            throw "Row '$cellId' lane '$($receipt.kind)' exceeds its source-evidence byte bound."
        }
        $ownerVerifier = Resolve-RepositoryPath ([string]$policy.owner_verifier_script)
        & $ownerVerifier `
            -ExpectedKind ([string]$receipt.kind) `
            -ReportPath $reportPath -RawEvidencePath $rawPath -CellObservationPath $cellPath `
            -RuntimeProfilePath $profileAbsolute -BuildManifestPath $buildManifestAbsolute `
            -MachineReportPath $machinePath -ArtifactManifestPath $rowManifestPath `
            -PolicyPath $policyAbsolute
    }
    $resolvedCells.Add($cell)
}
$buildArtifactKeys = @($buildManifest.artifacts | ForEach-Object {
    "$($_.platform)/$($_.target_triple)/$($_.package_kind)"
})
Assert-ExactStringSet $buildArtifactKeys @($usedBuildArtifactKeys | Sort-Object -Unique) `
    "cross-target build artifact closure"

$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Sealed platform matrix qualification requires the exact clean source revision."
}
$repositoryPrefix = "$($repositoryRoot.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
$comparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
if ($outputAbsolute -eq $repositoryRoot -or $outputAbsolute.StartsWith($repositoryPrefix, $comparison)) {
    throw "Sealed matrix output must be outside the source repository."
}
if (Test-Path -LiteralPath $outputAbsolute) { throw "Matrix output directory already exists: $outputAbsolute" }
New-Item -ItemType Directory -Path $outputAbsolute -ErrorAction Stop | Out-Null

$resolvedCampaignPath = Join-Path $outputAbsolute "resolved-campaign.json"
$reportPath = Join-Path $outputAbsolute "qualification-report.json"
$stdoutPath = Join-Path $outputAbsolute "qualification.stdout.log"
$stderrPath = Join-Path $outputAbsolute "qualification.stderr.log"
$resolvedCampaign = [ordered]@{
    campaign_id = [string]$campaign.campaign_id
    source_revision = $sourceSha
    release_candidate_id = [string]$campaign.release_candidate_id
    build_manifest_sha256 = $buildManifestFile.sha256
    cells = @($resolvedCells)
}
Write-CreateOnlyJson $resolvedCampaignPath $resolvedCampaign 32

$exitCode = Invoke-BoundedReplay @($profileAbsolute, $resolvedCampaignPath, $reportPath) `
    ([int]$policy.runner.timeout_seconds) $stdoutPath $stderrPath
if ($exitCode -ne 0) { throw "Platform matrix qualification failed with exit code $exitCode." }
$qualification = Read-BoundedJson $reportPath $maximumCampaignBytes "qualification report"
if ($qualification.schema_version -ne [int]$policy.runner.report_schema_version -or
    [string]$qualification.status -ne "qualified" -or @($qualification.missing_cells).Count -ne 0 -or
    [string]$qualification.source_revision -ne $sourceSha -or
    [string]$qualification.release_candidate_id -ne [string]$campaign.release_candidate_id -or
    [string]$qualification.build_manifest_sha256 -ne $buildManifestFile.sha256 -or
    [string]::IsNullOrWhiteSpace([string]$qualification.profile_sha256) -or
    [string]::IsNullOrWhiteSpace([string]$qualification.evidence_sha256)) {
    throw "Platform matrix report is not complete, qualified, and release-bound."
}

$evidenceDirectory = Join-Path $outputAbsolute "evidence"
New-Item -ItemType Directory -Path $evidenceDirectory -ErrorAction Stop | Out-Null
$closureEntries = [System.Collections.Generic.List[object]]::new()
foreach ($entry in @($evidenceByHash.Values | Sort-Object sha256)) {
    $destination = Join-Path $outputAbsolute ([string]$entry.bundle_path)
    [IO.File]::Copy([string]$entry.source_path, $destination, $false)
    $copy = Get-BoundedFile $destination $maximumArtifactBytes "bundled evidence"
    if ($copy.sha256 -ne [string]$entry.sha256 -or $copy.length -ne [long]$entry.byte_length) {
        throw "Bundled evidence copy failed readback verification."
    }
    $closureEntries.Add([ordered]@{
        sha256 = [string]$entry.sha256
        byte_length = [long]$entry.byte_length
        bundle_path = [string]$entry.bundle_path
        roles = @($entry.roles | Sort-Object)
    })
}
$closurePath = Join-Path $outputAbsolute "evidence-closure.json"
$closure = [ordered]@{
    schema_version = 1
    source_revision = $sourceSha
    release_candidate_id = [string]$campaign.release_candidate_id
    build_manifest_sha256 = $buildManifestFile.sha256
    total_evidence_bytes = [long]$admittedEvidenceBytes
    entries = @($closureEntries)
}
Write-CreateOnlyJson $closurePath $closure 12

$sealedPath = Join-Path $outputAbsolute "sealed-matrix.json"
$sealed = [ordered]@{
    schema_version = 1
    status = "qualified"
    source_revision = $sourceSha
    release_candidate_id = [string]$campaign.release_candidate_id
    build_manifest_sha256 = $buildManifestFile.sha256
    policy_sha256 = $policyFile.sha256
    runtime_profile_file_sha256 = $profileFile.sha256
    campaign_envelope_sha256 = $campaignFile.sha256
    matrix_artifact_manifest_sha256 = $matrixManifestFile.sha256
    resolved_campaign_sha256 = (Get-BoundedFile $resolvedCampaignPath $maximumCampaignBytes "resolved campaign").sha256
    qualification_report_sha256 = (Get-BoundedFile $reportPath $maximumCampaignBytes "qualification report").sha256
    stdout_sha256 = (Get-BoundedFile $stdoutPath $maximumArtifactBytes "qualification stdout" $true).sha256
    stderr_sha256 = (Get-BoundedFile $stderrPath $maximumArtifactBytes "qualification stderr" $true).sha256
    evidence_closure_sha256 = (Get-BoundedFile $closurePath $maximumCampaignBytes "evidence closure").sha256
}
$endHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $endHeadSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Source changed or became dirty during platform matrix resolution."
}
Write-CreateOnlyJson $sealedPath $sealed 8
