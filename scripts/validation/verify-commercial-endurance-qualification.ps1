param(
    [string]$PreloaderReportPath,
    [string]$ExpectedPreloaderReportSha256,
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
Import-Module (Join-Path $PSScriptRoot "window-owner-closure.psm1") -Force
Import-Module (Join-Path $PSScriptRoot "native-preloader-closure.psm1") -Force
$script:ObservedJsonHashes = [System.Collections.Generic.Dictionary[string,string]]::new(
    [StringComparer]::Ordinal
)
$script:ObservedAncillaryHashes = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)

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

function Get-EnduranceMeasurementProjection($Object, $Phase, $Requirement, $MachineTimeouts) {
    if ('measurement_timing' -cnotin @($Object.PSObject.Properties.Name)) { throw 'Started phase has no owner-derived measurement timing' }
    $timing=$Object.measurement_timing
    Assert-PhaseMeasurementTiming $timing
    $startupMs=120000
    if ('startup_ms' -cin @($MachineTimeouts.PSObject.Properties | ForEach-Object { $_.Name })) { $startupMs=$MachineTimeouts.startup_ms }
    Assert-JsonUnsignedInteger $startupMs 'Startup timeout'
    if ([bigint]$startupMs -le 0 -or
        [bigint]$timing.startup_deadline_at_run_us - [bigint]$timing.startup_started_at_run_us -ne [bigint]$startupMs * 1000 -or
        [bigint]$timing.measurement_deadline_at_run_us - [bigint]$timing.measurement_started_at_run_us -ne [bigint]$Requirement.minimum_duration_us -or
        [bigint]$Phase.started_at_run_us -ne [bigint]$timing.measurement_started_at_run_us -or
        [bigint]$Phase.completed_at_run_us -lt [bigint]$timing.measurement_deadline_at_run_us) {
        throw 'Phase measurement window differs from original startup budget or full required duration'
    }
    $projection=[ordered]@{}
    foreach ($field in @('startup_started_at_run_us','startup_deadline_at_run_us','owners_ready_at_run_us','measurement_started_at_run_us','measurement_deadline_at_run_us')) { $projection[$field]=$timing.$field }
    return $projection | ConvertTo-Json -Compress
}
function Assert-EnduranceAncillaryBinding($Object, [string]$ExpectedSha256, [string]$Description) {
    $declared = 'ancillary_program_sha256' -cin @($Object.PSObject.Properties.Name)
    if ([string]::IsNullOrEmpty($ExpectedSha256)) {
        if ($declared -or 'ancillary_export_artifacts' -cin @($Object.PSObject.Properties.Name) -or 'wire_journals' -cin @($Object.PSObject.Properties.Name)) { throw "$Description invents an unapproved ancillary program." }
        return
    }
    if (-not $declared -or $Object.ancillary_program_sha256 -isnot [string] -or
        $Object.ancillary_program_sha256 -cne $ExpectedSha256) {
        throw "$Description differs from the approved shared ancillary program."
    }
}

function Get-EnduranceAncillaryProjection($Object) {
    return [ordered]@{ancillary_program_sha256=$Object.ancillary_program_sha256;ancillary_export_artifacts=$Object.ancillary_export_artifacts;wire_journals=$Object.wire_journals} | ConvertTo-Json -Depth 30 -Compress
}

function Read-EnduranceWireIdentity([string]$Path, [uint64]$MaximumBytes) {
    $stream = [IO.FileStream]::new($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -le 0 -or [uint64]$stream.Length -gt $MaximumBytes) { throw 'Wire journal exceeds its approved bound.' }
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false,$true), $false, 4096, $true)
        try {
            $line = [Text.StringBuilder]::new()
            for ($index=0; $index -lt 65536; $index++) {
                $character = $reader.Read()
                if ($character -eq -1) { throw 'Wire identity has no complete JSONL record.' }
                if ($character -eq 10) { return ConvertFrom-EnduranceStrictJson $line.ToString() }
                [void]$line.Append([char]$character)
            }
            throw 'Wire identity exceeds its bounded record size.'
        } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}

