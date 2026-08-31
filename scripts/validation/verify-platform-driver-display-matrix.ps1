param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$BundleDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{40}$")][string]$ExpectedSourceSha,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedPolicyPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedRuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedCaptureAuthorityManifestPath,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{64}$")][string]$ExpectedCaptureAuthorityManifestSha256,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedVerifierToolsManifestPath,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{64}$")][string]$ExpectedVerifierToolsManifestSha256,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedReleaseCandidateId,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{64}$")][string]$ExpectedBuildManifestSha256
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$jsonAdmissions = @{}
$anchorAdmissions = [System.Collections.Generic.List[object]]::new()
$anchorReadLocks = [System.Collections.Generic.List[System.IDisposable]]::new()

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Get-BoundedFile([string]$Path, [long]$MaximumBytes, [string]$Label, [bool]$AllowEmpty = $false) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        (-not $AllowEmpty -and $item.Length -le 0) -or $item.Length -gt $MaximumBytes) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length) { throw "$Label changed during verification." }
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try { $hash = (($sha256.ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) -join "") }
        finally { $sha256.Dispose() }
        return [pscustomobject]@{ path = $item.FullName; sha256 = $hash; length = [long]$stream.Length }
    } finally {
        $stream.Dispose()
    }
}

function Read-BoundedJson([string]$Path, [long]$MaximumBytes, [string]$Label, [bool]$RetainBytes = $false) {
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
        $script:jsonAdmissions[$item.FullName] = [pscustomobject]@{
            sha256 = $hash
            length = [long]$bytes.Length
            bytes = if ($RetainBytes) { $bytes } else { $null }
        }
        return $value
    } finally {
        $stream.Dispose()
    }
}

function Write-CreateOnlyBytes([string]$Path, [byte[]]$Bytes) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $stream.Write($Bytes, 0, $Bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
}

function Add-AnchorAdmission([string]$Path, [long]$MaximumBytes, [string]$Label, [bool]$AllowEmpty = $false) {
    $admission = Get-BoundedFile $Path $MaximumBytes $Label $AllowEmpty
    $script:anchorAdmissions.Add([pscustomobject]@{
        path = $admission.path
        sha256 = $admission.sha256
        length = $admission.length
        maximum_bytes = $MaximumBytes
        allow_empty = $AllowEmpty
        label = $Label
    })
    if ($IsWindows) {
        $script:anchorReadLocks.Add(
            [IO.File]::Open($admission.path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        )
    }
}

function Assert-AnchorSnapshotUnchanged {
    foreach ($admission in $script:anchorAdmissions) {
        $current = Get-BoundedFile ([string]$admission.path) ([long]$admission.maximum_bytes) `
            ([string]$admission.label) ([bool]$admission.allow_empty)
        if ($current.sha256 -ne [string]$admission.sha256 -or
            $current.length -ne [long]$admission.length) {
            throw "Private qualification snapshot changed during verification: $($admission.label)."
        }
    }
}

function Resolve-SnapshotPath([string]$SnapshotRoot, [string]$RelativePath, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) {
        throw "$Label must be a repository-relative path."
    }
    $root = [IO.Path]::GetFullPath($SnapshotRoot)
    $resolved = [IO.Path]::GetFullPath((Join-Path $root $RelativePath))
    $prefix = "$($root.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
    $comparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if (-not $resolved.StartsWith($prefix, $comparison)) { throw "$Label escapes the source snapshot." }
    return $resolved
}

function Read-TrustedArchiveEntry(
    [IO.Compression.ZipArchive]$Archive,
    [string]$RelativePath,
    [long]$MaximumBytes,
    [string]$Label
) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath) -or
        $RelativePath.Contains("..")) {
        throw "$Label has a non-canonical trusted-source path."
    }
    $canonical = $RelativePath.Replace('\', '/')
    $matches = @($Archive.Entries | Where-Object { $_.FullName -eq $canonical })
    if ($matches.Count -ne 1 -or $matches[0].Length -gt $MaximumBytes -or $matches[0].Length -gt [int]::MaxValue) {
        throw "$Label is absent, duplicated, or oversized in the trusted source archive."
    }
    $stream = $matches[0].Open()
    try {
        $bytes = [byte[]]::new([int]$matches[0].Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -le 0) { throw "$Label ended before its admitted archive length." }
            $offset += $read
        }
    } finally { $stream.Dispose() }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $hash = (($sha256.ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join "") }
    finally { $sha256.Dispose() }
    return [pscustomobject]@{
        sha256 = $hash
        length = [long]$bytes.Length
        bytes = $bytes
        text = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff)
    }
}

function Copy-AdmittedFile(
    [string]$SourcePath,
    [string]$DestinationPath,
    [long]$MaximumBytes,
    [string]$Label,
    [bool]$AllowEmpty = $false
) {
    $item = Get-Item -LiteralPath $SourcePath -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        (-not $AllowEmpty -and $item.Length -le 0) -or $item.Length -gt $MaximumBytes) {
        throw "$Label is not a bounded regular non-link file: $SourcePath"
    }
    $parent = Split-Path -Parent $DestinationPath
    New-Item -ItemType Directory -Path $parent -Force -ErrorAction Stop | Out-Null
    $source = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $destination = [IO.File]::Open($DestinationPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        if ($source.Length -ne $item.Length) { throw "$Label changed before snapshot admission." }
        $buffer = [byte[]]::new(1048576)
        $length = 0L
        while (($read = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $null = $sha256.TransformBlock($buffer, 0, $read, $null, 0)
            $destination.Write($buffer, 0, $read)
            $length += $read
        }
        $null = $sha256.TransformFinalBlock([byte[]]::new(0), 0, 0)
        if ($length -ne $source.Length) { throw "$Label changed during snapshot admission." }
        $destination.Flush($true)
        return [pscustomobject]@{
            path = [IO.Path]::GetFullPath($DestinationPath)
            sha256 = (($sha256.Hash | ForEach-Object { $_.ToString("x2") }) -join "")
            length = $length
            source_path = $item.FullName
        }
    } finally {
        $sha256.Dispose()
        $destination.Dispose()
        $source.Dispose()
    }
}

function Invoke-BoundedProcess(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$WorkingDirectory,
    [int]$TimeoutSeconds,
    [string]$StdoutPath,
    [string]$StderrPath
) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.WorkingDirectory = $WorkingDirectory
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
        if (-not $process.Start()) { throw "Could not start platform matrix replay." }
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderr)
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { $null = $process.WaitForExit(1000) }
        if (-not $process.HasExited) {
            try { $process.Kill($true); $process.WaitForExit() } catch { }
            throw "Platform matrix replay exceeded its $TimeoutSeconds second deadline."
        }
        $process.WaitForExit()
        $stdoutCopy.GetAwaiter().GetResult()
        $stderrCopy.GetAwaiter().GetResult()
        $stdout.Flush($true)
        $stderr.Flush($true)
        return $process.ExitCode
    } finally {
        $process.Dispose()
        $stdout.Dispose()
        $stderr.Dispose()
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

function Resolve-ApprovedVerifierTool([object]$Manifest, [string]$ManifestPath, [string]$ToolId) {
    $matches = @($Manifest.tools | Where-Object { [string]$_.id -eq $ToolId })
    if ($matches.Count -ne 1 -or [string]$matches[0].sha256 -notmatch '^[0-9a-f]{64}$' -or
        [IO.Path]::IsPathRooted([string]$matches[0].path) -or [string]::IsNullOrWhiteSpace([string]$matches[0].path)) {
        throw "Verifier tools manifest has no unique bounded '$ToolId' entry."
    }
    $root = [IO.Path]::GetFullPath((Split-Path -Parent $ManifestPath))
    $path = [IO.Path]::GetFullPath((Join-Path $root ([string]$matches[0].path)))
    $prefix = "$($root.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
    $comparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if (-not $path.StartsWith($prefix, $comparison)) { throw "Verifier tool '$ToolId' escapes its approved directory." }
    $cursor = [IO.Path]::GetFullPath((Split-Path -Parent $path))
    while ($true) {
        $directory = Get-Item -LiteralPath $cursor -ErrorAction Stop
        if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Verifier tool '$ToolId' traverses a linked directory."
        }
        if ($cursor -eq $root) { break }
        $cursor = [IO.Path]::GetFullPath((Split-Path -Parent $cursor))
    }
    $admission = Get-BoundedFile $path 8589934592 "approved verifier tool '$ToolId'"
    if ([string]$admission.sha256 -ne [string]$matches[0].sha256) {
        throw "Verifier tool '$ToolId' differs from its separately approved SHA-256."
    }
    return $admission
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$bundle = [IO.Path]::GetFullPath($BundleDirectory)
$expectedSource = $ExpectedSourceSha.ToLowerInvariant()
$expectedBuildManifest = $ExpectedBuildManifestSha256.ToLowerInvariant()
$expectedPolicyAbsolute = Resolve-RepositoryPath $ExpectedPolicyPath
$expectedProfileAbsolute = Resolve-RepositoryPath $ExpectedRuntimeProfilePath
$expectedCaptureAuthorityAbsolute = Resolve-RepositoryPath $ExpectedCaptureAuthorityManifestPath
$expectedVerifierToolsAbsolute = Resolve-RepositoryPath $ExpectedVerifierToolsManifestPath
$bundleItem = Get-Item -LiteralPath $bundle -ErrorAction Stop
if (-not $bundleItem.PSIsContainer -or
    ($bundleItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Qualification bundle must be a regular non-link directory."
}
$maximumJsonBytes = 8388608L
$maximumArtifactBytes = 8589934592L
$maximumTotalEvidenceBytes = 68719476736L
$expectedPolicy = Read-BoundedJson $expectedPolicyAbsolute 1048576 "externally approved matrix policy" $true
$expectedProfile = Read-BoundedJson $expectedProfileAbsolute 1048576 "externally approved runtime profile" $true
$expectedCaptureAuthority = Read-BoundedJson $expectedCaptureAuthorityAbsolute 8388608 "external capture authority manifest" $true
$expectedVerifierTools = Read-BoundedJson $expectedVerifierToolsAbsolute 1048576 "external verifier tools manifest" $true
$expectedPolicyAdmission = $jsonAdmissions[[IO.Path]::GetFullPath($expectedPolicyAbsolute)]
$expectedProfileAdmission = $jsonAdmissions[[IO.Path]::GetFullPath($expectedProfileAbsolute)]
$expectedCaptureAuthorityAdmission = $jsonAdmissions[[IO.Path]::GetFullPath($expectedCaptureAuthorityAbsolute)]
$expectedVerifierToolsAdmission = $jsonAdmissions[[IO.Path]::GetFullPath($expectedVerifierToolsAbsolute)]
if ([string]$expectedCaptureAuthorityAdmission.sha256 -ne $ExpectedCaptureAuthorityManifestSha256.ToLowerInvariant()) {
    throw "External capture authority manifest differs from its separately approved SHA-256."
}
if ([string]$expectedVerifierToolsAdmission.sha256 -ne $ExpectedVerifierToolsManifestSha256.ToLowerInvariant()) {
    throw "External verifier tools manifest differs from its separately approved SHA-256."
}
$anchorDirectory = Join-Path ([IO.Path]::GetTempPath()) ("mondrian-platform-matrix-anchors-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $anchorDirectory -ErrorAction Stop | Out-Null
$anchorPolicyPath = Join-Path $anchorDirectory "approved-policy.json"
$anchorProfilePath = Join-Path $anchorDirectory "approved-runtime-profile.json"
$anchorCaptureAuthorityPath = Join-Path $anchorDirectory "approved-capture-authority.json"
$anchorVerifierToolsPath = Join-Path $anchorDirectory "approved-verifier-tools.json"
$anchorClosurePath = Join-Path $anchorDirectory "admitted-evidence-closure.json"
Write-CreateOnlyBytes $anchorPolicyPath $expectedPolicyAdmission.bytes
Write-CreateOnlyBytes $anchorProfilePath $expectedProfileAdmission.bytes
Write-CreateOnlyBytes $anchorCaptureAuthorityPath $expectedCaptureAuthorityAdmission.bytes
Write-CreateOnlyBytes $anchorVerifierToolsPath $expectedVerifierToolsAdmission.bytes
Add-AnchorAdmission $anchorPolicyPath 1048576 "private approved policy"
Add-AnchorAdmission $anchorProfilePath 1048576 "private approved runtime profile"
Add-AnchorAdmission $anchorCaptureAuthorityPath 8388608 "private approved capture authority"
Add-AnchorAdmission $anchorVerifierToolsPath 1048576 "private approved verifier tools"
trap {
    $previousReplayVariable = Get-Variable -Name "previousReplayEnvironment" -Scope Script -ErrorAction SilentlyContinue
    if ($null -ne $previousReplayVariable) {
        foreach ($name in $script:previousReplayEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($name, $script:previousReplayEnvironment[$name], "Process")
        }
    }
    foreach ($name in @(
        "MondrianQualificationSourceVerifierScriptBlock", "MondrianQualificationPlatformProducerSha256",
        "MondrianQualificationGpuProducerSha256", "MondrianQualificationGpuProfileAdmission"
    )) { Remove-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue }
    foreach ($lock in $anchorReadLocks) { $lock.Dispose() }
    if (Test-Path -LiteralPath $anchorDirectory) {
        Remove-Item -LiteralPath $anchorDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
    throw
}
if ($expectedPolicy.schema_version -ne 1 -or $expectedPolicy.execution_policy -ne "sealed-required" -or
    $expectedPolicy.executed_runtime_image_required -ne $true -or
    $expectedPolicy.owner_verified_leaf_receipts_required -ne $true -or
    $expectedPolicy.source_evidence_required -ne $true -or
    $expectedPolicy.environment_snapshots_required -ne $true -or
    $expectedPolicy.capture_authority_manifest_required -ne $true -or
    $expectedPolicy.capture_authority_manifest_schema_version -ne 1 -or
    $expectedPolicy.separately_approved_verifier_tools_required -ne $true -or
    $expectedPolicy.runtime_cargo_replay_forbidden -ne $true -or
    [string]$expectedPolicy.runner.verifier_tool_id -ne "platform_qualification_replay" -or
    [int]$expectedPolicy.runner.timeout_seconds -le 0 -or
    [int]$expectedPolicy.runner.report_schema_version -ne 1 -or
    $expectedPolicy.owner_verifier_contracts.raw_json_must_be_owner_evaluated -ne $true -or
    $expectedProfile.schema_version -ne [int]$expectedPolicy.runtime_profile_schema_version) {
    throw "External policy or runtime profile weakens the sealed qualification contract."
}
if ($expectedCaptureAuthority.schema_version -ne [int]$expectedPolicy.capture_authority_manifest_schema_version -or
    [string]::IsNullOrWhiteSpace([string]$expectedCaptureAuthority.authority_id) -or
    [string]::IsNullOrWhiteSpace([string]$expectedCaptureAuthority.approved_at_utc) -or
    [string]$expectedCaptureAuthority.source_revision -ne $expectedSource -or
    [string]$expectedCaptureAuthority.release_candidate_id -ne $ExpectedReleaseCandidateId -or
    [string]$expectedCaptureAuthority.build_manifest_sha256 -ne $expectedBuildManifest -or
    [string]$expectedCaptureAuthority.policy_sha256 -ne [string]$expectedPolicyAdmission.sha256 -or
    [string]$expectedCaptureAuthority.runtime_profile_sha256 -ne [string]$expectedProfileAdmission.sha256) {
    throw "External capture authority manifest does not bind the approved release inputs."
}
if ($expectedVerifierTools.schema_version -ne 1 -or
    [string]$expectedVerifierTools.source_revision -ne $expectedSource -or
    [string]$expectedVerifierTools.release_candidate_id -ne $ExpectedReleaseCandidateId -or
    [string]::IsNullOrWhiteSpace([string]$expectedVerifierTools.authority_id) -or
    [string]::IsNullOrWhiteSpace([string]$expectedVerifierTools.approved_at_utc)) {
    throw "External verifier tools manifest does not bind the approved source/release."
}
Assert-ExactStringSet @(
    "display_contract_replay", "display_calibration_replay", "platform_qualification_replay"
) @($expectedVerifierTools.tools | ForEach-Object { [string]$_.id }) "external verifier tool closure"
$displayContractReplayTool = Resolve-ApprovedVerifierTool $expectedVerifierTools $expectedVerifierToolsAbsolute `
    "display_contract_replay"
$displayCalibrationReplayTool = Resolve-ApprovedVerifierTool $expectedVerifierTools $expectedVerifierToolsAbsolute `
    "display_calibration_replay"
$platformQualificationReplayTool = Resolve-ApprovedVerifierTool $expectedVerifierTools $expectedVerifierToolsAbsolute `
    "platform_qualification_replay"
Add-AnchorAdmission $displayContractReplayTool.path 8589934592 "approved display contract replay"
Add-AnchorAdmission $displayCalibrationReplayTool.path 8589934592 "approved display calibration replay"
Add-AnchorAdmission $platformQualificationReplayTool.path 8589934592 "approved platform qualification replay"
$initialHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $initialHeadSha -ne $expectedSource -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Bundle verification requires the exact clean externally trusted source revision."
}
$sourceArchivePath = Join-Path $anchorDirectory "trusted-source.zip"
$treeEntries = @(& git -C $repositoryRoot ls-tree -r --full-tree $expectedSource)
if ($LASTEXITCODE -ne 0 -or @($treeEntries | Where-Object { $_ -match '^(120000|160000)\s' }).Count -ne 0) {
    throw "Trusted source revision is unavailable or contains links/submodules."
}
& git -C $repositoryRoot archive --format=zip "--output=$sourceArchivePath" $expectedSource
if ($LASTEXITCODE -ne 0) { throw "Could not archive the externally trusted source revision." }
Add-AnchorAdmission $sourceArchivePath $maximumArtifactBytes "trusted source archive"
$policyRelative = [IO.Path]::GetRelativePath($repositoryRoot, $expectedPolicyAbsolute).Replace('\', '/')
$archiveStream = [IO.File]::Open($sourceArchivePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
$trustedArchive = [IO.Compression.ZipArchive]::new($archiveStream, [IO.Compression.ZipArchiveMode]::Read, $false)
try {
    $trustedPolicy = Read-TrustedArchiveEntry $trustedArchive $policyRelative 1048576 "checked-in matrix policy"
    if ($trustedPolicy.sha256 -ne [string]$expectedPolicyAdmission.sha256) {
        throw "Externally approved matrix policy differs from the trusted source revision."
    }
    $trustedOwnerVerifier = Read-TrustedArchiveEntry $trustedArchive `
        ([string]$expectedPolicy.owner_verifier_script) 8388608 "trusted owner verifier"
    $trustedSourceVerifier = Read-TrustedArchiveEntry $trustedArchive `
        ([string]$expectedPolicy.source_verifier_script) 8388608 "trusted source verifier"
    $trustedPlatformProducer = Read-TrustedArchiveEntry $trustedArchive `
        ([string]$expectedPolicy.source_evidence_contracts.platform_probe_producer_script) 8388608 `
        "trusted platform producer supervisor"
    $trustedGpuProducer = Read-TrustedArchiveEntry $trustedArchive `
        ([string]$expectedPolicy.source_evidence_contracts.gpu_color_producer_script) 8388608 `
        "trusted GPU producer supervisor"
    $trustedGpuProfile = Read-TrustedArchiveEntry $trustedArchive `
        ([string]$expectedPolicy.source_evidence_contracts.gpu_color_profile_path) 1048576 `
        "trusted GPU gate profile"
} finally {
    $trustedArchive.Dispose()
    $archiveStream.Dispose()
}
$ownerVerifierScriptBlock = [ScriptBlock]::Create($trustedOwnerVerifier.text)
$global:MondrianQualificationSourceVerifierScriptBlock = [ScriptBlock]::Create($trustedSourceVerifier.text)
$global:MondrianQualificationPlatformProducerSha256 = [string]$trustedPlatformProducer.sha256
$global:MondrianQualificationGpuProducerSha256 = [string]$trustedGpuProducer.sha256
$global:MondrianQualificationGpuProfileAdmission = $trustedGpuProfile
$sealedPath = Join-Path $bundle "sealed-matrix.json"
$closurePath = Join-Path $bundle "evidence-closure.json"
$resolvedPath = Join-Path $bundle "resolved-campaign.json"
$reportPath = Join-Path $bundle "qualification-report.json"
$stdoutPath = Join-Path $bundle "qualification.stdout.log"
$stderrPath = Join-Path $bundle "qualification.stderr.log"
$sealed = Read-BoundedJson $sealedPath $maximumJsonBytes "sealed matrix" $true
$closure = Read-BoundedJson $closurePath $maximumJsonBytes "evidence closure" $true
$resolved = Read-BoundedJson $resolvedPath $maximumJsonBytes "resolved campaign" $true
$report = Read-BoundedJson $reportPath $maximumJsonBytes "qualification report" $true
Write-CreateOnlyBytes $anchorClosurePath $jsonAdmissions[[IO.Path]::GetFullPath($closurePath)].bytes
$snapshotResolvedPath = Join-Path $anchorDirectory "resolved-campaign.json"
$snapshotReportPath = Join-Path $anchorDirectory "qualification-report.json"
Write-CreateOnlyBytes $snapshotResolvedPath $jsonAdmissions[[IO.Path]::GetFullPath($resolvedPath)].bytes
Write-CreateOnlyBytes $snapshotReportPath $jsonAdmissions[[IO.Path]::GetFullPath($reportPath)].bytes
Add-AnchorAdmission $anchorClosurePath $maximumJsonBytes "private evidence closure"
Add-AnchorAdmission $snapshotResolvedPath $maximumJsonBytes "private resolved campaign"
Add-AnchorAdmission $snapshotReportPath $maximumJsonBytes "private qualification report"

if ($sealed.schema_version -ne 1 -or [string]$sealed.status -ne "qualified" -or
    [string]$sealed.source_revision -ne $expectedSource -or
    [string]$sealed.release_candidate_id -ne $ExpectedReleaseCandidateId -or
    [string]$sealed.build_manifest_sha256 -ne $expectedBuildManifest -or
    $closure.schema_version -ne 1 -or [string]$sealed.source_revision -ne [string]$closure.source_revision -or
    [string]$sealed.source_revision -ne [string]$resolved.source_revision -or
    [string]$sealed.source_revision -ne [string]$report.source_revision -or
    [string]$sealed.release_candidate_id -ne [string]$closure.release_candidate_id -or
    [string]$sealed.release_candidate_id -ne [string]$resolved.release_candidate_id -or
    [string]$sealed.release_candidate_id -ne [string]$report.release_candidate_id -or
    [string]$sealed.build_manifest_sha256 -ne [string]$closure.build_manifest_sha256 -or
    [string]$sealed.build_manifest_sha256 -ne [string]$resolved.build_manifest_sha256 -or
    [string]$sealed.build_manifest_sha256 -ne [string]$report.build_manifest_sha256 -or
    [string]$report.status -ne "qualified" -or @($report.missing_cells).Count -ne 0 -or
    [string]::IsNullOrWhiteSpace([string]$report.profile_sha256) -or
    [string]::IsNullOrWhiteSpace([string]$report.evidence_sha256)) {
    throw "Qualification bundle identities or terminal verdict are inconsistent."
}

$topLevelNames = @(Get-ChildItem -LiteralPath $bundle -Force | ForEach-Object { $_.Name })
Assert-ExactStringSet @(
    "sealed-matrix.json", "evidence-closure.json", "resolved-campaign.json",
    "qualification-report.json", "qualification.stdout.log", "qualification.stderr.log", "evidence"
) $topLevelNames "bundle top-level closure"

$stdoutAdmission = Get-BoundedFile $stdoutPath $maximumArtifactBytes "qualification stdout" $true
$stderrAdmission = Get-BoundedFile $stderrPath $maximumArtifactBytes "qualification stderr" $true
if ([string]$jsonAdmissions[[IO.Path]::GetFullPath($closurePath)].sha256 -ne [string]$sealed.evidence_closure_sha256 -or
    [string]$jsonAdmissions[[IO.Path]::GetFullPath($resolvedPath)].sha256 -ne [string]$sealed.resolved_campaign_sha256 -or
    [string]$jsonAdmissions[[IO.Path]::GetFullPath($reportPath)].sha256 -ne [string]$sealed.qualification_report_sha256 -or
    $stdoutAdmission.sha256 -ne [string]$sealed.stdout_sha256 -or
    $stderrAdmission.sha256 -ne [string]$sealed.stderr_sha256) {
    throw "Qualification bundle top-level artifact hash mismatch."
}

$evidenceRoot = Join-Path $bundle "evidence"
$evidenceRootItem = Get-Item -LiteralPath $evidenceRoot -ErrorAction Stop
if (-not $evidenceRootItem.PSIsContainer -or
    ($evidenceRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Evidence closure directory must be regular and non-link."
}
$declaredPaths = @($closure.entries | ForEach-Object { [string]$_.bundle_path })
$actualEvidence = @(Get-ChildItem -LiteralPath $evidenceRoot -Force)
if (@($actualEvidence | Where-Object { $_.PSIsContainer -or ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count -ne 0) {
    throw "Evidence closure contains a directory or link."
}
$actualPaths = @($actualEvidence | ForEach-Object {
    "evidence/$($_.Name)"
})
Assert-ExactStringSet $declaredPaths $actualPaths "bundled evidence file closure"

$totalBytes = 0L
$roleHashes = @{}
$rolePaths = @{}
$bundleEvidenceAdmissions = [System.Collections.Generic.List[object]]::new()
$snapshotEvidenceRoot = Join-Path $anchorDirectory "evidence"
New-Item -ItemType Directory -Path $snapshotEvidenceRoot -ErrorAction Stop | Out-Null
foreach ($entry in @($closure.entries)) {
    $relative = [string]$entry.bundle_path
    if ([IO.Path]::IsPathRooted($relative) -or -not $relative.StartsWith("evidence/", [StringComparison]::Ordinal)) {
        throw "Evidence closure contains a non-canonical bundle path."
    }
    $path = [IO.Path]::GetFullPath((Join-Path $bundle $relative))
    $prefix = "$($evidenceRoot.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
    $comparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if (-not $path.StartsWith($prefix, $comparison)) { throw "Evidence closure path escapes its directory." }
    $snapshotPath = Join-Path $anchorDirectory $relative
    $file = Copy-AdmittedFile $path $snapshotPath $maximumArtifactBytes "bundled evidence"
    if ($file.sha256 -ne [string]$entry.sha256 -or $file.length -ne [long]$entry.byte_length -or
        @($entry.roles).Count -eq 0) {
        throw "Bundled evidence hash, length, or role mismatch."
    }
    $bundleEvidenceAdmissions.Add($file)
    Add-AnchorAdmission $file.path $maximumArtifactBytes "private bundled evidence '$relative'"
    $totalBytes += $file.length
    if ($totalBytes -gt $maximumTotalEvidenceBytes) { throw "Evidence closure exceeds its byte bound." }
    foreach ($role in @($entry.roles)) {
        $key = [string]$role
        if ($roleHashes.ContainsKey($key)) { throw "Evidence role '$key' is duplicated." }
        $roleHashes[$key] = $file.sha256
        $rolePaths[$key] = $file.path
    }
}
if ($totalBytes -ne [long]$closure.total_evidence_bytes -or
    $roleHashes["policy"] -ne [string]$sealed.policy_sha256 -or
    $roleHashes["runtime-profile"] -ne [string]$sealed.runtime_profile_file_sha256 -or
    $roleHashes["campaign-envelope"] -ne [string]$sealed.campaign_envelope_sha256 -or
    $roleHashes["matrix-artifact-manifest"] -ne [string]$sealed.matrix_artifact_manifest_sha256 -or
    $roleHashes["build-manifest"] -ne [string]$sealed.build_manifest_sha256) {
    throw "Evidence closure does not bind every matrix authority artifact."
}

$expectedRoles = [System.Collections.Generic.List[string]]::new()
foreach ($role in @("policy", "runtime-profile", "campaign-envelope", "matrix-artifact-manifest", "build-manifest")) {
    $expectedRoles.Add($role)
}
foreach ($cell in @($resolved.cells)) {
    $cellId = [string]$cell.cell_id
    foreach ($role in @(
        "row:$cellId:seal",
        "row:$cellId:observation",
        "row:$cellId:artifact-manifest",
        "row:$cellId:product-artifact",
        "row:$cellId:runtime-image",
        "row:$cellId:build-provenance",
        "row:$cellId:machine-report",
        "row:$cellId:environment-before",
        "row:$cellId:environment-after"
    )) {
        $expectedRoles.Add($role)
    }
    foreach ($receipt in @($cell.reports)) {
        $kind = [string]$receipt.kind
        $expectedRoles.Add("row:${cellId}:${kind}:report")
        $expectedRoles.Add("row:${cellId}:${kind}:raw")
    }
    $roleManifestPath = [string]$rolePaths["row:$cellId:artifact-manifest"]
    if ([string]::IsNullOrWhiteSpace($roleManifestPath)) {
        throw "Bundle is missing the row artifact manifest for '$cellId'."
    }
    $roleManifest = Read-BoundedJson $roleManifestPath $maximumJsonBytes "bundled row artifact manifest"
    foreach ($receipt in @($cell.reports)) {
        $kind = [string]$receipt.kind
        $laneEntries = @($roleManifest.artifacts | Where-Object { [string]$_.kind -eq $kind })
        if ($laneEntries.Count -ne 1) { throw "Bundled row '$cellId' has no unique lane '$kind'." }
        foreach ($sourceEntry in @($laneEntries[0].source_evidence.entries)) {
            $expectedRoles.Add("row:${cellId}:${kind}:source:$($sourceEntry.role)")
        }
    }
}
Assert-ExactStringSet @($expectedRoles) @($rolePaths.Keys) "bundled evidence role closure"
Assert-ExactStringSet @($resolved.cells | ForEach-Object { [string]$_.cell_id }) `
    @($expectedCaptureAuthority.rows | ForEach-Object { [string]$_.cell_id }) `
    "external capture authority row closure"

if ($roleHashes["policy"] -ne [string]$expectedPolicyAdmission.sha256 -or
    $roleHashes["runtime-profile"] -ne [string]$expectedProfileAdmission.sha256 -or
    $roleHashes["build-manifest"] -ne $expectedBuildManifest) {
    throw "Bundle authority artifacts do not match the external trust anchors."
}

$bundledBuildManifestPath = [string]$rolePaths["build-manifest"]
$bundledBuildManifest = Read-BoundedJson $bundledBuildManifestPath $maximumJsonBytes "bundled build manifest"
if ([string]$bundledBuildManifest.source_revision -ne $expectedSource -or
    [string]$bundledBuildManifest.release_candidate_id -ne $ExpectedReleaseCandidateId -or
    [int]$bundledBuildManifest.schema_version -ne [int]$expectedPolicy.build_manifest_schema_version) {
    throw "Bundled build manifest does not match the approved release candidate."
}

$campaignEnvelope = Read-BoundedJson ([string]$rolePaths["campaign-envelope"]) $maximumJsonBytes "bundled campaign envelope"
$matrixManifest = Read-BoundedJson ([string]$rolePaths["matrix-artifact-manifest"]) $maximumJsonBytes "bundled matrix artifact manifest"
if ($campaignEnvelope.schema_version -ne 1 -or $matrixManifest.schema_version -ne 1 -or
    [string]$campaignEnvelope.campaign_id -ne [string]$resolved.campaign_id -or
    [string]$campaignEnvelope.source_revision -ne $expectedSource -or
    [string]$matrixManifest.source_revision -ne $expectedSource -or
    [string]$campaignEnvelope.release_candidate_id -ne $ExpectedReleaseCandidateId -or
    [string]$matrixManifest.release_candidate_id -ne $ExpectedReleaseCandidateId -or
    [string]$campaignEnvelope.build_manifest_sha256 -ne $expectedBuildManifest -or
    [string]$matrixManifest.build_manifest_sha256 -ne $expectedBuildManifest) {
    throw "Bundled campaign or matrix artifact manifest is not externally release-bound."
}
Assert-ExactStringSet @($resolved.cells | ForEach-Object { [string]$_.cell_id }) `
    @($matrixManifest.sealed_rows | ForEach-Object { [string]$_.cell_id }) `
    "bundled matrix row closure"

$replayEnvironment = @{
    MONDRIAN_DISPLAY_CONTRACT_REPLAY_EXECUTABLE = [string]$displayContractReplayTool.path
    MONDRIAN_DISPLAY_CONTRACT_REPLAY_SHA256 = [string]$displayContractReplayTool.sha256
    MONDRIAN_DISPLAY_CALIBRATION_REPLAY_EXECUTABLE = [string]$displayCalibrationReplayTool.path
    MONDRIAN_DISPLAY_CALIBRATION_REPLAY_SHA256 = [string]$displayCalibrationReplayTool.sha256
}
$previousReplayEnvironment = @{}
foreach ($name in $replayEnvironment.Keys) {
    $previousReplayEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    [Environment]::SetEnvironmentVariable($name, [string]$replayEnvironment[$name], "Process")
}
$usedBuildArtifactKeys = [System.Collections.Generic.List[string]]::new()
foreach ($cell in @($resolved.cells)) {
    $cellId = [string]$cell.cell_id
    $cellPath = [string]$rolePaths["row:$cellId:observation"]
    $machinePath = [string]$rolePaths["row:$cellId:machine-report"]
    if ([string]::IsNullOrWhiteSpace($cellPath) -or [string]::IsNullOrWhiteSpace($machinePath)) {
        throw "Bundle is missing the observation or machine report for row '$cellId'."
    }
    $matrixRows = @($matrixManifest.sealed_rows | Where-Object { [string]$_.cell_id -eq $cellId })
    if ($matrixRows.Count -ne 1) { throw "Bundled row '$cellId' is not unique." }
    $matrixRow = $matrixRows[0]
    $sealRole = "row:$cellId:seal"
    $manifestRole = "row:$cellId:artifact-manifest"
    $observationRole = "row:$cellId:observation"
    $sealPath = [string]$rolePaths[$sealRole]
    $rowManifestPath = [string]$rolePaths[$manifestRole]
    $sealedRow = Read-BoundedJson $sealPath $maximumJsonBytes "bundled sealed row"
    $rowManifest = Read-BoundedJson $rowManifestPath $maximumJsonBytes "bundled row artifact manifest"
    $authorityRows = @($expectedCaptureAuthority.rows | Where-Object { [string]$_.cell_id -eq $cellId })
    if ($authorityRows.Count -ne 1) { throw "External capture authority row '$cellId' is not unique." }
    $authorityRow = $authorityRows[0]
    if ($roleHashes[$sealRole] -ne [string]$matrixRow.sealed_row_sha256 -or
        $roleHashes[$observationRole] -ne [string]$matrixRow.cell_observation_sha256 -or
        $roleHashes[$manifestRole] -ne [string]$matrixRow.row_artifact_manifest_sha256 -or
        $sealedRow.schema_version -ne 1 -or [string]$sealedRow.status -ne "sealed_for_matrix_resolution" -or
        [string]$sealedRow.cell_id -ne $cellId -or [string]$rowManifest.cell_id -ne $cellId -or
        [string]$sealedRow.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$rowManifest.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$sealedRow.source_sha -ne $expectedSource -or
        [string]$rowManifest.source_revision -ne $expectedSource -or
        [string]$sealedRow.release_candidate_id -ne $ExpectedReleaseCandidateId -or
        [string]$rowManifest.release_candidate_id -ne $ExpectedReleaseCandidateId -or
        [string]$sealedRow.build_manifest_sha256 -ne $expectedBuildManifest -or
        [string]$rowManifest.build_manifest_sha256 -ne $expectedBuildManifest -or
        [string]$sealedRow.policy_sha256 -ne [string]$expectedPolicyAdmission.sha256 -or
        [string]$sealedRow.runtime_profile_file_sha256 -ne [string]$expectedProfileAdmission.sha256 -or
        [string]$sealedRow.cell_observation_sha256 -ne [string]$roleHashes[$observationRole] -or
        [string]$sealedRow.artifact_manifest_sha256 -ne [string]$roleHashes[$manifestRole] -or
        [string]$sealedRow.machine_report_sha256 -ne [string]$roleHashes["row:$cellId:machine-report"] -or
        [string]$rowManifest.machine_report.sha256 -ne [string]$roleHashes["row:$cellId:machine-report"] -or
        [string]$authorityRow.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$authorityRow.environment_before_sha256 -ne [string]$roleHashes["row:$cellId:environment-before"] -or
        [string]$authorityRow.environment_after_sha256 -ne [string]$roleHashes["row:$cellId:environment-after"]) {
        throw "Bundled row seal chain is not atomically closed for '$cellId'."
    }
    Assert-ExactStringSet @($cell.reports | ForEach-Object { [string]$_.kind }) `
        @($rowManifest.artifacts | ForEach-Object { [string]$_.kind }) `
        "bundled row '$cellId' lane closure"
    Assert-ExactStringSet @($cell.reports | ForEach-Object { [string]$_.kind }) `
        @($authorityRow.captures | ForEach-Object { [string]$_.kind }) `
        "external capture authority lane closure for '$cellId'"
    $buildArtifacts = @($bundledBuildManifest.artifacts | Where-Object {
        [string]$_.platform -eq [string]$cell.product_artifact.platform -and
        [string]$_.target_triple -eq [string]$cell.product_artifact.target_triple -and
        [string]$_.package_kind -eq [string]$cell.product_artifact.package_kind
    })
    if ($buildArtifacts.Count -ne 1) {
        throw "Bundled build manifest has no unique target artifact for row '$cellId'."
    }
    $buildArtifact = $buildArtifacts[0]
    $buildArtifactKey = "$($buildArtifact.platform)/$($buildArtifact.target_triple)/$($buildArtifact.package_kind)"
    $usedBuildArtifactKeys.Add($buildArtifactKey)
    $productRole = "row:$cellId:product-artifact"
    $runtimeRole = "row:$cellId:runtime-image"
    $provenanceRole = "row:$cellId:build-provenance"
    if ($roleHashes[$productRole] -ne [string]$cell.product_artifact.sha256 -or
        $roleHashes[$productRole] -ne [string]$buildArtifact.sha256 -or
        $roleHashes[$runtimeRole] -ne [string]$cell.product_artifact.runtime_image_sha256 -or
        $roleHashes[$runtimeRole] -ne [string]$buildArtifact.runtime_image.sha256 -or
        $roleHashes[$provenanceRole] -ne [string]$cell.product_artifact.build_provenance_sha256 -or
        $roleHashes[$provenanceRole] -ne [string]$buildArtifact.build_provenance.sha256) {
        throw "Bundled package, runtime image, or provenance differs from the approved build manifest."
    }
    if ([string]$sealedRow.product_artifact_sha256 -ne [string]$roleHashes[$productRole] -or
        [string]$sealedRow.runtime_image_sha256 -ne [string]$roleHashes[$runtimeRole] -or
        [string]$sealedRow.build_provenance_sha256 -ne [string]$roleHashes[$provenanceRole] -or
        [string]$sealedRow.platform -ne [string]$cell.product_artifact.platform -or
        [string]$sealedRow.target_triple -ne [string]$cell.product_artifact.target_triple -or
        [string]$sealedRow.package_kind -ne [string]$cell.product_artifact.package_kind -or
        [string]$rowManifest.product_artifact.sha256 -ne [string]$roleHashes[$productRole] -or
        [string]$rowManifest.product_artifact.runtime_image_sha256 -ne [string]$roleHashes[$runtimeRole] -or
        [string]$rowManifest.product_artifact.build_provenance_sha256 -ne [string]$roleHashes[$provenanceRole]) {
        throw "Bundled row seal does not bind its exact target artifacts."
    }
    $provenancePath = [string]$rolePaths[$provenanceRole]
    $provenance = Read-BoundedJson $provenancePath $maximumJsonBytes "bundled build provenance"
    if ($provenance.schema_version -ne 1 -or
        [string]$provenance.source_revision -ne $expectedSource -or
        [string]$provenance.release_candidate_id -ne $ExpectedReleaseCandidateId -or
        [string]$provenance.target_triple -ne [string]$cell.product_artifact.target_triple -or
        [string]$provenance.package_kind -ne [string]$cell.product_artifact.package_kind -or
        [string]$provenance.product_artifact_sha256 -ne [string]$cell.product_artifact.sha256 -or
        [string]$provenance.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256) {
        throw "Bundled build provenance does not bind the exact row target."
    }
    foreach ($receipt in @($cell.reports)) {
        $kind = [string]$receipt.kind
        $laneReportPath = [string]$rolePaths["row:${cellId}:${kind}:report"]
        $rawPath = [string]$rolePaths["row:${cellId}:${kind}:raw"]
        if ([string]::IsNullOrWhiteSpace($laneReportPath) -or [string]::IsNullOrWhiteSpace($rawPath)) {
            throw "Bundle is missing owner evidence for row '$cellId' lane '$kind'."
        }
        $manifestLanes = @($rowManifest.artifacts | Where-Object { [string]$_.kind -eq $kind })
        $authorityCaptures = @($authorityRow.captures | Where-Object { [string]$_.kind -eq $kind })
        if ($manifestLanes.Count -ne 1 -or
            $authorityCaptures.Count -ne 1 -or
            [string]$roleHashes["row:${cellId}:${kind}:report"] -ne [string]$receipt.report_sha256 -or
            [string]$roleHashes["row:${cellId}:${kind}:report"] -ne [string]$manifestLanes[0].report_sha256 -or
            [string]$roleHashes["row:${cellId}:${kind}:raw"] -ne [string]$receipt.raw_evidence_sha256 -or
            [string]$roleHashes["row:${cellId}:${kind}:raw"] -ne [string]$manifestLanes[0].raw_evidence_sha256) {
            throw "Bundled row manifest does not bind lane '$kind' evidence."
        }
        $manifestLane = $manifestLanes[0]
        $authorityCapture = $authorityCaptures[0]
        if ([string]$authorityCapture.capture_id -ne [string]$manifestLane.source_evidence.capture_id -or
            [string]$authorityCapture.source_verifier_id -ne [string]$manifestLane.source_evidence.source_verifier_id) {
            throw "External capture authority does not bind lane '$kind' for row '$cellId'."
        }
        if ($kind -in @("platform_probe", "gpu_color")) {
            $challengeEntries = @($manifestLane.source_evidence.entries | Where-Object {
                [string]$_.role -eq "authority-challenge.json"
            })
            $transcriptEntries = @($manifestLane.source_evidence.entries | Where-Object {
                [string]$_.role -eq [string]$manifestLane.source_evidence.bindings.session_transcript_role
            })
            if ($challengeEntries.Count -ne 1 -or $transcriptEntries.Count -ne 1 -or
                [string]$authorityCapture.challenge_id -ne [string]$manifestLane.source_evidence.bindings.authority_challenge_id -or
                [string]$authorityCapture.challenge_manifest_sha256 -ne [string]$manifestLane.source_evidence.bindings.authority_challenge_sha256 -or
                [string]$authorityCapture.challenge_manifest_sha256 -ne [string]$challengeEntries[0].sha256 -or
                [string]$authorityCapture.producer_id -ne [string]$manifestLane.source_evidence.bindings.producer_id -or
                [string]$authorityCapture.producer_sha256 -ne [string]$manifestLane.source_evidence.bindings.producer_sha256 -or
                [string]$authorityCapture.session_transcript_sha256 -ne [string]$transcriptEntries[0].sha256) {
                throw "External capture authority does not bind the '$kind' challenge/producer session for row '$cellId'."
            }
        }
        Assert-ExactStringSet @($manifestLane.source_evidence.entries | ForEach-Object { [string]$_.role }) `
            @($authorityCapture.source_entries | ForEach-Object { [string]$_.role }) `
            "external source role closure for row '$cellId' lane '$kind'"
        foreach ($sourceEntry in @($manifestLane.source_evidence.entries)) {
            $authorizedEntries = @($authorityCapture.source_entries | Where-Object { [string]$_.role -eq [string]$sourceEntry.role })
            if ($authorizedEntries.Count -ne 1 -or
                [string]$authorizedEntries[0].sha256 -ne [string]$sourceEntry.sha256 -or
                [long]$authorizedEntries[0].byte_length -ne [long]$sourceEntry.byte_length) {
                throw "External capture authority source '$($sourceEntry.role)' differs for row '$cellId' lane '$kind'."
            }
        }
        if ($kind -eq "viewer_display") {
            if ([string]::IsNullOrWhiteSpace([string]$authorityCapture.operator_id)) {
                throw "Viewer capture authority has no approved operator identity for row '$cellId'."
            }
            $operatorPath = [string]$rolePaths["row:${cellId}:${kind}:source:operator-observation.json"]
            $operatorObservation = Read-BoundedJson $operatorPath $maximumJsonBytes "authorized Viewer operator observation"
            if ([string]$operatorObservation.operator_id -ne [string]$authorityCapture.operator_id) {
                throw "Viewer operator identity is not approved by the external capture authority."
            }
        } elseif (-not [string]::IsNullOrWhiteSpace([string]$authorityCapture.operator_id)) {
            throw "Non-Viewer capture authority must not invent an operator identity."
        }
        Assert-AnchorSnapshotUnchanged
        & $ownerVerifierScriptBlock `
            -ExpectedKind $kind `
            -ReportPath $laneReportPath -RawEvidencePath $rawPath -CellObservationPath $cellPath `
            -RuntimeProfilePath $anchorProfilePath -BuildManifestPath $bundledBuildManifestPath `
            -MachineReportPath $machinePath -ArtifactManifestPath $rowManifestPath `
            -EvidenceClosurePath $anchorClosurePath -EvidenceBundleDirectory $anchorDirectory `
            -PolicyPath $anchorPolicyPath
        if (-not $?) { throw "Owner verifier failed for row '$cellId' lane '$kind'." }
        Assert-AnchorSnapshotUnchanged
    }
    if (@($cell.scenarios | Where-Object { [string]$_.scenario -eq "managed_icc" }).Count -eq 1) {
        $platformRaw = Read-BoundedJson ([string]$rolePaths["row:${cellId}:platform_probe:raw"]) `
            $maximumJsonBytes "platform ICC cross-lane evidence"
        $viewerRaw = Read-BoundedJson ([string]$rolePaths["row:${cellId}:viewer_display:raw"]) `
            $maximumJsonBytes "Viewer ICC cross-lane evidence"
        $platformManagedIcc = @($platformRaw.scenarios | Where-Object { [string]$_.scenario -eq "managed_icc" })
        $viewerManagedIcc = @($viewerRaw.scenarios | Where-Object { [string]$_.scenario -eq "managed_icc" })
        if ($platformManagedIcc.Count -ne 1 -or $viewerManagedIcc.Count -ne 1 -or
            [string]$platformManagedIcc[0].icc_profile_sha256 -notmatch '^[0-9a-f]{64}$' -or
            [string]$platformManagedIcc[0].icc_profile_sha256 -ne [string]$viewerManagedIcc[0].icc_profile_sha256 -or
            [string]$viewerManagedIcc[0].icc_processor_sha256 -notmatch '^[0-9a-f]{64}$') {
            throw "Platform and Viewer lanes did not replay the same ICC profile/processor for row '$cellId'."
        }
    }
}
$buildArtifactKeys = @($bundledBuildManifest.artifacts | ForEach-Object {
    "$($_.platform)/$($_.target_triple)/$($_.package_kind)"
})
Assert-ExactStringSet $buildArtifactKeys @($usedBuildArtifactKeys | Sort-Object -Unique) `
    "bundled cross-target build artifact closure"

$replayDirectory = Join-Path ([IO.Path]::GetTempPath()) ("mondrian-platform-matrix-replay-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $replayDirectory -ErrorAction Stop | Out-Null
$replayReportPath = Join-Path $replayDirectory "qualification-report.json"
$replayStdoutPath = Join-Path $replayDirectory "qualification.stdout.log"
$replayStderrPath = Join-Path $replayDirectory "qualification.stderr.log"
try {
    $replayExitCode = Invoke-BoundedProcess ([string]$platformQualificationReplayTool.path) @(
        $anchorProfilePath, $snapshotResolvedPath, $replayReportPath
    ) $replayDirectory ([int]$expectedPolicy.runner.timeout_seconds) $replayStdoutPath $replayStderrPath
    if ($replayExitCode -ne 0) { throw "Platform matrix evaluator replay failed with exit code $replayExitCode." }
    $replayedReport = Read-BoundedJson $replayReportPath $maximumJsonBytes "replayed qualification report"
    $replayAdmission = $jsonAdmissions[[IO.Path]::GetFullPath($replayReportPath)]
    $bundledReportAdmission = $jsonAdmissions[[IO.Path]::GetFullPath($reportPath)]
    if ([string]$replayAdmission.sha256 -ne [string]$bundledReportAdmission.sha256 -or
        [string]$replayedReport.evidence_sha256 -ne [string]$report.evidence_sha256) {
        throw "Replayed Matrix evaluator report differs from the bundled qualification verdict."
    }
} finally {
    if (Test-Path -LiteralPath $replayDirectory) {
        Remove-Item -LiteralPath $replayDirectory -Recurse -Force
    }
}

Assert-AnchorSnapshotUnchanged

foreach ($admission in $bundleEvidenceAdmissions) {
    $current = Get-BoundedFile ([string]$admission.source_path) $maximumArtifactBytes "bundled evidence final recheck"
    if ($current.sha256 -ne [string]$admission.sha256 -or $current.length -ne [long]$admission.length) {
        throw "Bundled evidence changed during verification."
    }
}
foreach ($topLevel in @(
    [pscustomobject]@{ path = $sealedPath; label = "sealed matrix" },
    [pscustomobject]@{ path = $closurePath; label = "evidence closure" },
    [pscustomobject]@{ path = $resolvedPath; label = "resolved campaign" },
    [pscustomobject]@{ path = $reportPath; label = "qualification report" }
)) {
    $current = Get-BoundedFile ([string]$topLevel.path) $maximumJsonBytes "$($topLevel.label) final recheck"
    $admitted = $jsonAdmissions[[IO.Path]::GetFullPath([string]$topLevel.path)]
    if ($current.sha256 -ne [string]$admitted.sha256 -or $current.length -ne [long]$admitted.length) {
        throw "$($topLevel.label) changed during verification."
    }
}
foreach ($log in @($stdoutAdmission, $stderrAdmission)) {
    $current = Get-BoundedFile ([string]$log.path) $maximumArtifactBytes "qualification log final recheck" $true
    if ($current.sha256 -ne [string]$log.sha256 -or $current.length -ne [long]$log.length) {
        throw "Qualification log changed during verification."
    }
}

$endHeadSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $endHeadSha -ne $expectedSource -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Source changed or became dirty during platform qualification bundle verification."
}
Assert-AnchorSnapshotUnchanged
foreach ($name in $previousReplayEnvironment.Keys) {
    [Environment]::SetEnvironmentVariable($name, $previousReplayEnvironment[$name], "Process")
}
foreach ($name in @(
    "MondrianQualificationSourceVerifierScriptBlock", "MondrianQualificationPlatformProducerSha256",
    "MondrianQualificationGpuProducerSha256", "MondrianQualificationGpuProfileAdmission"
)) { Remove-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue }
foreach ($lock in $anchorReadLocks) { $lock.Dispose() }
Remove-Item -LiteralPath $anchorDirectory -Recurse -Force

Write-Host "Platform/driver/display qualification bundle: verified"