function Assert-EnduranceAncillaryFiles($Raw, $Events, $Plan, $Program, [string]$PhaseId, [string]$PhaseKind) {
    $paths = [Collections.Generic.List[string]]::new()
    $exports = @($Events | Where-Object { $_.kind -ceq 'export_artifact_verified' })
    if ($Raw.ancillary_export_artifacts -isnot [array] -or $Raw.ancillary_export_artifacts.Count -ne $exports.Count -or $exports.Count -gt 256) { throw 'Ancillary artifact inventory differs from actual verified events.' }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $exportPlan = @($Plan.exports | Where-Object { $_.phase_id -ceq $PhaseId })
    foreach ($entry in $Raw.ancillary_export_artifacts) {
        Assert-ExactJsonProperties $entry @('artifact_id','verification_path','verification_sha256') 'Ancillary verification file'
        if (-not $seen.Add([string]$entry.artifact_id) -or $exportPlan.Count -ne 1) { throw 'Ancillary artifact is repeated or has no exact export plan.' }
        $matching = @($exports | Where-Object { $_.artifact_id -ceq $entry.artifact_id })
        if ($matching.Count -ne 1) { throw 'Ancillary artifact has no unique verified event.' }
        $path = Resolve-ExistingLeaf $entry.verification_path 'Ancillary verification file'
        $directory = Resolve-ExistingDirectory $exportPlan[0].output_directory 'Approved export directory'
        if ((Split-Path -Parent $path) -cne $directory) { throw 'Ancillary verification file escaped the approved output directory.' }
        Assert-LowerSha256 $entry.verification_sha256 'Ancillary verification file digest'
        $sidecar = Read-BoundedJson $path 'Ancillary verification file' 2097152
        if ($script:ObservedJsonHashes[$path] -cne $entry.verification_sha256 -or $sidecar.schema_version -ne 1 -or $sidecar.status -cne 'verified') { throw 'Ancillary verification file hash or success state differs.' }
        Assert-EnduranceAncillaryBinding $sidecar.request $Raw.ancillary_program_sha256 'Verified export request'
        Assert-EnduranceAncillaryBinding $sidecar.ancillary_mxf_rescan $Raw.ancillary_program_sha256 'Actual MXF rescan'
        Assert-JsonUnsignedInteger $sidecar.ancillary_mxf_rescan.frames_verified 'MXF rescan frames'
        if ($sidecar.ancillary_mxf_rescan.frames_verified -ne $Program.frame_count -or $Program.frame_count -le 0) { throw 'MXF rescan did not verify the complete frozen program.' }
        if ($sidecar.request.artifact_id -cne $entry.artifact_id -or $sidecar.evidence.report.artifact_id -cne $entry.artifact_id -or $sidecar.evidence.validation_report_sha256 -cne $matching[0].validation_report_sha256 -or $sidecar.evidence.report.artifact_sha256 -cne $matching[0].artifact_sha256) { throw 'Ancillary verification differs from the actual artifact event.' }
        $artifact = Resolve-ExistingLeaf $sidecar.request.path 'Verified MXF artifact'
        if ((Split-Path -Parent $artifact) -cne $directory -or $path -cne ($artifact + '.independent-verification.json') -or (Get-LowerSha256 $artifact) -cne $matching[0].artifact_sha256) { throw 'Final MXF artifact differs from independent verification.' }
        [void]$paths.Add($path)
        [void]$paths.Add($artifact)
        $script:ObservedAncillaryHashes[$artifact] = [string]$matching[0].artifact_sha256
    }
    if ($Raw.wire_journals -isnot [array] -or $Raw.wire_journals.Count -gt 256 -or ($PhaseKind -ceq 'continuous_export' -and $Raw.wire_journals.Count -ne 0) -or ($PhaseKind -cne 'continuous_export' -and $Raw.wire_journals.Count -eq 0)) { throw 'Ancillary wire journal inventory differs from the physical phase.' }
    $journalPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $Raw.wire_journals) {
        Assert-ExactJsonProperties $entry @('path','sha256') 'Ancillary wire journal'
        $path = Resolve-ExistingLeaf $entry.path 'Ancillary wire journal'
        $wire = $Plan.reference_output.wire_readback
        $directory = Resolve-ExistingDirectory $wire.receipt_directory 'Approved wire journal directory'
        if (-not $journalPaths.Add($path) -or (Split-Path -Parent $path) -cne $directory) { throw 'Wire journal is repeated or escaped its approved directory.' }
        Assert-LowerSha256 $entry.sha256 'Wire journal digest'
        if ((Get-LowerSha256 $path) -cne $entry.sha256) { throw 'Closed wire journal digest differs.' }
        $identity = Read-EnduranceWireIdentity $path $wire.maximum_receipt_bytes
        Assert-EnduranceAncillaryBinding $identity $Raw.ancillary_program_sha256 'Physical wire session'
        if ($identity.schema_version -ne 1 -or $identity.event -cne 'session_identity' -or $identity.phase_id -cne $PhaseId) { throw 'Physical wire session identity differs.' }
        if ($identity.output_device -cne $Plan.reference_output.device_id -or $identity.output_generation -ne $Plan.reference_output.device_generation -or $identity.receiver_device -cne $wire.device_id -or $identity.receiver_generation -ne $wire.device_generation) { throw 'Wire journal differs from the approved physical devices.' }
        [void]$paths.Add($path)
        $script:ObservedAncillaryHashes[$path] = [string]$entry.sha256
    }
    return $paths.ToArray()
}

function Assert-EnduranceUniqueJsonProperties([System.Text.Json.JsonElement]$Element) {
    if ($Element.ValueKind -eq [System.Text.Json.JsonValueKind]::Object) {
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($property in $Element.EnumerateObject()) {
            if (-not $names.Add($property.Name)) { throw 'Endurance evidence contains duplicate or case-colliding JSON properties.' }
            Assert-EnduranceUniqueJsonProperties $property.Value
        }
    } elseif ($Element.ValueKind -eq [System.Text.Json.JsonValueKind]::Array) {
        foreach ($item in $Element.EnumerateArray()) { Assert-EnduranceUniqueJsonProperties $item }
    }
}

function ConvertFrom-EnduranceStrictJson([string]$Text) {
    $options = [System.Text.Json.JsonDocumentOptions]::new()
    $options.MaxDepth = 128
    $document = [System.Text.Json.JsonDocument]::Parse($Text, $options)
    try { Assert-EnduranceUniqueJsonProperties $document.RootElement } finally { $document.Dispose() }
    return $Text | ConvertFrom-Json -Depth 128 -DateKind String
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
    return ConvertFrom-EnduranceStrictJson $text
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
    if (@(Compare-Object $expectedSorted $actual -CaseSensitive).Count -ne 0) {
        throw "$Description has unknown or missing properties."
    }
}

function ConvertTo-WindowHistoryCanonicalValue([object]$Value) {
    if ($null -eq $Value) { return $null }
    if ($Value -is [System.Collections.IDictionary]) {
        $result = [ordered]@{}
        foreach ($key in @($Value.Keys | Sort-Object -CaseSensitive)) { $result[$key] = ConvertTo-WindowHistoryCanonicalValue $Value[$key] }
        return $result
    }
    if ($Value -is [pscustomobject]) {
        $result = [ordered]@{}
        foreach ($key in @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive)) { $result[$key] = ConvertTo-WindowHistoryCanonicalValue $Value.$key }
        return $result
    }
    if ($Value -is [Array]) {
        $result = @($Value | ForEach-Object { ConvertTo-WindowHistoryCanonicalValue $_ })
        return ,$result
    }
    return $Value
}

function ConvertTo-WindowHistoryCanonicalJson([object]$Value) {
    ConvertTo-Json -InputObject (ConvertTo-WindowHistoryCanonicalValue $Value) -Depth 100 -Compress
}

function Assert-CanonicalJson([string]$Json, [object]$Parsed, [string]$Description) {
    $normalized = ConvertTo-Json -InputObject $Parsed -Depth 100 -Compress
    if ($normalized -cne $Json) {
        throw "$Description is not the canonical compact JSON encoding."
    }
}

function Assert-JsonBoolean([object]$Value, [string]$Description) {
    if ($Value -isnot [bool]) {
        throw "$Description must be a JSON boolean."
    }
}

function Assert-JsonString([object]$Value, [string]$Description) {
    if ($Value -isnot [string]) {
        throw "$Description must be a JSON string."
    }
}

function Assert-JsonObject([object]$Value, [string]$Description) {
    if ($Value -isnot [pscustomobject]) {
        throw "$Description must be a JSON object."
    }
}

function Assert-JsonUnsignedInteger([object]$Value, [string]$Description) {
    $isInteger = $Value -is [byte] -or $Value -is [sbyte] -or
        $Value -is [int16] -or $Value -is [uint16] -or
        $Value -is [int32] -or $Value -is [uint32] -or
        $Value -is [int64] -or $Value -is [uint64]
    if (-not $isInteger -or [decimal]$Value -lt 0) {
        throw "$Description must be a nonnegative JSON integer."
    }
}

function Assert-JsonUnsignedU32([object]$Value, [string]$Description) {
    Assert-JsonUnsignedInteger $Value $Description
    if ([decimal]$Value -gt [uint32]::MaxValue) {
        throw "$Description must fit in an unsigned 32-bit integer."
    }
}

function Get-ClosureSnapshot([string[]]$Paths) {
    $rows = [System.Collections.Generic.List[object]]::new()
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($path in $Paths) {
        $absolute = [IO.Path]::GetFullPath($path)
        if (-not $seen.Add($absolute)) { continue }
        [void]$rows.Add([pscustomobject]@{
            path = $absolute
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
        throw "$Description changed before the immutable replay snapshot: $absolute"
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
$ancillaryProgramPath = $null
$ancillaryProgram = $null
$approvedToolPaths = [Collections.Generic.List[string]]::new()
$exportPresetPaths = [Collections.Generic.List[string]]::new()
$approvedBmx = $null
if ('bmx' -cin @($machinePlan.verifier_tools.PSObject.Properties.Name)) { $approvedBmx = $machinePlan.verifier_tools.bmx }
if ($null -ne $approvedBmx) {
    Assert-ExactJsonProperties $approvedBmx @('raw2bmx','mxf2raw','raw2bmx_version_output_sha256','mxf2raw_version_output_sha256','runtime_files') 'Approved BMX toolchain'
    Assert-LowerSha256 $approvedBmx.raw2bmx_version_output_sha256 'Approved raw2bmx version output'
    Assert-LowerSha256 $approvedBmx.mxf2raw_version_output_sha256 'Approved mxf2raw version output'
    if ($null -ne $approvedBmx.runtime_files -and ($approvedBmx.runtime_files -isnot [array] -or $approvedBmx.runtime_files.Count -gt 256)) { throw 'Approved BMX runtime closure exceeds its bound.' }
    foreach ($binding in @($approvedBmx.raw2bmx, $approvedBmx.mxf2raw) + @($approvedBmx.runtime_files | Where-Object { $null -ne $_ })) {
        Assert-ExactJsonProperties $binding @('path','sha256') 'Approved BMX file binding'
        Assert-LowerSha256 $binding.sha256 'Approved BMX file digest'
        $path = Resolve-ExistingLeaf $binding.path 'Approved BMX file'
        if ((Get-LowerSha256 $path) -cne $binding.sha256) { throw 'BMX file differs from external approval.' }
        $script:ObservedAncillaryHashes[$path] = [string]$binding.sha256
        [void]$approvedToolPaths.Add($path)
    }
}
$ancillaryProgramSha256 = ''
if ('ancillary_program' -cin @($machinePlan.PSObject.Properties.Name) -and $null -ne $machinePlan.ancillary_program) {
    $ancillaryBinding = $machinePlan.ancillary_program
    Assert-ExactJsonProperties $ancillaryBinding @('path','sha256') 'Shared ancillary program binding'
    Assert-JsonString $ancillaryBinding.path 'Shared ancillary program path'
    if (-not [IO.Path]::IsPathFullyQualified($ancillaryBinding.path)) { throw 'Shared ancillary program path must be absolute.' }
    $ancillaryProgramSha256 = [string]$ancillaryBinding.sha256
    Assert-LowerSha256 $ancillaryProgramSha256 'Shared ancillary program digest'
    $ancillaryProgramPath = Resolve-ExistingLeaf $ancillaryBinding.path 'Shared ancillary program'
    $ancillaryProgram = Read-BoundedJson $ancillaryProgramPath 'Shared ancillary program' 8388608
    Assert-JsonUnsignedInteger $ancillaryProgram.frame_count 'Shared ancillary program frame count'
    if ($ancillaryProgram.frame_count -eq 0 -or $ancillaryProgram.frame_count -gt 100000000) { throw 'Shared ancillary program duration is outside its declared bound.' }
    if ($script:ObservedJsonHashes[$ancillaryProgramPath] -cne $ancillaryProgramSha256) { throw 'Shared ancillary program differs from the externally approved machine plan.' }
}
$preloaderReceiptPath = $null
if (@($manifest.phases | Where-Object { [string]$_.terminal.status -cne 'not_run' }).Count -gt 0) {
    if ([string]::IsNullOrWhiteSpace($PreloaderReportPath) -or [string]::IsNullOrWhiteSpace($ExpectedPreloaderReportSha256)) {
        throw 'Started phases require an externally bound native pre-loader outer-owner report.'
    }
    Assert-LowerSha256 $ExpectedPreloaderReportSha256 'Native pre-loader report SHA-256'
    $preloaderReceiptPath = Resolve-ExistingLeaf $PreloaderReportPath 'Native pre-loader outer-owner report'
    $preloaderReport = Read-BoundedJson $preloaderReceiptPath 'Native pre-loader outer-owner report' 1048576
    if ($script:ObservedJsonHashes[$preloaderReceiptPath] -cne $ExpectedPreloaderReportSha256) {
        throw 'Native pre-loader report differs from external approval.'
    }
    Assert-NativePreloaderReport $preloaderReport $manifestPath (Get-LowerSha256 $manifestPath) $machinePlan $ExpectedMachinePlanSha256 $ExpectedRuntimeImageSha256
}
if ([int]$profileObject.schema_version -ne 1 -or [int]$manifest.schema_version -ne 4 -or
    [int]$captureAuthority.schema_version -ne 2 -or [int]$machinePlan.schema_version -ne 2) {
    throw "Endurance profile must use schema 1; run schema 4; authority and machine plan schema 2."
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
Import-Module (Join-Path $PSScriptRoot 'phase-owner-closure.psm1') -Force
$phaseOwnerNames = [System.Collections.Generic.List[string]]::new()
$phaseOwnerPaths = [System.Collections.Generic.List[string]]::new()
$phaseOwnerBodies = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
$startedOwnerPhases = @($manifest.phases | Where-Object { $_.terminal.status -cne 'not_run' })
if ($manifest.phase_owner_history -isnot [array] -or $manifest.phase_owner_history.Count -ne $startedOwnerPhases.Count) { throw 'Phase owner history does not match every started phase' }
for ($ordinal = 0; $ordinal -lt $startedOwnerPhases.Count; $ordinal++) {
    $phase = $startedOwnerPhases[$ordinal]
    $owner = $manifest.phase_owner_history[$ordinal]
    $requirement = @($profileObject.phases | Where-Object { $_.phase_id -ceq $phase.phase_id })
    if ($requirement.Count -ne 1) { throw 'Phase owner report has no exact profile requirement' }
    Assert-MondrianPhaseOwnerReceipt $owner $manifest.run_id $phase.phase_id $ordinal $requirement[0].kind
    $ownerBody = $owner.canonical_json | ConvertFrom-Json -Depth 80 -DateKind String
    $null = Get-EnduranceMeasurementProjection $ownerBody $phase $requirement[0] $machinePlan.timeouts
    if ($ordinal -gt 0 -and [bigint]$ownerBody.measurement_timing.startup_started_at_run_us -lt [bigint]$startedOwnerPhases[$ordinal-1].completed_at_run_us) {
        throw 'Phase preparation overlaps the previous phase owner lifetime'
    }
    Assert-EnduranceAncillaryBinding $ownerBody $ancillaryProgramSha256 'Complete phase owner report'
    $phaseOwnerBodies.Add([string]$phase.phase_id, $ownerBody)
    if ([string]$requirement[0].kind -cne 'playback_reference') {
        $exports = @($machinePlan.exports | Where-Object { $_.phase_id -ceq $phase.phase_id })
        if ($exports.Count -ne 1) { throw 'Started Export phase has no unique approved preset.' }
        $presetPath = Resolve-ExistingLeaf $exports[0].preset.path 'Approved phase export preset'
        $preset = Read-BoundedJson $presetPath 'Approved phase export preset' 262144
        if ($script:ObservedJsonHashes[$presetPath] -cne $exports[0].preset.sha256) { throw 'Started phase preset differs from external approval.' }
        [void]$exportPresetPaths.Add($presetPath)
        $requiresBmx = $preset.artifact.kind -ceq 'professional_delivery' -and $preset.artifact.profile -ceq 'as11_x9_naba_hd720p5994'
        if ($requiresBmx -and ($null -eq $approvedBmx -or $null -eq $approvedBmx.runtime_files -or 'bmx_runtime' -cnotin @($ownerBody.terminal.PSObject.Properties.Name) -or $null -eq $ownerBody.terminal.bmx_runtime)) { throw 'Started AS-11 phase has no approved, consumed BMX runtime.' }
    }
    $ownerPath = Resolve-ExistingLeaf $owner.report_path 'Complete phase owner report'
    if ((Split-Path -Parent $ownerPath) -cne $chunkRoot) { throw 'Phase owner report must belong to the closed evidence directory' }
    $ownerName = Split-Path -Leaf $ownerPath
    if ($phaseOwnerNames.Contains($ownerName)) { throw 'Repeated phase owner report path' }
    [void]$phaseOwnerNames.Add($ownerName)
    [void]$phaseOwnerPaths.Add($ownerPath)
    $published = Read-BoundedJson $ownerPath 'Complete phase owner report' 4194304
    if (($published | ConvertTo-Json -Depth 80 -Compress) -cne ($owner | ConvertTo-Json -Depth 80 -Compress)) { throw 'Durable phase owner report differs from manifest history' }
}
$closure = $manifest.owner_closure
Assert-JsonObject $closure 'Run owner closure'
if ([string]$closure.kind -ceq 'with_ffmpeg') {
    $fields = @($closure.PSObject.Properties.Name)
    if ($fields.Count -ne 3 -or 'kind' -cnotin $fields -or 'surface' -cnotin $fields -or 'ffmpeg' -cnotin $fields) {
        throw 'Combined owner closure must retain exactly Surface and FFmpeg owners.'
    }
    $capsule = $closure.ffmpeg
    Assert-JsonObject $capsule 'FFmpeg capsule closure'
    $expected = @('namespace_seal_verified', 'children_admitted', 'children_settled', 'children_remaining', 'children_abandoned', 'child_cleanup_failures', 'deadline_exceeded', 'capsule_removed', 'cleanup_error')
    $actual = @($capsule.PSObject.Properties.Name)
    if ($actual.Count -ne $expected.Count -or @($actual | Where-Object { $_ -cnotin $expected }).Count -ne 0) {
        throw 'FFmpeg capsule closure fields differ from the exact shared contract.'
    }
    foreach ($field in @('namespace_seal_verified', 'deadline_exceeded', 'capsule_removed')) { Assert-JsonBoolean $capsule.$field "Capsule $field" }
    foreach ($field in @('children_admitted', 'children_settled', 'children_remaining', 'children_abandoned')) { Assert-JsonUnsignedInteger $capsule.$field "Capsule $field" }
    if ($capsule.child_cleanup_failures -isnot [System.Array] -or $capsule.child_cleanup_failures.Count -gt 256) { throw 'Capsule child cleanup facts are not a bounded array.' }
    foreach ($childFailure in $capsule.child_cleanup_failures) {
        Assert-JsonObject $childFailure 'Capsule native child cleanup'
        $expectedChildFields = @('child_pid','native_exit_observed','kill_error','wait_error','deadline_exceeded','stdin_error','stdout_error','stderr_error')
        $actualChildFields = @($childFailure.PSObject.Properties.Name)
        if ($actualChildFields.Count -ne $expectedChildFields.Count -or @($actualChildFields | Where-Object { $_ -cnotin $expectedChildFields }).Count -ne 0) { throw 'Capsule child cleanup has missing or unknown raw fields.' }
        Assert-JsonUnsignedInteger $childFailure.child_pid 'Capsule child PID'
        foreach ($field in @('native_exit_observed','deadline_exceeded')) { Assert-JsonBoolean $childFailure.$field "Child cleanup $field" }
        foreach ($field in @('kill_error','wait_error','stdin_error','stdout_error','stderr_error')) {
            if ($null -ne $childFailure.$field -and ($childFailure.$field -isnot [string] -or $childFailure.$field.Length -eq 0)) { throw "Child cleanup $field is not null or original error text." }
        }
    }
    if (-not $capsule.namespace_seal_verified -or -not $capsule.capsule_removed -or $capsule.deadline_exceeded -or
        [decimal]$capsule.children_admitted -ne [decimal]$capsule.children_settled -or
        [decimal]$capsule.children_remaining -ne 0 -or [decimal]$capsule.children_abandoned -ne 0 -or
        $capsule.child_cleanup_failures -isnot [System.Array] -or @($capsule.child_cleanup_failures).Count -ne 0 -or $null -ne $capsule.cleanup_error) {
        throw 'FFmpeg capsule has incomplete native owner/namespace cleanup evidence.'
    }
    $closure = $closure.surface
    Assert-JsonObject $closure 'Surface owner closure'
} elseif (@($manifest.phases | Where-Object { [string]$_.terminal.status -cne 'not_run' }).Count -ne 0) {
    throw 'A started phase requires the exact-runtime capsule closure.'
}
$closureProperties = @($closure.PSObject.Properties.Name)
if ([string]$closure.kind -ceq "event_loop") {
    if ($closureProperties.Count -ne 2 -or "kind" -notin $closureProperties -or
        "closure" -notin $closureProperties) {
        throw "Endurance EventLoop owner closure has unexpected fields."
    }
    $eventLoopClosure = $closure.closure
    $eventLoopProperties = @($eventLoopClosure.PSObject.Properties.Name)
    if ($eventLoopProperties.Count -ne 3 -or
        "schema_version" -notin $eventLoopProperties -or
        "rust_owner_released" -notin $eventLoopProperties -or
        "physical_native_termination_verified" -notin $eventLoopProperties -or
        [int]$eventLoopClosure.schema_version -ne 1 -or
        $eventLoopClosure.rust_owner_released -ne $true -or
        $eventLoopClosure.physical_native_termination_verified -ne $false) {
        throw "Endurance EventLoop owner closure is invalid."
    }
} elseif ([string]$closure.kind -ceq "not_applicable") {
    if ($closureProperties.Count -ne 1 -or "kind" -notin $closureProperties) {
        throw "Endurance NotApplicable owner closure has unexpected fields."
    }
    $startedConcurrentRecovery = $false
    foreach ($profilePhase in @($profileObject.phases | Where-Object {
        [string]$_.kind -ceq "concurrent_recovery"
    })) {
        $matchingRunPhases = @($manifest.phases | Where-Object {
            [string]$_.phase_id -ceq [string]$profilePhase.phase_id
        })
        if ($matchingRunPhases.Count -eq 1 -and
            [string]$matchingRunPhases[0].terminal.status -cne "not_run") {
            $startedConcurrentRecovery = $true
        }
    }
    if ($startedConcurrentRecovery) {
        throw "A started Concurrent Recovery phase requires EventLoop owner closure evidence."
    }
} else {
    throw "Endurance run owner closure kind is unsupported."
}

$declaredNames = [System.Collections.Generic.List[string]]::new()
$evidencePaths = [System.Collections.Generic.List[string]]::new()
foreach ($name in $phaseOwnerNames) { [void]$declaredNames.Add($name) }
foreach ($path in $phaseOwnerPaths) { [void]$evidencePaths.Add($path) }
foreach ($path in @($approvedToolPaths) + @($exportPresetPaths)) { [void]$evidencePaths.Add($path) }
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
    if ([string]$phase.terminal.status -cne 'not_run') {
        $measurement = Get-EnduranceMeasurementProjection $rawEvidence $phase $profilePhase $machinePlan.timeouts
        foreach ($copy in @($producerReport, $phase.producer, $phaseOwnerBodies[[string]$phase.phase_id])) {
            if ((Get-EnduranceMeasurementProjection $copy $phase $profilePhase $machinePlan.timeouts) -cne $measurement) { throw 'Owner-derived measurement timing differs across sealed phase reports' }
        }
        Assert-EnduranceAncillaryBinding $producerReport $ancillaryProgramSha256 'Endurance producer report'
        Assert-EnduranceAncillaryBinding $rawEvidence $ancillaryProgramSha256 'Endurance raw producer evidence'
        Assert-EnduranceAncillaryBinding $phase.producer $ancillaryProgramSha256 'Manifest producer'
        if (-not [string]::IsNullOrEmpty($ancillaryProgramSha256)) {
            $projection = Get-EnduranceAncillaryProjection $rawEvidence
            foreach ($copy in @($producerReport, $phase.producer, $phaseOwnerBodies[[string]$phase.phase_id])) {
                if ((Get-EnduranceAncillaryProjection $copy) -cne $projection) { throw 'Shared ancillary owner inventories differ across sealed phase reports.' }
            }
            foreach ($path in @(Assert-EnduranceAncillaryFiles $rawEvidence @($rawEvidence.events) $machinePlan $ancillaryProgram ([string]$phase.phase_id) ([string]$profilePhase.kind))) {
                [void]$evidencePaths.Add($path)
            }
        }
    }
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
                Assert-CanonicalJson $receiptJson $receipt "Endurance recovery receipt"
                Assert-JsonUnsignedInteger $receipt.schema_version "Recovery receipt schema"
                Assert-JsonUnsignedInteger $receipt.cycle_index "Recovery receipt cycle"
                Assert-JsonString $receipt.step "Recovery receipt step"
                Assert-JsonString $receipt.operation_id "Recovery receipt operation identity"
                $expectedReceiptSchema = if ($expectedStep -ceq "surface_device_reopen") { 4 } else { 3 }
                if ([int]$receipt.schema_version -ne $expectedReceiptSchema -or
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
                            Assert-CanonicalJson $windowRunJson $windowRun "Window-run receipt"
                            Assert-JsonUnsignedInteger $windowRun.schema_version "Window-run schema"
                            Assert-JsonString $windowRun.outcome "Window-run outcome"
                            Assert-JsonString $windowRun.recovery_receipt_json "Window-run recovery JSON"
                            Assert-JsonString $windowRun.recovery_receipt_sha256 "Window-run recovery digest"
                            Assert-ExactJsonProperties $windowRun @(
                                "schema_version", "outcome", "recovery_receipt_json",
                                "recovery_receipt_sha256", "runtime_shutdown_json",
                                "runtime_shutdown_sha256", "host_shutdown_json",
                                "host_shutdown_sha256", "gpu_shutdown_json",
                                "gpu_shutdown_sha256", "native_return_json",
                                "native_return_sha256", "generation_history"
                            ) "Window-run receipt"
                            if ([int]$windowRun.schema_version -ne 3 -or
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
                                $leafValue = $leaf.Json | ConvertFrom-Json
                                Assert-CanonicalJson $leaf.Json $leafValue "Window-run embedded receipt"
                            }
                            Assert-MondrianWindowOwnerClosure `
                                ($windowRun.runtime_shutdown_json | ConvertFrom-Json) `
                                ($windowRun.host_shutdown_json | ConvertFrom-Json)
                            $nativeReturn = ([string]$windowRun.native_return_json) | ConvertFrom-Json
                            Assert-ExactJsonProperties $nativeReturn @(
                                "event_loop_borrow_returned", "window_owner_scope_exited",
                                "physical_native_termination"
                            ) "Window native-return evidence"
                            Assert-JsonBoolean $nativeReturn.event_loop_borrow_returned "Window event-loop handback"
                            Assert-JsonBoolean $nativeReturn.window_owner_scope_exited "Window owner-scope return"
                            Assert-JsonString $nativeReturn.physical_native_termination "Window physical native termination"
                            if (-not [bool]$nativeReturn.event_loop_borrow_returned -or
                                -not [bool]$nativeReturn.window_owner_scope_exited -or
                                [string]$nativeReturn.physical_native_termination -cne "unverified") {
                                throw "Window native-return evidence overclaims or omits authority release."
                            }
                            $finalGpu = ([string]$windowRun.gpu_shutdown_json) | ConvertFrom-Json
                            Assert-ExactJsonProperties $finalGpu @(
                                "surface_generation", "device_generation",
                                "publication_cleanup", "retirement"
                            ) "Window final GPU shutdown evidence"
                            Assert-JsonUnsignedInteger $finalGpu.surface_generation "Window final Surface generation"
                            Assert-JsonUnsignedInteger $finalGpu.device_generation "Window final Device generation"
                            Assert-ExactJsonProperties $finalGpu.publication_cleanup @("Ok") "Window GPU publication cleanup"
                            Assert-ExactJsonProperties $finalGpu.retirement @("retired") "Window GPU retirement outcome"
                            $finalRetirement = $finalGpu.retirement.retired
                            Assert-ExactJsonProperties $finalRetirement @(
                                "worker_shutdown", "wake_callbacks", "native_wake_failures", "wake_registration_rejections",
                                "worker_started", "worker_terminated", "worker_panicked",
                                "timed_out", "retirement_requested", "retirement_handoff_accepted",
                                "retirement_completed", "renderer_retirement",
                                "generation_terminal_kind"
                            ) "Window final GPU retirement receipt"
                            Assert-MondrianGpuWakeClosure $finalRetirement $true
                            foreach ($field in @(
                                "worker_started", "worker_terminated", "worker_panicked", "timed_out",
                                "retirement_requested", "retirement_handoff_accepted", "retirement_completed"
                            )) {
                                Assert-JsonBoolean $finalRetirement.$field "Window final GPU $field"
                            }
                            Assert-ExactJsonProperties $finalRetirement.renderer_retirement @(
                                "cpu_yuv_upload", "native_device_removed"
                            ) "Window final Renderer retirement receipt"
                            Assert-JsonString $finalRetirement.renderer_retirement.cpu_yuv_upload "Window final GPU upload-worker exit"
                            Assert-JsonBoolean $finalRetirement.renderer_retirement.native_device_removed "Window final native-device removal"
                            if ([uint64]$finalGpu.surface_generation -ne [uint64]$receipt.surface_generation_after -or
                                [uint64]$finalGpu.device_generation -ne [uint64]$receipt.device_generation_after -or
                                $null -ne $finalGpu.publication_cleanup.Ok -or
                                -not [bool]$finalRetirement.worker_started -or
                                -not [bool]$finalRetirement.worker_terminated -or
                                [bool]$finalRetirement.worker_panicked -or [bool]$finalRetirement.timed_out -or
                                -not [bool]$finalRetirement.retirement_requested -or
                                -not [bool]$finalRetirement.retirement_handoff_accepted -or
                                -not [bool]$finalRetirement.retirement_completed -or
                                [string]$finalRetirement.renderer_retirement.cpu_yuv_upload -cne "returned" -or
                                [bool]$finalRetirement.renderer_retirement.native_device_removed -or
                                $null -ne $finalRetirement.generation_terminal_kind) {
                                throw "Window final GPU retirement is not bound to the recovered generation."
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
                        Assert-JsonString $receipt.sequence_binding_sha256 "Reopen Sequence binding"
                        Assert-JsonUnsignedInteger $receipt.surface_generation_before "Recovery old Surface generation"
                        Assert-JsonUnsignedInteger $receipt.surface_generation_after "Recovery new Surface generation"
                        Assert-JsonUnsignedInteger $receipt.device_generation_before "Recovery old Device generation"
                        Assert-JsonUnsignedInteger $receipt.device_generation_after "Recovery new Device generation"
                        Assert-JsonString $receipt.shutdown_receipt_json "Recovery old shutdown JSON"
                        Assert-JsonString $receipt.shutdown_receipt_sha256 "Recovery old shutdown digest"
                        Assert-JsonString $receipt.reopened_contract_json "Recovery reopened contract JSON"
                        Assert-JsonString $receipt.reopened_contract_sha256 "Recovery reopened contract digest"
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
                        Assert-CanonicalJson $shutdownJson $shutdown "Surface/device shutdown evidence"
                        Assert-CanonicalJson $reopenedJson $reopened "Reopened Surface contract"
                        Assert-ExactJsonProperties $shutdown @(
                            "worker_shutdown", "wake_callbacks", "native_wake_failures", "wake_registration_rejections",
                            "schema_version", "surface_generation", "device_generation",
                            "worker_started", "worker_terminated",
                            "worker_panicked", "timed_out", "retirement_requested",
                            "retirement_handoff_accepted", "retirement_completed",
                            "renderer_retirement", "generation_terminal_kind"
                        ) "Surface/device shutdown evidence"
                        Assert-MondrianGpuWakeClosure $shutdown $true
                        Assert-JsonUnsignedInteger $shutdown.schema_version "Surface shutdown schema"
                        Assert-JsonUnsignedInteger $shutdown.surface_generation "Old Surface generation"
                        Assert-JsonUnsignedInteger $shutdown.device_generation "Old Device generation"
                        foreach ($field in @(
                            "worker_started", "worker_terminated", "worker_panicked", "timed_out",
                            "retirement_requested", "retirement_handoff_accepted", "retirement_completed"
                        )) {
                            Assert-JsonBoolean $shutdown.$field "Old GPU $field"
                        }
                        Assert-ExactJsonProperties $shutdown.renderer_retirement @(
                            "cpu_yuv_upload", "native_device_removed"
                        ) "Surface/device Renderer retirement receipt"
                        Assert-JsonString $shutdown.renderer_retirement.cpu_yuv_upload "Old GPU upload-worker exit"
                        Assert-JsonBoolean $shutdown.renderer_retirement.native_device_removed "Old native-device removal"
                        if ($null -ne $windowRun) {
                            $history = $windowRun.generation_history
                            Assert-ExactJsonProperties $history @('schema_version', 'overflowed', 'events') 'Window generation history'
                            Assert-JsonUnsignedU32 $history.schema_version 'Window history schema'
                            Assert-JsonBoolean $history.overflowed 'Window history overflow'
                            if ($history.schema_version -ne 1 -or $history.overflowed -or
                                $history.events -isnot [Array] -or $history.events.Count -ne 4) {
                                throw 'Window history is incomplete or overflowed'
                            }
                            $windowHistoryEvents = $history.events
                            for ($i = 0; $i -lt 2; $i++) {
                                Assert-ExactJsonProperties $windowHistoryEvents[$i] @('event', 'surface_generation', 'device_generation') 'Window generation activation'
                                Assert-JsonUnsignedInteger $windowHistoryEvents[$i].surface_generation 'Window history Surface'
                                Assert-JsonUnsignedInteger $windowHistoryEvents[$i].device_generation 'Window history Device'
                            }
                            for ($i = 2; $i -lt 4; $i++) {
                                Assert-ExactJsonProperties $windowHistoryEvents[$i] @('event', 'shutdown') 'Window generation retirement'
                            }
                            if ($windowHistoryEvents[0].event -cne 'began' -or $windowHistoryEvents[1].event -cne 'activated' -or
                                $windowHistoryEvents[2].event -cne 'retired' -or $windowHistoryEvents[3].event -cne 'final' -or
                                $windowHistoryEvents[0].surface_generation -ne $receipt.surface_generation_before -or
                                $windowHistoryEvents[0].device_generation -ne $receipt.device_generation_before -or
                                $windowHistoryEvents[1].surface_generation -ne $receipt.surface_generation_after -or
                                $windowHistoryEvents[1].device_generation -ne $receipt.device_generation_after) {
                                throw 'Window generation order or identity does not match recovery'
                            }
                            $historyOld = $windowHistoryEvents[2].shutdown
                            Assert-ExactJsonProperties $historyOld @('surface_generation', 'device_generation', 'publication_cleanup', 'retirement') 'Window old generation history'
                            Assert-JsonUnsignedInteger $historyOld.surface_generation 'Window old history Surface'
                            Assert-JsonUnsignedInteger $historyOld.device_generation 'Window old history Device'
                            Assert-ExactJsonProperties $historyOld.publication_cleanup @('Ok') 'Window old publication cleanup'
                            Assert-ExactJsonProperties $historyOld.retirement @('retired') 'Window old retirement'
                            if ($null -ne $historyOld.publication_cleanup.Ok -or
                                $historyOld.surface_generation -ne $shutdown.surface_generation -or
                                $historyOld.device_generation -ne $shutdown.device_generation) {
                                throw 'Window old generation history has dirty publication or wrong identity'
                            }
                            $expectedOld = [ordered]@{}
                            foreach ($field in $shutdown.PSObject.Properties.Name) {
                                if ($field -cnotin @('schema_version', 'surface_generation', 'device_generation')) { $expectedOld[$field] = $shutdown.$field }
                            }
                            if ((ConvertTo-WindowHistoryCanonicalJson $historyOld.retirement.retired) -cne (ConvertTo-WindowHistoryCanonicalJson $expectedOld) -or
                                (ConvertTo-WindowHistoryCanonicalJson $windowHistoryEvents[3].shutdown) -cne (ConvertTo-WindowHistoryCanonicalJson $finalGpu)) {
                                throw 'Window history raw retirement differs from the independent GPU receipts'
                            }
                        }
                        Assert-ExactJsonProperties $reopened @(
                            "schema_version", "surface_generation", "device_generation",
                            "actual_surface_presented", "original_picture_sha256",
                            "reopened_picture_json", "reopened_picture_sha256"
                        ) "Reopened Surface contract"
                        Assert-JsonUnsignedInteger $reopened.schema_version "Reopened Surface schema"
                        Assert-JsonUnsignedInteger $reopened.surface_generation "Reopened Surface generation"
                        Assert-JsonUnsignedInteger $reopened.device_generation "Reopened Device generation"
                        Assert-JsonBoolean $reopened.actual_surface_presented "Reopened actual Surface presentation"
                        Assert-JsonString $reopened.original_picture_sha256 "Original Surface picture digest"
                        Assert-JsonString $reopened.reopened_picture_json "Reopened Surface picture JSON"
                        Assert-JsonString $reopened.reopened_picture_sha256 "Reopened Surface picture digest"
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
                        Assert-CanonicalJson $pictureJson $picture "Reopened Surface picture contract"
                        Assert-ExactJsonProperties $picture @(
                            "sequence_id", "frame", "width", "height", "output_target",
                            "output_color_space", "monitor_color_space", "tone_map",
                            "display_view", "frame_residency", "display_contract_sha256"
                        ) "Reopened Surface picture contract"
                        Assert-JsonString $picture.sequence_id "Reopened picture Sequence identity"
                        Assert-JsonUnsignedInteger $picture.frame "Reopened picture frame"
                        Assert-JsonUnsignedU32 $picture.width "Reopened picture width"
                        Assert-JsonUnsignedU32 $picture.height "Reopened picture height"
                        Assert-JsonString $picture.output_target "Reopened picture output target"
                        Assert-JsonString $picture.output_color_space "Reopened picture output color space"
                        Assert-JsonString $picture.monitor_color_space "Reopened picture monitor color space"
                        Assert-JsonBoolean $picture.tone_map "Reopened picture tone-map flag"
                        Assert-JsonBoolean $picture.frame_residency.execution_observed "Reopened picture execution observation"
                        Assert-JsonString $picture.frame_residency.working_residency "Reopened picture working residency"
                        if ($null -ne $picture.display_view) {
                            Assert-JsonObject $picture.display_view "Reopened picture display/view"
                            Assert-ExactJsonProperties $picture.display_view @("display", "view") "Reopened picture display/view"
                            Assert-JsonString $picture.display_view.display "Reopened picture display"
                            Assert-JsonString $picture.display_view.view "Reopened picture view"
                        }
                        Assert-JsonObject $picture.frame_residency "Reopened picture frame residency"
                        $residencyProperties = @(
                            "decode_residency", "working_residency", "input_transform_path",
                            "execution_observed", "zero_copy", "low_copy", "upload_count",
                            "native_bridge_copy_count", "readback_count", "reason"
                        )
                        if (@($picture.frame_residency.PSObject.Properties.Name) -ccontains "native_video_import") {
                            $residencyProperties += "native_video_import"
                        }
                        Assert-ExactJsonProperties $picture.frame_residency $residencyProperties "Reopened picture frame residency"
                        Assert-JsonString $picture.frame_residency.decode_residency "Reopened picture decode residency"
                        Assert-JsonString $picture.frame_residency.input_transform_path "Reopened picture input-transform path"
                        Assert-JsonBoolean $picture.frame_residency.zero_copy "Reopened picture zero-copy flag"
                        Assert-JsonBoolean $picture.frame_residency.low_copy "Reopened picture low-copy flag"
                        Assert-JsonUnsignedU32 $picture.frame_residency.upload_count "Reopened picture upload count"
                        Assert-JsonUnsignedU32 $picture.frame_residency.native_bridge_copy_count "Reopened picture native bridge-copy count"
                        Assert-JsonUnsignedU32 $picture.frame_residency.readback_count "Reopened picture readback count"
                        Assert-JsonString $picture.frame_residency.reason "Reopened picture residency reason"
                        if (@($picture.frame_residency.PSObject.Properties.Name) -ccontains "native_video_import") {
                            $nativeImport = $picture.frame_residency.native_video_import
                            Assert-JsonObject $nativeImport "Reopened picture native-import evidence"
                            $nativeImportProperties = @(
                                "status", "zero_copy_ready", "low_copy_ready",
                                "decoder_gpu_resident", "decoder_handle_kind",
                                "renderer_backend_ready", "renderer_supports_handle_kind",
                                "renderer_supports_source_texture_format", "reason"
                            )
                            foreach ($optionalField in @(
                                "renderer_backend_label", "renderer_unavailable_reason",
                                "renderer_import_mode"
                            )) {
                                if (@($nativeImport.PSObject.Properties.Name) -ccontains $optionalField) {
                                    $nativeImportProperties += $optionalField
                                    Assert-JsonString $nativeImport.$optionalField "Native-import $optionalField"
                                }
                            }
                            Assert-ExactJsonProperties $nativeImport $nativeImportProperties "Reopened picture native-import evidence"
                            Assert-JsonString $nativeImport.status "Native-import status"
                            Assert-JsonBoolean $nativeImport.zero_copy_ready "Native-import zero-copy readiness"
                            Assert-JsonBoolean $nativeImport.low_copy_ready "Native-import low-copy readiness"
                            Assert-JsonBoolean $nativeImport.decoder_gpu_resident "Native-import decoder residency"
                            if ($null -ne $nativeImport.decoder_handle_kind) {
                                Assert-JsonString $nativeImport.decoder_handle_kind "Native-import decoder handle"
                            }
                            Assert-JsonBoolean $nativeImport.renderer_backend_ready "Native-import Renderer readiness"
                            Assert-JsonBoolean $nativeImport.renderer_supports_handle_kind "Native-import handle support"
                            Assert-JsonBoolean $nativeImport.renderer_supports_source_texture_format "Native-import texture-format support"
                            Assert-JsonString $nativeImport.reason "Native-import reason"
                        }
                        Assert-JsonString $picture.display_contract_sha256 "Reopened display contract digest"
                        Assert-LowerSha256 ([string]$picture.display_contract_sha256) "Reopened display contract"
                        if ([uint64]$receipt.surface_generation_before -eq 0 -or
                            [uint64]$receipt.surface_generation_after -eq 0 -or
                            [uint64]$receipt.surface_generation_before -eq [uint64]$receipt.surface_generation_after -or
                            [uint64]$receipt.device_generation_before -eq 0 -or
                            [uint64]$receipt.device_generation_after -eq 0 -or
                            [uint64]$receipt.device_generation_before -eq [uint64]$receipt.device_generation_after -or
                            [int]$shutdown.schema_version -ne 4 -or
                            [uint64]$shutdown.surface_generation -ne [uint64]$receipt.surface_generation_before -or
                            [uint64]$shutdown.device_generation -ne [uint64]$receipt.device_generation_before -or
                            -not [bool]$shutdown.worker_started -or
                            -not [bool]$shutdown.worker_terminated -or [bool]$shutdown.worker_panicked -or
                            [bool]$shutdown.timed_out -or -not [bool]$shutdown.retirement_requested -or
                            -not [bool]$shutdown.retirement_handoff_accepted -or
                            -not [bool]$shutdown.retirement_completed -or
                            [string]$shutdown.renderer_retirement.cpu_yuv_upload -cne "returned" -or
                            [bool]$shutdown.renderer_retirement.native_device_removed -or
                            $null -ne $shutdown.generation_terminal_kind -or
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
$replayInputs = @($preloaderReceiptPath, $ancillaryProgramPath | Where-Object { $null -ne $_ }) + @($profile, $manifestPath, $replayBinary, $captureAuthorityPath, $machinePlanPath) + @($evidencePaths)
$before = Get-ClosureSnapshot $replayInputs
foreach ($observed in $script:ObservedJsonHashes.GetEnumerator()) {
    Assert-SnapshotAnchor $before $observed.Key $observed.Value "Parsed endurance JSON input"
}
foreach ($observed in $script:ObservedAncillaryHashes.GetEnumerator()) {
    Assert-SnapshotAnchor $before $observed.Key $observed.Value 'Approved ancillary artifact or wire journal'
}
Assert-SnapshotAnchor $before $replayBinary $ReplayBinarySha256 "Endurance replay binary"
Assert-SnapshotAnchor $before $profile $ExpectedProfileFileSha256 "Endurance profile"
Assert-SnapshotAnchor $before $captureAuthorityPath $ExpectedCaptureAuthoritySha256 "Endurance capture authority"
Assert-SnapshotAnchor $before $machinePlanPath $ExpectedMachinePlanSha256 "Endurance machine plan"
if ($null -ne $ancillaryProgramPath) { Assert-SnapshotAnchor $before $ancillaryProgramPath $ancillaryProgramSha256 'Shared ancillary program' }
if ($null -ne $preloaderReceiptPath) { Assert-SnapshotAnchor $before $preloaderReceiptPath $ExpectedPreloaderReportSha256 "Native pre-loader report" }

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
if ([int]$report.schema_version -ne 4 -or [string]$report.status -cne "qualified" -or
    @($report.missing_phases).Count -ne 0 -or [string]$report.evidence_sha256 -notmatch '^[0-9a-f]{64}$') {
    throw "Endurance replay output is not a complete qualified schema-3 report."
}
if (($report.phase_owner_history | ConvertTo-Json -Depth 80 -Compress) -cne ($manifest.phase_owner_history | ConvertTo-Json -Depth 80 -Compress)) { throw "Qualification report changed complete phase owner history" }
if ($report.phases -isnot [array] -or $report.phases.Count -ne $manifest.phases.Count) { throw 'Normalized report changed phase inventory' }
foreach ($measuredPhase in $manifest.phases) {
    $normalized=@($report.phases | Where-Object { $_.phase_id -ceq $measuredPhase.phase_id })
    $requirement=@($profileObject.phases | Where-Object { $_.phase_id -ceq $measuredPhase.phase_id })
    if ($normalized.Count -ne 1 -or $requirement.Count -ne 1) { throw 'Normalized report has no unique phase measurement' }
    $expectedTiming=Get-EnduranceMeasurementProjection $measuredPhase.producer $measuredPhase $requirement[0] $machinePlan.timeouts
    if ((Get-EnduranceMeasurementProjection $normalized[0] $measuredPhase $requirement[0] $machinePlan.timeouts) -cne $expectedTiming) { throw 'Normalized report changed owner-derived measurement timing' }
}
if (($report.owner_closure | ConvertTo-Json -Depth 8 -Compress) -cne
    ($manifest.owner_closure | ConvertTo-Json -Depth 8 -Compress)) {
    throw "Endurance qualification report lost or changed run owner closure evidence."
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
