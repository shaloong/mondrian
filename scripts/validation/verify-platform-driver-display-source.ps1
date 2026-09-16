param(
    [Parameter(Mandatory = $true)][ValidateSet("platform_probe", "gpu_color", "viewer_display")][string]$ExpectedKind,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ReportPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RawEvidencePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CellObservationPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineReportPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ArtifactManifestPath,
    [string]$EvidenceClosurePath = "",
    [string]$EvidenceBundleDirectory = "",
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$PolicyPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Read-BoundedFile([string]$Path, [long]$MaximumBytes, [string]$Label, [bool]$AllowEmpty = $false) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        (-not $AllowEmpty -and $item.Length -le 0) -or $item.Length -gt $MaximumBytes -or $item.Length -gt [int]::MaxValue) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -ne $item.Length) { throw "$Label changed during source replay." }
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
            path = $item.FullName
            sha256 = $hash
            length = [long]$bytes.Length
            bytes = $bytes
            text = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff)
        }
    } finally {
        $stream.Dispose()
    }
}

function Read-BoundedJson([string]$Path, [long]$MaximumBytes, [string]$Label) {
    $file = Read-BoundedFile $Path $MaximumBytes $Label
    try { $value = $file.text | ConvertFrom-Json }
    catch { throw "$Label is not valid sealed JSON: $($_.Exception.Message)" }
    return [pscustomobject]@{ value = $value; file = $file }
}

function Resolve-ContainedPath([string]$ManifestPath, [string]$RelativePath, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) {
        throw "$Label path must be a non-empty relative path."
    }
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

function Resolve-RepositoryPath([string]$RelativePath, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) {
        throw "$Label must be a non-empty repository-relative path."
    }
    $resolved = [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $RelativePath))
    $prefix = "$($script:repositoryRoot.TrimEnd([IO.Path]::DirectorySeparatorChar))$([IO.Path]::DirectorySeparatorChar)"
    $comparison = if ([OperatingSystem]::IsWindows()) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    if (-not $resolved.StartsWith($prefix, $comparison)) { throw "$Label escapes the repository." }
    return $resolved
}

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if ($expectedValues.Count -ne @($Expected).Count -or $actualValues.Count -ne @($Actual).Count -or
        @(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label is not an exact unique set."
    }
}

function Assert-EnvironmentMatches([object]$Expected, [object]$Actual, [string]$Label) {
    foreach ($field in @(
        "platform", "architecture", "os_version", "os_build", "window_system", "compositor",
        "graphics_backend", "adapter_name", "adapter_vendor", "adapter_device_id", "adapter_kind",
        "renderer_driver", "renderer_driver_info",
        "display_identity", "native_display_path_id", "display_inventory_sha256"
    )) {
        if ([string]$Expected.$field -ne [string]$Actual.$field) {
            throw "$Label environment field '$field' mismatch."
        }
    }
    if ([string]$Expected.driver.kind -ne [string]$Actual.driver.kind) {
        throw "$Label driver kind mismatch."
    }
    foreach ($field in @("name", "version", "os_build", "kernel_version", "drm_driver", "vulkan_driver", "vulkan_driver_version", "mesa_version")) {
        if ($null -ne $Expected.driver.PSObject.Properties[$field] -and
            [string]$Expected.driver.$field -ne [string]$Actual.driver.$field) {
            throw "$Label driver field '$field' mismatch."
        }
    }
}

function Convert-SurfaceToProduct([string]$Surface) {
    switch ($Surface) {
        "srgb" { return "Srgb" }
        "display_p3" { return "DisplayP3" }
        "bt2100_pq" { return "Bt2100Pq" }
        "extended_linear_edr" { return "ExtendedSrgbLinear" }
        default { throw "Unknown matrix surface color space '$Surface'." }
    }
}

function Get-StringSha256([string]$Value) {
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        return (($sha256.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)) |
            ForEach-Object { $_.ToString("x2") }) -join "")
    } finally { $sha256.Dispose() }
}

function Assert-IccPayload([byte[]]$Bytes, [string]$Label) {
    if ($Bytes.Length -lt 132) { throw "$Label is too short to be a complete ICC profile." }
    $declaredLength = ([uint64]$Bytes[0] * 16777216L) + ([uint64]$Bytes[1] * 65536L) +
        ([uint64]$Bytes[2] * 256L) + [uint64]$Bytes[3]
    if ($declaredLength -ne [uint64]$Bytes.Length -or
        [Text.Encoding]::ASCII.GetString($Bytes, 36, 4) -ne "acsp") {
        throw "$Label has an invalid ICC size declaration or signature."
    }
    $tagCount = ([uint64]$Bytes[128] * 16777216L) + ([uint64]$Bytes[129] * 65536L) +
        ([uint64]$Bytes[130] * 256L) + [uint64]$Bytes[131]
    if ($tagCount -gt 4096 -or 132L + (12L * [long]$tagCount) -gt $Bytes.Length) {
        throw "$Label has an invalid ICC tag table."
    }
    for ($index = 0; $index -lt [int]$tagCount; $index++) {
        $base = 132 + (12 * $index)
        $offset = ([uint64]$Bytes[$base + 4] * 16777216L) + ([uint64]$Bytes[$base + 5] * 65536L) +
            ([uint64]$Bytes[$base + 6] * 256L) + [uint64]$Bytes[$base + 7]
        $length = ([uint64]$Bytes[$base + 8] * 16777216L) + ([uint64]$Bytes[$base + 9] * 65536L) +
            ([uint64]$Bytes[$base + 10] * 256L) + [uint64]$Bytes[$base + 11]
        if ($length -eq 0 -or $offset -gt [uint64]$Bytes.Length -or
            $length -gt ([uint64]$Bytes.Length - $offset)) {
            throw "$Label has an out-of-bounds ICC tag payload."
        }
    }
}

function Get-IccProfileFingerprint([byte[]]$Bytes) {
    $domain = [Text.Encoding]::ASCII.GetBytes("mondrian.icc-profile-fingerprint.v1")
    $domainLength = [BitConverter]::GetBytes([uint64]$domain.Length)
    $payloadLength = [BitConverter]::GetBytes([uint64]$Bytes.Length)
    if (-not [BitConverter]::IsLittleEndian) {
        [Array]::Reverse($domainLength)
        [Array]::Reverse($payloadLength)
    }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $null = $sha256.TransformBlock($domainLength, 0, $domainLength.Length, $null, 0)
        $null = $sha256.TransformBlock($domain, 0, $domain.Length, $null, 0)
        $null = $sha256.TransformBlock($payloadLength, 0, $payloadLength.Length, $null, 0)
        $null = $sha256.TransformFinalBlock($Bytes, 0, $Bytes.Length)
        return (($sha256.Hash | ForEach-Object { $_.ToString("x2") }) -join "")
    } finally {
        $sha256.Dispose()
    }
}

function Assert-EdidPayload([byte[]]$Bytes, [string]$Label) {
    $header = [byte[]](0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00)
    if ($Bytes.Length -lt 128 -or $Bytes.Length % 128 -ne 0) {
        throw "$Label has an invalid EDID header or block length."
    }
    for ($index = 0; $index -lt $header.Length; $index++) {
        if ($Bytes[$index] -ne $header[$index]) { throw "$Label has an invalid EDID header." }
    }
    if ((1 + [int]$Bytes[126]) * 128 -ne $Bytes.Length) {
        throw "$Label EDID extension count does not match its payload length."
    }
    for ($block = 0; $block -lt ($Bytes.Length / 128); $block++) {
        $sum = 0
        for ($index = 0; $index -lt 128; $index++) { $sum += [int]$Bytes[($block * 128) + $index] }
        if (($sum % 256) -ne 0) { throw "$Label EDID block $block has an invalid checksum." }
        if ($block -gt 0 -and $Bytes[$block * 128] -eq 0x02) {
            $offset = [int]$Bytes[($block * 128) + 2]
            if ($offset -ne 0 -and ($offset -lt 4 -or $offset -gt 127)) {
                throw "$Label has an invalid CTA detailed-timing offset."
            }
            $end = if ($offset -eq 0) { 127 } else { $offset }
            $cursor = 4
            while ($cursor -lt $end) {
                $length = [int]($Bytes[($block * 128) + $cursor] -band 0x1f)
                if ($cursor + 1 + $length -gt $end) { throw "$Label has an overrun CTA data block." }
                $cursor += 1 + $length
            }
        }
    }
}

function Invoke-BoundedProcessWithLogs(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$WorkingDirectory,
    [int]$TimeoutSeconds,
    [string]$StdoutPath,
    [string]$StderrPath,
    [string]$Label
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
        if (-not $process.Start()) { throw "Could not start $Label." }
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderr)
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { $null = $process.WaitForExit(1000) }
        if (-not $process.HasExited) {
            try { $process.Kill($true); $process.WaitForExit() } catch { }
            throw "$Label exceeded its $TimeoutSeconds second deadline."
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

function Resolve-ApprovedReplayExecutable([string]$PathVariable, [string]$ShaVariable, [string]$Label) {
    $path = [Environment]::GetEnvironmentVariable($PathVariable, "Process")
    $expectedSha = [Environment]::GetEnvironmentVariable($ShaVariable, "Process")
    if ([string]::IsNullOrWhiteSpace($path) -or $expectedSha -notmatch '^[0-9a-f]{64}$') {
        throw "$Label has no separately approved executable identity."
    }
    $cursor = [IO.Path]::GetFullPath((Split-Path -Parent $path))
    while (-not [string]::IsNullOrWhiteSpace($cursor)) {
        $directory = Get-Item -LiteralPath $cursor -ErrorAction Stop
        if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "$Label traverses a linked directory."
        }
        if ($cursor -eq [IO.Path]::GetPathRoot($cursor)) { break }
        $parent = [IO.Path]::GetFullPath((Split-Path -Parent $cursor))
        if ($parent -eq $cursor) { break }
        $cursor = $parent
    }
    $admission = Read-BoundedFile $path 8589934592 $Label
    if ([string]$admission.sha256 -ne $expectedSha) { throw "$Label differs from its approved SHA-256." }
    return $admission.path
}

function Invoke-DisplayContractReplay([object]$Contract) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ("mondrian-display-contract-replay-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $directory -ErrorAction Stop | Out-Null
    $inputPath = Join-Path $directory "input.json"
    $outputPath = Join-Path $directory "output.json"
    $stdoutPath = Join-Path $directory "stdout.log"
    $stderrPath = Join-Path $directory "stderr.log"
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes(([ordered]@{
            schema_version = 1
            contracts = @($Contract)
        } | ConvertTo-Json -Depth 100 -Compress))
        $stream = [IO.File]::Open($inputPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
        finally { $stream.Dispose() }
        $executable = Resolve-ApprovedReplayExecutable `
            "MONDRIAN_DISPLAY_CONTRACT_REPLAY_EXECUTABLE" `
            "MONDRIAN_DISPLAY_CONTRACT_REPLAY_SHA256" "display contract replay"
        $exitCode = Invoke-BoundedProcessWithLogs $executable @($inputPath, $outputPath) `
            $directory 600 $stdoutPath $stderrPath "display contract replay"
        if ($exitCode -ne 0) {
            $detail = if (Test-Path -LiteralPath $stderrPath) { [IO.File]::ReadAllText($stderrPath) } else { "" }
            throw "Display contract replay failed with exit code ${exitCode}: $detail"
        }
        $result = Read-BoundedJson $outputPath 8388608 "display contract replay output"
        if ($result.value.schema_version -ne 1 -or @($result.value.contracts).Count -ne 1) {
            throw "Display contract replay output has an invalid schema or count."
        }
        return $result.value.contracts[0]
    } finally {
        if (Test-Path -LiteralPath $directory) {
            Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Invoke-DisplayCalibrationReplay(
    [object]$IccProfileFile,
    [string]$SourceColorSpace,
    [string]$RenderingIntent
) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ("mondrian-display-calibration-replay-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $directory -ErrorAction Stop | Out-Null
    $requestPath = Join-Path $directory "request.json"
    $outputPath = Join-Path $directory "output.json"
    $stdoutPath = Join-Path $directory "stdout.log"
    $stderrPath = Join-Path $directory "stderr.log"
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes(([ordered]@{
            schema_version = 1
            source_color_space = $SourceColorSpace
            rendering_intent = $RenderingIntent
        } | ConvertTo-Json -Depth 8 -Compress))
        $stream = [IO.File]::Open($requestPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
        finally { $stream.Dispose() }
        $executable = Resolve-ApprovedReplayExecutable `
            "MONDRIAN_DISPLAY_CALIBRATION_REPLAY_EXECUTABLE" `
            "MONDRIAN_DISPLAY_CALIBRATION_REPLAY_SHA256" "display calibration replay"
        $exitCode = Invoke-BoundedProcessWithLogs $executable @(
            [string]$IccProfileFile.path, $requestPath, $outputPath
        ) $directory 600 $stdoutPath $stderrPath "display calibration replay"
        if ($exitCode -ne 0) {
            $detail = if (Test-Path -LiteralPath $stderrPath) { [IO.File]::ReadAllText($stderrPath) } else { "" }
            throw "Display calibration replay failed with exit code ${exitCode}: $detail"
        }
        $result = Read-BoundedJson $outputPath 1048576 "display calibration replay output"
        if ($result.value.schema_version -ne 1) { throw "Display calibration replay output has an invalid schema." }
        return $result.value
    } finally {
        if (Test-Path -LiteralPath $directory) {
            Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

$trustedRepositoryRoot = [Environment]::GetEnvironmentVariable("MONDRIAN_QUALIFICATION_TRUSTED_REPOSITORY_ROOT", "Process")
$repositoryRoot = if ([string]::IsNullOrWhiteSpace($trustedRepositoryRoot)) {
    [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
} else { [IO.Path]::GetFullPath($trustedRepositoryRoot) }
$maximumJsonBytes = 8388608L
$policyAdmission = Read-BoundedJson $PolicyPath 1048576 "matrix policy"
$profileAdmission = Read-BoundedJson $RuntimeProfilePath 1048576 "runtime profile"
$cellAdmission = Read-BoundedJson $CellObservationPath $maximumJsonBytes "cell observation"
$machineAdmission = Read-BoundedJson $MachineReportPath $maximumJsonBytes "machine report"
$manifestAdmission = Read-BoundedJson $ArtifactManifestPath $maximumJsonBytes "row artifact manifest"
$rawAdmission = Read-BoundedJson $RawEvidencePath $maximumJsonBytes "owner raw evidence"
$reportAdmission = Read-BoundedJson $ReportPath $maximumJsonBytes "owner report"
$policy = $policyAdmission.value
$profile = $profileAdmission.value
$cell = $cellAdmission.value
$machine = $machineAdmission.value
$manifest = $manifestAdmission.value
$raw = $rawAdmission.value
$report = $reportAdmission.value
$maximumSourceBytes = [long]$policy.limits.maximum_source_evidence_file_bytes
$sourceContract = $policy.source_evidence_contracts
$matchingCell = @($profile.cells | Where-Object { [string]$_.cell_id -eq [string]$cell.cell_id })
$matchingArtifact = @($manifest.artifacts | Where-Object { [string]$_.kind -eq $ExpectedKind })
$matchingOwner = @($sourceContract.owners | Where-Object { [string]$_.kind -eq $ExpectedKind })
if ($policy.source_evidence_required -ne $true -or $policy.environment_snapshots_required -ne $true -or
    $sourceContract.schema_version -ne 1 -or $matchingCell.Count -ne 1 -or
    $matchingArtifact.Count -ne 1 -or $matchingOwner.Count -ne 1) {
    throw "Source replay has no unique policy, profile cell, or row artifact contract."
}
$artifact = $matchingArtifact[0]
$source = $artifact.source_evidence
if ($source.schema_version -ne 1 -or
    [string]$source.source_verifier_id -ne [string]$matchingOwner[0].source_verifier_id -or
    [string]::IsNullOrWhiteSpace([string]$source.capture_id)) {
    throw "Source replay identity is absent or mismatched."
}
$bindings = $source.bindings
if ([string]$bindings.cell_id -ne [string]$cell.cell_id -or
    [string]$bindings.cell_run_id -ne [string]$cell.cell_run_id -or
    [string]$bindings.source_revision -ne [string]$cell.source_revision -or
    [string]$bindings.release_candidate_id -ne [string]$cell.release_candidate_id -or
    [string]$bindings.build_manifest_sha256 -ne [string]$cell.build_manifest_sha256 -or
    [string]$bindings.build_provenance_sha256 -ne [string]$cell.product_artifact.build_provenance_sha256 -or
    [string]$bindings.product_artifact_sha256 -ne [string]$cell.product_artifact.sha256 -or
    [string]$bindings.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    [string]$bindings.machine_report_sha256 -ne [string]$cell.machine_report_sha256 -or
    [string]$bindings.environment_sha256 -ne [string]$report.environment_sha256) {
    throw "Source replay bindings do not identify the exact row."
}

$closure = $null
$closureRoot = $null
if (-not [string]::IsNullOrWhiteSpace($EvidenceClosurePath)) {
    $closureAdmission = Read-BoundedJson $EvidenceClosurePath $maximumJsonBytes "evidence closure"
    $closure = $closureAdmission.value
    $closureRoot = if ([string]::IsNullOrWhiteSpace($EvidenceBundleDirectory)) {
        Split-Path -Parent $closureAdmission.file.path
    } else {
        [IO.Path]::GetFullPath($EvidenceBundleDirectory)
    }
}

function Resolve-SourceEntry([object]$Entry, [string]$GlobalRole, [string]$Label) {
    if ([string]$Entry.role -notmatch [string]$script:sourceContract.source_role_pattern -or
        [string]$Entry.format -notin @("json", "jsonl", "text", "binary")) {
        throw "$Label has an invalid role or format."
    }
    if ($null -eq $script:closure) {
        $path = Resolve-ContainedPath $script:manifestAdmission.file.path ([string]$Entry.path) $Label
    } else {
        $matches = @($script:closure.entries | Where-Object { $GlobalRole -in @($_.roles | ForEach-Object { [string]$_ }) })
        if ($matches.Count -ne 1) { throw "$Label has no unique bundled closure role '$GlobalRole'." }
        if ([string]$matches[0].sha256 -ne [string]$Entry.sha256 -or
            [long]$matches[0].byte_length -ne [long]$Entry.byte_length) {
            throw "$Label differs from the admitted evidence closure."
        }
        $path = Resolve-ContainedPath (Join-Path $script:closureRoot "closure-anchor.json") ([string]$matches[0].bundle_path) $Label
    }
    $file = Read-BoundedFile $path $script:maximumSourceBytes $Label ([string]$Entry.format -eq "text")
    if ($file.sha256 -ne [string]$Entry.sha256 -or $file.length -ne [long]$Entry.byte_length) {
        throw "$Label hash or length does not match the sealed row manifest."
    }
    return $file
}

$sourceFiles = @{}
$entries = @($source.entries)
if ($entries.Count -eq 0) { throw "Source replay has no source entries." }
foreach ($entry in $entries) {
    $role = [string]$entry.role
    if ($sourceFiles.ContainsKey($role)) { throw "Source role '$role' is duplicated." }
    $globalRole = "row:$($cell.cell_id):${ExpectedKind}:source:$role"
    $sourceFiles[$role] = Resolve-SourceEntry $entry $globalRole "source role '$role'"
}

$environmentSnapshots = @(
    [pscustomobject]@{ phase = "before"; expected_sequence = 1; entry = $manifest.environment_snapshots.before }
    [pscustomobject]@{ phase = "after"; expected_sequence = 2; entry = $manifest.environment_snapshots.after }
)
foreach ($snapshot in $environmentSnapshots) {
    $role = "row:$($cell.cell_id):environment-$($snapshot.phase)"
    $sourceEntry = [pscustomobject]@{
        role = "environment-$($snapshot.phase)"
        path = [string]$snapshot.entry.path
        sha256 = [string]$snapshot.entry.sha256
        byte_length = [long]$snapshot.entry.byte_length
        format = "json"
    }
    $snapshotFile = Resolve-SourceEntry $sourceEntry $role "environment '$($snapshot.phase)' snapshot"
    try { $snapshotJson = $snapshotFile.text | ConvertFrom-Json }
    catch { throw "Environment '$($snapshot.phase)' snapshot is invalid JSON." }
    if ($snapshotJson.schema_version -ne [int]$sourceContract.environment_snapshot_schema_version -or
        [string]$snapshotJson.capture_phase -ne [string]$snapshot.phase -or
        [int]$snapshotJson.capture_sequence -ne [int]$snapshot.expected_sequence -or
        [string]$snapshotJson.cell_id -ne [string]$cell.cell_id -or
        [string]$snapshotJson.cell_run_id -ne [string]$cell.cell_run_id -or
        [string]$snapshotJson.source_revision -ne [string]$cell.source_revision -or
        [string]::IsNullOrWhiteSpace([string]$snapshotJson.captured_at_utc)) {
        throw "Environment '$($snapshot.phase)' snapshot envelope is invalid."
    }
    Assert-EnvironmentMatches $cell.environment $snapshotJson.environment "environment '$($snapshot.phase)' snapshot"
}
Assert-EnvironmentMatches $cell.environment $machine.environment "source replay machine report"

switch ($ExpectedKind) {
    "platform_probe" {
        foreach ($role in @($sourceContract.platform_probe_static_roles)) {
            if (-not $sourceFiles.ContainsKey([string]$role)) {
                throw "Platform producer source is missing static role '$role'."
            }
        }
        try { $plan = $sourceFiles["capture-plan.json"].text | ConvertFrom-Json }
        catch { throw "Platform capture plan is invalid JSON." }
        try { $challenge = $sourceFiles["authority-challenge.json"].text | ConvertFrom-Json }
        catch { throw "Platform authority challenge is invalid JSON." }
        try { $summary = $sourceFiles["supervisor-summary.json"].text | ConvertFrom-Json }
        catch { throw "Platform producer summary is invalid JSON." }
        $platformProducerVariable = Get-Variable -Scope Global -Name "MondrianQualificationPlatformProducerSha256" -ErrorAction SilentlyContinue
        $approvedProducer = if ($null -ne $platformProducerVariable) {
            [pscustomobject]@{ sha256 = [string]$global:MondrianQualificationPlatformProducerSha256 }
        } else {
            $approvedProducerPath = Resolve-RepositoryPath ([string]$sourceContract.platform_probe_producer_script) "approved platform producer"
            Read-BoundedFile $approvedProducerPath 8388608 "approved platform producer"
        }
        if ($plan.schema_version -ne 1 -or [string]$plan.capture_id -ne [string]$source.capture_id -or
            [string]$plan.cell_id -ne [string]$cell.cell_id -or [string]$plan.cell_run_id -ne [string]$cell.cell_run_id -or
            [string]$plan.source_revision -ne [string]$cell.source_revision -or
            [string]$plan.environment_sha256 -ne [string]$report.environment_sha256 -or
            [string]$plan.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
            [string]$plan.native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
            [uint32]$plan.target.width -eq 0 -or [uint32]$plan.target.height -eq 0 -or
            $challenge.schema_version -ne 1 -or [string]$challenge.capture_kind -ne "platform_probe" -or
            [string]$challenge.capture_id -ne [string]$source.capture_id -or
            [string]$challenge.cell_id -ne [string]$cell.cell_id -or
            [string]$challenge.cell_run_id -ne [string]$cell.cell_run_id -or
            [string]$challenge.source_revision -ne [string]$cell.source_revision -or
            [string]$challenge.release_candidate_id -ne [string]$cell.release_candidate_id -or
            [string]$challenge.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
            ([string]$challenge.nonce).Length -lt 32 -or
            [string]::IsNullOrWhiteSpace([string]$challenge.transition_authority_id) -or
            [string]$bindings.authority_challenge_id -ne [string]$challenge.challenge_id -or
            [string]$bindings.authority_challenge_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
            [string]$bindings.producer_id -ne "mondrian-platform-display-probe-source-v1" -or
            [string]$bindings.producer_sha256 -ne [string]$sourceFiles["producer-binary"].sha256 -or
            [string]$bindings.session_transcript_role -ne "supervisor-summary.json" -or
            $summary.schema_version -ne 1 -or [string]$summary.status -ne "passed" -or
            [string]$summary.source_revision -ne [string]$cell.source_revision -or
            [string]$summary.capture_id -ne [string]$source.capture_id -or
            [string]$summary.authority_challenge_id -ne [string]$challenge.challenge_id -or
            [string]$summary.authority_challenge_manifest_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
            [string]$summary.capture_plan_sha256 -ne [string]$sourceFiles["capture-plan.json"].sha256 -or
            [string]$summary.producer_id -ne [string]$bindings.producer_id -or
            [string]$summary.producer_executable_sha256 -ne [string]$sourceFiles["producer-binary"].sha256 -or
            [string]$summary.supervisor_script_sha256 -ne [string]$approvedProducer.sha256) {
            throw "Platform producer plan/challenge/session is not bound to the exact row."
        }
        $expectedCaptureKeys = [System.Collections.Generic.List[string]]::new()
        foreach ($scenario in @($matchingCell[0].required_scenarios)) {
            if ([string]$scenario.scenario -eq "managed_icc") {
                $expectedCaptureKeys.Add("managed_icc|icc")
                $expectedCaptureKeys.Add("managed_icc|hdr")
            } else {
                $expectedCaptureKeys.Add("$($scenario.scenario)|hdr")
            }
        }
        Assert-ExactStringSet @($expectedCaptureKeys) @($plan.captures | ForEach-Object {
            "$($_.scenario)|$($_.probe_kind)"
        }) "platform producer capture closure"
        Assert-ExactStringSet @($expectedCaptureKeys) @($summary.captures | ForEach-Object {
            "$($_.scenario)|$($_.probe_kind)"
        }) "platform producer transcript closure"
        $expectedRoles = [System.Collections.Generic.List[string]]::new()
        foreach ($role in @($sourceContract.platform_probe_static_roles)) { $expectedRoles.Add([string]$role) }
        $transcripts = @{}
        $validIccBackends = @{}
        $previousTranscriptSha256 = ""
        $previousCaptureFinishedAt = $null
        $captureSequence = 0
        foreach ($capture in @($plan.captures)) {
            $captureSequence += 1
            $scenarioId = [string]$capture.scenario
            $probeKind = [string]$capture.probe_kind
            $scenarioRequirement = @($matchingCell[0].required_scenarios | Where-Object {
                [string]$_.scenario -eq $scenarioId
            })
            if ($scenarioRequirement.Count -ne 1) { throw "Platform capture references an unknown scenario." }
            $expectedState = switch ($scenarioId) {
                "sdr_srgb" { [pscustomobject]@{ hdr_enabled = $false; wide_color_active = $false; active_transfer_function = $null } }
                "managed_icc" { [pscustomobject]@{ hdr_enabled = $false; wide_color_active = $null; active_transfer_function = $null } }
                "display_p3" { [pscustomobject]@{ hdr_enabled = $false; wide_color_active = $true; active_transfer_function = $null } }
                "hdr_pq" {
                    $transfer = if ([string]$scenarioRequirement[0].hdr_presentation -eq "mac_os_edr") { $null } else { "PQ" }
                    [pscustomobject]@{ hdr_enabled = $true; wide_color_active = $true; active_transfer_function = $transfer }
                }
                default { throw "Platform capture has no state-transition contract." }
            }
            if ([string]$capture.required_state.hdr_enabled -ne [string]$expectedState.hdr_enabled -or
                [string]$capture.required_state.wide_color_active -ne [string]$expectedState.wide_color_active -or
                [string]$capture.required_state.active_transfer_function -ne [string]$expectedState.active_transfer_function) {
                throw "Platform capture '$scenarioId/$probeKind' weakens its required display state."
            }
            $key = "$scenarioId|$probeKind"
            $base = "probe.$scenarioId.$probeKind"
            $requestRole = "$base.request.json"
            $transcriptRole = "$base.json"
            $transitionRole = "transition.$captureSequence.ack.json"
            $expectedRoles.Add($requestRole)
            $expectedRoles.Add($transcriptRole)
            $expectedRoles.Add($transitionRole)
            if ($probeKind -eq "icc") { $expectedRoles.Add("$base.icc-profile") }
            if (-not $sourceFiles.ContainsKey($requestRole) -or -not $sourceFiles.ContainsKey($transcriptRole) -or
                -not $sourceFiles.ContainsKey($transitionRole)) {
                throw "Platform producer capture '$key' is missing its request/transcript."
            }
            try { $request = $sourceFiles[$requestRole].text | ConvertFrom-Json }
            catch { throw "Platform producer request '$key' is invalid JSON." }
            try { $transcript = $sourceFiles[$transcriptRole].text | ConvertFrom-Json }
            catch { throw "Platform producer transcript '$key' is invalid JSON." }
            try { $transitionAck = $sourceFiles[$transitionRole].text | ConvertFrom-Json }
            catch { throw "Platform transition acknowledgement '$key' is invalid JSON." }
            $summaryCapture = @($summary.captures | Where-Object {
                [string]$_.scenario -eq $scenarioId -and [string]$_.probe_kind -eq $probeKind
            })
            if ($request.schema_version -ne 1 -or $transcript.schema_version -ne 1 -or
                [string]$request.capture_id -ne [string]$source.capture_id -or
                [string]$request.scenario -ne $scenarioId -or [string]$request.probe_kind -ne $probeKind -or
                [string]$request.expected_backend -ne [string]$capture.expected_backend -or
                [string]$request.cell_id -ne [string]$cell.cell_id -or
                [string]$request.cell_run_id -ne [string]$cell.cell_run_id -or
                [string]$request.source_revision -ne [string]$cell.source_revision -or
                [string]$request.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
                [string]$request.environment_sha256 -ne [string]$report.environment_sha256 -or
                [string]$request.display_identity -ne [string]$cell.environment.display_identity -or
                [string]$request.native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                [string]$request.display_inventory_sha256 -ne [string]$cell.environment.display_inventory_sha256 -or
                ($request.target | ConvertTo-Json -Compress) -ne ($plan.target | ConvertTo-Json -Compress) -or
                [string]$transcript.producer_id -ne [string]$bindings.producer_id -or
                [string]$transcript.producer_executable_sha256 -ne [string]$sourceFiles["producer-binary"].sha256 -or
                [string]$transcript.supervisor_script_sha256 -ne [string]$approvedProducer.sha256 -or
                [string]$transcript.authority_challenge.challenge_id -ne [string]$challenge.challenge_id -or
                [string]$transcript.authority_challenge.manifest_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
                [string]$transcript.authority_challenge.nonce_sha256 -ne (Get-StringSha256 ([string]$challenge.nonce)) -or
                [string]$transcript.capture_id -ne [string]$source.capture_id -or
                [uint32]$transcript.sequence -ne [uint32]$request.sequence -or
                [string]$transcript.scenario -ne $scenarioId -or [string]$transcript.probe_kind -ne $probeKind -or
                [string]$transcript.expected_backend -ne [string]$capture.expected_backend -or
                [string]$transcript.cell_id -ne [string]$cell.cell_id -or
                [string]$transcript.cell_run_id -ne [string]$cell.cell_run_id -or
                [string]$transcript.source_revision -ne [string]$cell.source_revision -or
                [string]$transcript.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
                [string]$transcript.environment_sha256 -ne [string]$report.environment_sha256 -or
                [string]$transcript.display_identity -ne [string]$cell.environment.display_identity -or
                [string]$transcript.native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                [string]$transcript.display_inventory_sha256 -ne [string]$cell.environment.display_inventory_sha256 -or
                [uint64]$transcript.process_id -eq 0 -or [uint64]$transcript.captured_unix_nanos -eq 0 -or
                [string]$transcript.result.kind -ne $probeKind -or
                [string]$transcript.result.backend -ne [string]$capture.expected_backend -or
                [string]$transcript.result.resolved_native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                $transcript.result.discovery_available -ne $true -or
                [string]$capture.transition_id -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$' -or
                $transitionAck.schema_version -ne 1 -or
                [string]$transitionAck.transition_id -ne [string]$capture.transition_id -or
                [string]$transitionAck.capture_id -ne [string]$source.capture_id -or
                [uint32]$transitionAck.sequence -ne [uint32]$request.sequence -or
                [string]$transitionAck.scenario -ne $scenarioId -or
                [string]$transitionAck.probe_kind -ne $probeKind -or
                [string]$transitionAck.cell_id -ne [string]$cell.cell_id -or
                [string]$transitionAck.cell_run_id -ne [string]$cell.cell_run_id -or
                [string]$transitionAck.challenge_id -ne [string]$challenge.challenge_id -or
                [string]$transitionAck.challenge_manifest_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
                [string]$transitionAck.challenge_nonce_sha256 -ne (Get-StringSha256 ([string]$challenge.nonce)) -or
                [string]$transitionAck.authority_id -ne [string]$challenge.transition_authority_id -or
                [string]$transitionAck.previous_transcript_sha256 -ne $previousTranscriptSha256 -or
                [string]$transitionAck.required_state.hdr_enabled -ne [string]$capture.required_state.hdr_enabled -or
                [string]$transitionAck.required_state.wide_color_active -ne [string]$capture.required_state.wide_color_active -or
                [string]$transitionAck.required_state.active_transfer_function -ne [string]$capture.required_state.active_transfer_function -or
                [string]::IsNullOrWhiteSpace([string]$transitionAck.acknowledged_at_utc) -or
                $summaryCapture.Count -ne 1 -or [int]$summaryCapture[0].exit_code -ne 0 -or
                [string]$summaryCapture[0].expected_backend -ne [string]$capture.expected_backend -or
                [uint32]$summaryCapture[0].sequence -ne [uint32]$request.sequence -or
                [string]$summaryCapture[0].transcript_sha256 -ne [string]$sourceFiles[$transcriptRole].sha256 -or
                [uint64]$summaryCapture[0].process_id -ne [uint64]$transcript.process_id -or
                [string]$summaryCapture[0].transition_id -ne [string]$capture.transition_id -or
                [string]$summaryCapture[0].transition_ack_sha256 -ne [string]$sourceFiles[$transitionRole].sha256 -or
                [string]::IsNullOrWhiteSpace([string]$summaryCapture[0].started_at_utc) -or
                [string]::IsNullOrWhiteSpace([string]$summaryCapture[0].finished_at_utc)) {
                throw "Platform producer transcript '$key' is not a replayable native capture."
            }
            $requestedTime = [DateTimeOffset]::MinValue
            $ackTime = [DateTimeOffset]::MinValue
            $startedTime = [DateTimeOffset]::MinValue
            $finishedTime = [DateTimeOffset]::MinValue
            $parseStyle = [Globalization.DateTimeStyles]::RoundtripKind
            $invariant = [Globalization.CultureInfo]::InvariantCulture
            if (-not [DateTimeOffset]::TryParseExact([string]$summaryCapture[0].transition_requested_at_utc, "O", $invariant, $parseStyle, [ref]$requestedTime) -or
                -not [DateTimeOffset]::TryParseExact([string]$transitionAck.acknowledged_at_utc, "O", $invariant, $parseStyle, [ref]$ackTime) -or
                -not [DateTimeOffset]::TryParseExact([string]$summaryCapture[0].started_at_utc, "O", $invariant, $parseStyle, [ref]$startedTime) -or
                -not [DateTimeOffset]::TryParseExact([string]$summaryCapture[0].finished_at_utc, "O", $invariant, $parseStyle, [ref]$finishedTime) -or
                $requestedTime.Offset -ne [TimeSpan]::Zero -or $ackTime.Offset -ne [TimeSpan]::Zero -or
                $startedTime.Offset -ne [TimeSpan]::Zero -or $finishedTime.Offset -ne [TimeSpan]::Zero -or
                $requestedTime -gt $ackTime -or $ackTime -gt $startedTime -or
                $startedTime -gt $finishedTime -or
                $startedTime.Subtract($ackTime).TotalMinutes -gt 5 -or
                ($null -ne $previousCaptureFinishedAt -and
                    $requestedTime -lt $previousCaptureFinishedAt)) {
                throw "Platform transition acknowledgement '$key' is not serial and fresh."
            }
            if ($probeKind -eq "hdr") {
                foreach ($stateField in @("hdr_enabled", "wide_color_active", "active_transfer_function")) {
                    $requiredValue = $capture.required_state.$stateField
                    if ($null -ne $requiredValue -and
                        [string]$transcript.result.details.$stateField -ne [string]$requiredValue) {
                        throw "Platform transition acknowledgement '$key' does not match the native result."
                    }
                }
            }
            if ($probeKind -eq "icc") {
                $payloadRole = "$base.icc-profile"
                if (-not $sourceFiles.ContainsKey($payloadRole) -or
                    [string]$transcript.result.profile_payload.sha256 -ne [string]$sourceFiles[$payloadRole].sha256 -or
                    [long]$transcript.result.profile_payload.byte_length -ne [long]$sourceFiles[$payloadRole].length -or
                    $null -ne $transcript.result.error) {
                    throw "Platform ICC transcript '$key' has no exact successful payload."
                }
                Assert-IccPayload $sourceFiles[$payloadRole].bytes "Platform producer ICC '$key'"
                $validIccBackends[[string]$capture.expected_backend] =
                    Get-IccProfileFingerprint $sourceFiles[$payloadRole].bytes
            }
            $transcripts[$key] = [pscustomobject]@{
                backend = [string]$capture.expected_backend
                role = $transcriptRole
                source_sha256 = [string]$sourceFiles[$transcriptRole].sha256
                value = $transcript
            }
            $previousTranscriptSha256 = [string]$sourceFiles[$transcriptRole].sha256
            $previousCaptureFinishedAt = $finishedTime
        }
        Assert-ExactStringSet @($expectedRoles) @($sourceFiles.Keys) "platform producer source role closure"
        $sequenceValues = @($transcripts.Values | ForEach-Object { [uint32]$_.value.sequence } | Sort-Object)
        for ($index = 0; $index -lt $sequenceValues.Count; $index++) {
            if ($sequenceValues[$index] -ne ($index + 1)) { throw "Platform producer sequence is not contiguous." }
        }
        Assert-ExactStringSet @($matchingCell[0].required_probe_backends) @($raw.probe_results | ForEach-Object {
            [string]$_.backend
        }) "native platform probe result closure"
        foreach ($backend in @($matchingCell[0].required_probe_backends)) {
            $rawProbe = @($raw.probe_results | Where-Object { [string]$_.backend -eq [string]$backend })
            $sourceHashes = @($transcripts.Values | Where-Object { [string]$_.backend -eq [string]$backend } |
                ForEach-Object { [string]$_.source_sha256 })
            if ($rawProbe.Count -ne 1 -or [string]$rawProbe[0].status -ne "qualified" -or
                [string]$rawProbe[0].display_identity -ne [string]$cell.environment.display_identity -or
                [string]$rawProbe[0].native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                [string]$rawProbe[0].environment_sha256 -ne [string]$report.environment_sha256) {
                throw "Native platform backend '$backend' did not derive the normalized probe result."
            }
            Assert-ExactStringSet $sourceHashes @($rawProbe[0].source_sha256s) "native backend '$backend' transcript hashes"
        }
        $platform = [string]$cell.environment.platform
        $rules = $policy.native_probe_rules.$platform
        switch ($platform) {
            "windows" { $iccBackends = @($rules.managed_icc); $wideBackends = @($rules.wide_color_hdr); $hdrBackends = $wideBackends }
            "mac_os" { $iccBackends = @($rules.managed_icc); $wideBackends = @($rules.wide_color_hdr_edr); $hdrBackends = $wideBackends }
            "linux" {
                $iccBackends = @($rules.managed_icc); $wideBackends = @($rules.wide_color_hdr); $hdrBackends = $wideBackends
                if ($rules.drm_edid_is_capability_only -ne $true) { throw "Linux DRM/EDID must remain capability-only." }
            }
            default { throw "Unsupported platform '$platform'." }
        }
        Assert-ExactStringSet @($matchingCell[0].required_scenarios | ForEach-Object { [string]$_.scenario }) @(
            $raw.scenarios | ForEach-Object { [string]$_.scenario
        }) "platform native scenario closure"
        foreach ($scenario in @($matchingCell[0].required_scenarios)) {
            $scenarioId = [string]$scenario.scenario
            $hdr = $transcripts["$scenarioId|hdr"]
            $details = $hdr.value.result.details
            if ($null -ne $hdr.value.result.error) { throw "Native HDR capture '$scenarioId' reported an API failure." }
            $qualified = switch ($scenarioId) {
                "sdr_srgb" {
                    [string]$hdr.backend -in $wideBackends -and $details.hdr_enabled -eq $false -and
                        $details.wide_color_active -eq $false
                }
                "managed_icc" {
                    $icc = $transcripts["managed_icc|icc"]
                    [string]$icc.backend -in $iccBackends -and $validIccBackends.ContainsKey([string]$icc.backend) -and
                        $details.hdr_enabled -eq $false
                }
                "display_p3" {
                    [string]$hdr.backend -in $wideBackends -and $details.wide_color_supported -eq $true -and
                        $details.wide_color_active -eq $true
                }
                "hdr_pq" {
                    if ([string]$hdr.backend -notin $hdrBackends -or $details.hdr_supported -ne $true -or
                        $details.hdr_enabled -ne $true -or $details.force_disabled -eq $true) { $false }
                    elseif ([string]$scenario.hdr_presentation -eq "mac_os_edr") {
                        $null -ne $details.current_headroom_ppm -and
                            [long]$details.current_headroom_ppm -ge [long]$scenario.minimum_edr_headroom_ppm
                    } else {
                        ([string]$details.active_transfer_function).ToLowerInvariant() -eq "pq" -and
                            ($null -eq $scenario.minimum_bits_per_color_channel -or
                                [long]$details.bits_per_color_channel -ge [long]$scenario.minimum_bits_per_color_channel) -and
                            ($null -eq $scenario.minimum_peak_luminance_nits -or
                                [long]$details.max_luminance_nits -ge [long]$scenario.minimum_peak_luminance_nits)
                    }
                }
                default { $false }
            }
            if (-not $qualified) { throw "Native producer did not qualify scenario '$scenarioId' from OS API returns." }
            $sourceTranscript = if ($scenarioId -eq "managed_icc") { $transcripts["managed_icc|icc"] } else { $hdr }
            $expectedIcc = if ($scenarioId -eq "managed_icc") {
                [string]$validIccBackends[[string]$sourceTranscript.backend]
            } else { "" }
            $rawScenario = @($raw.scenarios | Where-Object { [string]$_.scenario -eq $scenarioId })
            if ($rawScenario.Count -ne 1 -or [string]$rawScenario[0].status -ne "qualified" -or
                [string]$rawScenario[0].source_backend -ne [string]$sourceTranscript.backend -or
                [string]$rawScenario[0].source_sha256 -ne [string]$sourceTranscript.source_sha256 -or
                [string]$rawScenario[0].surface_color_space -ne [string]$scenario.surface_color_space -or
                [string]$rawScenario[0].transfer -ne [string]$scenario.transfer -or
                [string]$rawScenario[0].bits_per_color_channel -ne [string]$details.bits_per_color_channel -or
                [string]$rawScenario[0].wide_color_supported -ne [string]$details.wide_color_supported -or
                [string]$rawScenario[0].wide_color_active -ne [string]$details.wide_color_active -or
                [string]$rawScenario[0].hdr_supported -ne [string]$details.hdr_supported -or
                [string]$rawScenario[0].hdr_enabled -ne [string]$details.hdr_enabled -or
                [string]$rawScenario[0].active_hdr_transfer -ne [string]$details.active_transfer_function -or
                [string]$rawScenario[0].peak_luminance_nits -ne [string]$details.max_luminance_nits -or
                [string]$rawScenario[0].edr_headroom_ppm -ne [string]$details.current_headroom_ppm -or
                [string]$rawScenario[0].hdr_presentation -ne [string]$scenario.hdr_presentation -or
                [string]$rawScenario[0].icc_profile_sha256 -ne $expectedIcc) {
                throw "Native platform scenario '$scenarioId' does not derive its normalized facts."
            }
        }
    }
    "gpu_color" {
        $expectedRoles = [System.Collections.Generic.List[string]]::new()
        foreach ($role in @($sourceContract.gpu_color_static_roles)) { $expectedRoles.Add([string]$role) }
        foreach ($gate in @($policy.owner_verifier_contracts.gpu_color_required_gates)) {
            foreach ($suffix in @($sourceContract.gpu_color_gate_roles)) {
                $expectedRoles.Add("gate.$gate.$suffix")
            }
        }
        Assert-ExactStringSet @($expectedRoles) @($sourceFiles.Keys) "GPU source role closure"
        try { $gpuProfile = $sourceFiles["profile.json"].text | ConvertFrom-Json }
        catch { throw "GPU source profile is invalid JSON." }
        try { $challenge = $sourceFiles["authority-challenge.json"].text | ConvertFrom-Json }
        catch { throw "GPU authority challenge is invalid JSON." }
        $gpuProfileVariable = Get-Variable -Scope Global -Name "MondrianQualificationGpuProfileAdmission" -ErrorAction SilentlyContinue
        $approvedGpuProfileFile = if ($null -ne $gpuProfileVariable) {
            $global:MondrianQualificationGpuProfileAdmission
        } else {
            $approvedGpuProfilePath = Resolve-RepositoryPath ([string]$sourceContract.gpu_color_profile_path) "approved GPU profile"
            Read-BoundedFile $approvedGpuProfilePath 1048576 "approved GPU profile"
        }
        $gpuProducerVariable = Get-Variable -Scope Global -Name "MondrianQualificationGpuProducerSha256" -ErrorAction SilentlyContinue
        $approvedGpuProducerFile = if ($null -ne $gpuProducerVariable) {
            [pscustomobject]@{ sha256 = [string]$global:MondrianQualificationGpuProducerSha256 }
        } else {
            $approvedGpuProducerPath = Resolve-RepositoryPath ([string]$sourceContract.gpu_color_producer_script) "approved GPU producer"
            Read-BoundedFile $approvedGpuProducerPath 8388608 "approved GPU producer"
        }
        try { $approvedGpuProfile = $approvedGpuProfileFile.text | ConvertFrom-Json }
        catch { throw "Approved GPU profile is invalid JSON." }
        try { $summary = $sourceFiles["supervisor-summary.json"].text | ConvertFrom-Json }
        catch { throw "GPU supervisor summary is invalid JSON." }
        if ($gpuProfile.schema_version -ne 1 -or
            [string]$sourceFiles["profile.json"].sha256 -ne [string]$approvedGpuProfileFile.sha256 -or
            $approvedGpuProfile.schema_version -ne 1 -or $approvedGpuProfile.single_test_result_required -ne $true -or
            $approvedGpuProfile.diagnostic_skip_forbidden -ne $true -or $summary.schema_version -ne 2 -or
            [string]$summary.status -ne "passed" -or
            [string]$summary.source_sha -ne [string]$cell.source_revision -or
            [string]$summary.machine.report_sha256 -ne [string]$cell.machine_report_sha256 -or
            [string]$summary.profile.sha256 -ne [string]$sourceFiles["profile.json"].sha256 -or
            $challenge.schema_version -ne 1 -or [string]$challenge.capture_kind -ne "gpu_color" -or
            [string]$challenge.capture_id -ne [string]$source.capture_id -or
            [string]$challenge.cell_id -ne [string]$cell.cell_id -or
            [string]$challenge.cell_run_id -ne [string]$cell.cell_run_id -or
            [string]$challenge.source_revision -ne [string]$cell.source_revision -or
            [string]$challenge.release_candidate_id -ne [string]$cell.release_candidate_id -or
            [string]$challenge.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
            ([string]$challenge.nonce).Length -lt 32 -or
            [string]$bindings.authority_challenge_id -ne [string]$challenge.challenge_id -or
            [string]$bindings.authority_challenge_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
            [string]$bindings.producer_id -ne "mondrian-platform-gpu-color-source-v1" -or
            [string]$bindings.producer_sha256 -ne [string]$approvedGpuProducerFile.sha256 -or
            [string]$bindings.session_transcript_role -ne "supervisor-summary.json" -or
            [string]$summary.authority_challenge.challenge_id -ne [string]$challenge.challenge_id -or
            [string]$summary.authority_challenge.manifest_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
            [string]$summary.authority_challenge.nonce_sha256 -ne (Get-StringSha256 ([string]$challenge.nonce)) -or
            [string]$summary.producer.id -ne [string]$bindings.producer_id -or
            [string]$summary.producer.script_sha256 -ne [string]$approvedGpuProducerFile.sha256 -or
            [string]$summary.build.stdout_sha256 -ne [string]$sourceFiles["build.stdout"].sha256 -or
            [string]$summary.build.stderr_sha256 -ne [string]$sourceFiles["build.stderr"].sha256 -or
            [int]$summary.build.exit_code -ne 0 -or $summary.build.timed_out -ne $false -or
            [string]$summary.adapter.name -ne [string]$cell.environment.adapter_name -or
            [string]$summary.adapter.vendor_id -ne [string]$cell.environment.adapter_vendor -or
            [string]$summary.adapter.device_id -ne [string]$cell.environment.adapter_device_id -or
            [string]$summary.adapter.driver -ne [string]$cell.environment.renderer_driver -or
            [string]$summary.adapter.driver_info -ne [string]$cell.environment.renderer_driver_info -or
            ([string]$summary.adapter.device_type).ToLowerInvariant() -in @("cpu", "other") -or
            ([string]$summary.adapter.backend).ToLowerInvariant() -ne ([string]$cell.environment.graphics_backend).ToLowerInvariant()) {
            throw "GPU supervisor source is not bound to the exact row adapter/build."
        }
        Assert-ExactStringSet @($policy.owner_verifier_contracts.gpu_color_required_gates) `
            @($summary.gates | ForEach-Object { [string]$_.id }) "GPU supervisor gate closure"
        Assert-ExactStringSet @($policy.owner_verifier_contracts.gpu_color_required_gates) `
            @($approvedGpuProfile.gates | ForEach-Object { [string]$_.id }) "approved GPU profile gate closure"
        foreach ($gate in @($summary.gates)) {
            $id = [string]$gate.id
            $rawMatches = @($raw.gates | Where-Object { [string]$_.id -eq $id })
            $approvedGates = @($approvedGpuProfile.gates | Where-Object { [string]$_.id -eq $id })
            if ($approvedGates.Count -ne 1) { throw "GPU gate '$id' has no unique approved command." }
            $approvedGate = $approvedGates[0]
            $expectedArgv = [System.Collections.Generic.List[string]]::new()
            foreach ($argument in @("test", "--locked", "-p", [string]$approvedGpuProfile.package, "--all-features", "-j", "1")) {
                $expectedArgv.Add($argument)
            }
            if ([string]$approvedGate.target -eq "lib") {
                $expectedArgv.Add("--lib")
            } else {
                $expectedArgv.Add("--test"); $expectedArgv.Add([string]$approvedGate.integration_test)
            }
            $expectedArgv.Add([string]$approvedGate.test)
            $expectedArgv.Add("--"); $expectedArgv.Add("--exact"); $expectedArgv.Add("--nocapture")
            if ($approvedGate.ignored -eq $true) { $expectedArgv.Add("--ignored") }
            try { $gateMeasurement = $sourceFiles["gate.$id.measurement.json"].text | ConvertFrom-Json }
            catch { throw "GPU gate '$id' measurement source is invalid JSON." }
            try { $gateReport = $sourceFiles["gate.$id.report.json"].text | ConvertFrom-Json }
            catch { throw "GPU gate '$id' source report is invalid JSON." }
            $combinedOutput = $sourceFiles["gate.$id.stdout"].text + "`n" + $sourceFiles["gate.$id.stderr"].text
            if ($gate.passed -ne $true -or [int]$gate.exit_code -ne 0 -or $gate.timed_out -ne $false -or
                [string]$gate.test -ne [string]$approvedGate.test -or
                [string]$gate.target -ne [string]$approvedGate.target -or
                [string]$gate.integration_test -ne [string]$approvedGate.integration_test -or
                [bool]$gate.ignored -ne [bool]$approvedGate.ignored -or
                [string]$gate.stdout_sha256 -ne [string]$sourceFiles["gate.$id.stdout"].sha256 -or
                [string]$gate.stderr_sha256 -ne [string]$sourceFiles["gate.$id.stderr"].sha256 -or
                [string]$gate.report_sha256 -ne [string]$sourceFiles["gate.$id.report.json"].sha256 -or
                (@($gate.exact_argv) -join [char]0) -ne ($expectedArgv.ToArray() -join [char]0) -or
                [string]::IsNullOrWhiteSpace([string]$gate.started_at_utc) -or
                [string]::IsNullOrWhiteSpace([string]$gate.finished_at_utc) -or
                [string]$gate.test_executable_sha256 -notmatch '^[0-9a-f]{64}$' -or
                [uint64]$gate.test_process_id -eq 0 -or
                $combinedOutput -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;' -or
                $combinedOutput -match '(?i)skipping diagnostic GPU color gate|skipping diagnostic native YUV color gate' -or
                $gateReport.schema_version -ne 3 -or [string]$gateReport.gate_id -ne $id -or
                [string]$gateReport.status -ne "qualified" -or $gateReport.skipped -ne $false -or
                [string]$gateReport.source_revision -ne [string]$cell.source_revision -or
                [string]$gateReport.cell_run_id -ne [string]$cell.cell_run_id -or
                [string]$gateReport.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
                [string]$gateReport.adapter_name -ne [string]$cell.environment.adapter_name -or
                [string]$gateReport.adapter_device_id -ne [string]$cell.environment.adapter_device_id -or
                [string]$gateReport.cargo_test -ne [string]$approvedGate.test -or
                (@($gateReport.exact_argv) -join [char]0) -ne ($expectedArgv.ToArray() -join [char]0) -or
                [string]$gateReport.started_at_utc -ne [string]$gate.started_at_utc -or
                [string]$gateReport.finished_at_utc -ne [string]$gate.finished_at_utc -or
                [string]$gateReport.authority_challenge_manifest_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
                [string]$gateReport.producer_script_sha256 -ne [string]$approvedGpuProducerFile.sha256 -or
                [string]$gateReport.test_executable_sha256 -ne [string]$gate.test_executable_sha256 -or
                [uint64]$gateReport.test_process_id -ne [uint64]$gate.test_process_id -or
                [string]$gateReport.stdout_sha256 -ne [string]$sourceFiles["gate.$id.stdout"].sha256 -or
                [string]$gateReport.stderr_sha256 -ne [string]$sourceFiles["gate.$id.stderr"].sha256 -or
                [string]$gateReport.measurement_sha256 -ne [string]$sourceFiles["gate.$id.measurement.json"].sha256 -or
                $gateMeasurement.schema_version -ne 1 -or [string]$gateMeasurement.gate_id -ne $id -or
                [string]$gateMeasurement.adapter.name -ne [string]$cell.environment.adapter_name -or
                [string]$gateMeasurement.adapter.vendor_id -ne [string]$cell.environment.adapter_vendor -or
                [string]$gateMeasurement.adapter.device_id -ne [string]$cell.environment.adapter_device_id -or
                [string]$gateMeasurement.adapter.driver -ne [string]$cell.environment.renderer_driver -or
                [string]$gateMeasurement.adapter.driver_info -ne [string]$cell.environment.renderer_driver_info -or
                [string]$gateMeasurement.attestation.challenge_id -ne [string]$challenge.challenge_id -or
                [string]$gateMeasurement.attestation.challenge_manifest_sha256 -ne [string]$sourceFiles["authority-challenge.json"].sha256 -or
                [string]$gateMeasurement.attestation.challenge_nonce_sha256 -ne (Get-StringSha256 ([string]$challenge.nonce)) -or
                [string]$gateMeasurement.attestation.cell_run_id -ne [string]$cell.cell_run_id -or
                [string]$gateMeasurement.attestation.source_revision -ne [string]$cell.source_revision -or
                [string]$gateMeasurement.attestation.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
                [string]$gateMeasurement.attestation.producer_script_sha256 -ne [string]$approvedGpuProducerFile.sha256 -or
                [string]$gateMeasurement.attestation.test_executable_sha256 -ne [string]$gate.test_executable_sha256 -or
                [uint64]$gateMeasurement.attestation.process_id -ne [uint64]$gate.test_process_id -or
                ([string]$gateMeasurement.adapter.backend).ToLowerInvariant() -ne
                    ([string]$cell.environment.graphics_backend).ToLowerInvariant() -or
                $rawMatches.Count -ne 1 -or [string]$rawMatches[0].status -ne "qualified" -or
                [string]$rawMatches[0].report_sha256 -ne [string]$gate.report_sha256 -or
                [string]$rawMatches[0].adapter_vendor -ne [string]$cell.environment.adapter_vendor -or
                [string]$rawMatches[0].renderer_driver -ne [string]$cell.environment.renderer_driver -or
                [string]$rawMatches[0].renderer_driver_info -ne [string]$cell.environment.renderer_driver_info) {
                throw "GPU gate '$id' source evidence does not derive the normalized result."
            }
            Assert-ExactStringSet @($approvedGate.measurements | ForEach-Object { [string]$_.metric }) `
                @($gateReport.measurements | ForEach-Object { [string]$_.metric }) `
                "GPU gate '$id' measurement closure"
            Assert-ExactStringSet @($approvedGate.measurements | ForEach-Object { [string]$_.metric }) `
                @($gateMeasurement.measurements | ForEach-Object { [string]$_.metric }) `
                "GPU gate '$id' test-emitted measurement closure"
            foreach ($measurement in @($approvedGate.measurements)) {
                $observed = @($gateReport.measurements | Where-Object { [string]$_.metric -eq [string]$measurement.metric })
                $emitted = @($gateMeasurement.measurements | Where-Object { [string]$_.metric -eq [string]$measurement.metric })
                if ($observed.Count -ne 1 -or -not [double]::IsFinite([double]$observed[0].value) -or
                    $emitted.Count -ne 1 -or -not [double]::IsFinite([double]$emitted[0].value) -or
                    [double]$observed[0].value -ne [double]$emitted[0].value -or
                    [string]$observed[0].comparison -ne [string]$measurement.comparison -or
                    [double]$observed[0].limit -ne [double]$measurement.limit) {
                    throw "GPU gate '$id' has an invalid '$($measurement.metric)' measurement."
                }
                $within = switch ([string]$measurement.comparison) {
                    "at_most" { [double]$observed[0].value -le [double]$measurement.limit }
                    "at_least" { [double]$observed[0].value -ge [double]$measurement.limit }
                    default { $false }
                }
                if (-not $within) { throw "GPU gate '$id' measurement '$($measurement.metric)' exceeds its approved limit." }
            }
        }
    }
    "viewer_display" {
        $expectedRoles = [System.Collections.Generic.List[string]]::new()
        foreach ($role in @($sourceContract.viewer_static_roles)) { $expectedRoles.Add([string]$role) }
        foreach ($scenario in @($matchingCell[0].required_scenarios)) {
            foreach ($suffix in @($sourceContract.viewer_scenario_roles)) {
                $expectedRoles.Add("scenario.$($scenario.scenario).$suffix")
            }
        }
        $hasManagedIccScenario = @($matchingCell[0].required_scenarios | Where-Object {
            [string]$_.scenario -eq "managed_icc"
        }).Count -eq 1
        if ($hasManagedIccScenario) { $expectedRoles.Add("scenario.managed_icc.icc-profile") }
        Assert-ExactStringSet @($expectedRoles) @($sourceFiles.Keys) "Viewer source role closure"
        $viewerIccProfile = $null
        if ($hasManagedIccScenario) {
            $viewerIccProfile = $sourceFiles["scenario.managed_icc.icc-profile"]
            Assert-IccPayload $viewerIccProfile.bytes "Viewer managed ICC profile"
        }
        try { $operator = $sourceFiles["operator-observation.json"].text | ConvertFrom-Json }
        catch { throw "Viewer operator observation is invalid JSON." }
        if ($operator.schema_version -ne 1 -or
            [string]$operator.qualification_run_id -ne [string]$source.capture_id -or
            [string]::IsNullOrWhiteSpace([string]$operator.operator_id) -or
            [string]::IsNullOrWhiteSpace([string]$operator.observed_at_utc)) {
            throw "Viewer operator observation is not bound to the capture."
        }
        $qualifiedProcess = $null
        $qualifiedAdapter = $null
        $qualifiedDisplay = $null
        foreach ($scenario in @($matchingCell[0].required_scenarios)) {
            $scenarioId = [string]$scenario.scenario
            $jsonlRole = "scenario.$scenarioId.product.jsonl"
            $stimulusRole = "scenario.$scenarioId.stimulus.json"
            try { $stimulus = $sourceFiles[$stimulusRole].text | ConvertFrom-Json }
            catch { throw "Viewer stimulus '$scenarioId' is invalid JSON." }
            if ($stimulus.schema_version -ne 1 -or [string]$stimulus.scenario -ne $scenarioId -or
                [string]$stimulus.qualification_run_id -ne [string]$source.capture_id -or
                [string]$stimulus.source_revision -ne [string]$cell.source_revision -or
                [string]$stimulus.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
                [string]$stimulus.surface_color_space -ne [string]$scenario.surface_color_space -or
                [string]$stimulus.transfer -ne [string]$scenario.transfer -or
                [string]$stimulus.native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                [string]$stimulus.adapter_name -ne [string]$cell.environment.adapter_name -or
                [string]$stimulus.adapter_vendor -ne [string]$cell.environment.adapter_vendor -or
                [string]$stimulus.adapter_device_id -ne [string]$cell.environment.adapter_device_id -or
                [string]$stimulus.renderer_driver -ne [string]$cell.environment.renderer_driver -or
                [string]$stimulus.renderer_driver_info -ne [string]$cell.environment.renderer_driver_info) {
                throw "Viewer stimulus '$scenarioId' is not bound to this capture."
            }
            $records = @(
                $sourceFiles[$jsonlRole].text -split "`r?`n" |
                    Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
                    ForEach-Object { $_ | ConvertFrom-Json }
            )
            if ($records.Count -eq 0) { throw "Viewer scenario '$scenarioId' has no product records." }
            $processes = @($records | ForEach-Object { "$($_.process_instance_id)|$($_.process_id)" } | Sort-Object -Unique)
            $adapters = @($records | ForEach-Object { "$($_.renderer_adapter.name)|$($_.renderer_adapter.vendor_id)|$($_.renderer_adapter.device_id)|$($_.renderer_adapter.device_type)|$($_.renderer_adapter.driver)|$($_.renderer_adapter.backend)" } | Sort-Object -Unique)
            $displays = @($records | ForEach-Object { "$($_.display_target.name)|$($_.display_target.position[0])|$($_.display_target.position[1])|$($_.display_target.physical_size[0])|$($_.display_target.physical_size[1])|$($_.display_target.native_display_id)|$($_.display_target.native_display_path_id)" } | Sort-Object -Unique)
            $sequences = @($records | ForEach-Object { [uint64]$_.qualification_record_sequence })
            if ($processes.Count -ne 1 -or $adapters.Count -ne 1 -or $displays.Count -ne 1 -or
                @($records | Where-Object {
                    [string]$_.qualification_run_id -ne [string]$source.capture_id -or
                    [string]$_.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
                    [string]$_.display_target.native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                    [string]$_.renderer_adapter.name -ne [string]$cell.environment.adapter_name -or
                    [string]$_.renderer_adapter.vendor_id -ne [string]$cell.environment.adapter_vendor -or
                    [string]$_.renderer_adapter.device_id -ne [string]$cell.environment.adapter_device_id -or
                    [string]$_.renderer_adapter.driver -ne [string]$cell.environment.renderer_driver -or
                    [string]$_.renderer_adapter.driver_info -ne [string]$cell.environment.renderer_driver_info -or
                    ([string]$_.renderer_adapter.device_type).ToLowerInvariant() -in @("cpu", "other") -or
                    ([string]$_.renderer_adapter.backend).ToLowerInvariant() -ne
                        ([string]$cell.environment.graphics_backend).ToLowerInvariant()
                }).Count -ne 0 -or @($sequences | Sort-Object -Unique).Count -ne $sequences.Count) {
                throw "Viewer scenario '$scenarioId' splices runs, processes, adapters, displays, or sequences."
            }
            for ($index = 1; $index -lt $sequences.Count; $index++) {
                if ($sequences[$index] -le $sequences[$index - 1]) {
                    throw "Viewer scenario '$scenarioId' has non-monotonic product records."
                }
            }
            if ($null -eq $qualifiedProcess) {
                $qualifiedProcess = $processes[0]; $qualifiedAdapter = $adapters[0]; $qualifiedDisplay = $displays[0]
            } elseif ($qualifiedProcess -ne $processes[0] -or $qualifiedAdapter -ne $adapters[0] -or $qualifiedDisplay -ne $displays[0]) {
                throw "Viewer source scenarios were captured from different process/adapter/display chains."
            }
            $surface = Convert-SurfaceToProduct ([string]$scenario.surface_color_space)
            $ready = @($records | Where-Object {
                [string]$_.health.status -eq "Ready" -and
                [string]$_.display_snapshot.validation_status -eq "Pass" -and
                [string]$_.display_snapshot.surface_color_space -eq $surface -and
                [int]$_.display_snapshot.blocker_count -eq 0 -and
                $_.health.external_texture_registered -eq $true -and
                [int]$_.presented_external_texture_batches -gt 0 -and
                [int]$_.stage_readback_stages -eq 0 -and
                $null -ne $_.display_output_contract -and
                [string]$_.display_output_contract.surface_color_space -eq [string]$_.display_snapshot.surface_color_space -and
                [string]$_.display_output_contract.surface_format -eq [string]$_.display_snapshot.surface_format -and
                [string]$_.display_output_contract.surface_hdr_mode -eq [string]$_.display_snapshot.surface_hdr_mode
            })
            if ($ready.Count -eq 0) { throw "Viewer scenario '$scenarioId' has no replayable Ready product record." }
            $final = $ready[-1]
            $replayed = Invoke-DisplayContractReplay $final.display_output_contract
            $expectedPlatform = switch ([string]$cell.environment.platform) {
                "windows" { "Windows" }
                "mac_os" { "Macos" }
                "linux" { "Linux" }
                default { throw "Viewer source has an unsupported display platform." }
            }
            if ([string]$replayed.contract_sha256 -ne [string]$final.display_snapshot.contract_sha256 -or
                [string]$replayed.platform -ne $expectedPlatform -or
                [string]$replayed.display_id.name -ne [string]$final.display_target.name -or
                [int]$replayed.display_id.position[0] -ne [int]$final.display_target.position[0] -or
                [int]$replayed.display_id.position[1] -ne [int]$final.display_target.position[1] -or
                [int]$replayed.display_id.physical_size[0] -ne [int]$final.display_target.physical_size[0] -or
                [int]$replayed.display_id.physical_size[1] -ne [int]$final.display_target.physical_size[1] -or
                [long]$replayed.scale_factor_ppm -ne [long]$final.display_target.scale_factor_ppm -or
                [string]$replayed.surface_format -ne [string]$final.display_snapshot.surface_format -or
                [string]$replayed.surface_color_space -ne [string]$final.display_snapshot.surface_color_space -or
                [string]$replayed.surface_hdr_mode -ne [string]$final.display_snapshot.surface_hdr_mode -or
                [string]$replayed.requested_viewer_mode -ne [string]$final.display_snapshot.requested_viewer_mode -or
                [string]$replayed.resolved_output_color_space -ne [string]$final.display_snapshot.resolved_output_color_space -or
                $replayed.validation_passed -ne $true -or [int]$replayed.blocker_count -ne 0 -or
                [int]$replayed.blocker_count -ne [int]$final.display_snapshot.blocker_count -or
                [int]$replayed.warning_count -ne [int]$final.display_snapshot.warning_count) {
                throw "Viewer '$scenarioId' full Display Output Contract does not derive its diagnostic snapshot."
            }
            $carrierObserved = @($records | Where-Object {
                $_.ui_surface_carrier_active -eq $true -and
                $_.ui_surface_carrier_target_rebuilt -eq $false -and
                [int]$_.presented_external_texture_batches -gt 0
            }).Count -gt 0
            $carrierRequired = $scenarioId -in @("display_p3", "hdr_pq")
            $transferObserved = switch ([string]$scenario.transfer) {
                "surface_code_values" { [int]$final.presented_surface_code_value_batches -gt 0 }
                "extended_linear_values" { [int]$final.presented_surface_code_value_batches -gt 0 }
                "device_code_values" { [int]$final.presented_device_code_value_batches -gt 0 }
                default { $false }
            }
            if (($carrierRequired -and -not $carrierObserved) -or -not $transferObserved -or
                ($scenarioId -eq "hdr_pq" -and $replayed.hdr_requested_supported -ne $true) -or
                ($scenarioId -eq "managed_icc" -and
                    ($replayed.monitor_profile_managed -ne $true -or
                     [string]::IsNullOrWhiteSpace([string]$replayed.monitor_profile_fingerprint_sha256) -or
                     [string]::IsNullOrWhiteSpace([string]$final.display_calibration_identity_sha256)))) {
                throw "Viewer '$scenarioId' did not derive its carrier, transfer, HDR, or ICC execution facts."
            }
            $calibrationReplay = $null
            if ($scenarioId -eq "managed_icc") {
                if ([string]::IsNullOrWhiteSpace([string]$final.display_calibration_rendering_intent)) {
                    throw "Viewer managed ICC record does not identify its rendering intent."
                }
                $calibrationReplay = Invoke-DisplayCalibrationReplay $viewerIccProfile `
                    ([string]$replayed.resolved_output_color_space) `
                    ([string]$final.display_calibration_rendering_intent)
                if ([string]$calibrationReplay.profile_fingerprint_sha256 -ne
                        [string]$replayed.monitor_profile_fingerprint_sha256 -or
                    [string]$calibrationReplay.calibration_identity_sha256 -ne
                        [string]$final.display_calibration_identity_sha256) {
                    throw "Viewer managed ICC profile or sampled processor identity is not reproducible."
                }
            }
            $rawMatches = @($raw.scenarios | Where-Object { [string]$_.scenario -eq $scenarioId })
            $operatorMatches = @($operator.observations | Where-Object { [string]$_.scenario_id -eq $scenarioId })
            if ($rawMatches.Count -ne 1 -or $operatorMatches.Count -ne 1 -or $operatorMatches[0].passed -ne $true -or
                [string]$operatorMatches[0].process_instance_id -ne [string]$final.process_instance_id -or
                [string]$operatorMatches[0].output_contract_sha256 -ne [string]$replayed.contract_sha256 -or
                [string]$operatorMatches[0].native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
                [string]$rawMatches[0].output_contract_sha256 -ne [string]$replayed.contract_sha256 -or
                [string]$rawMatches[0].operator_attestation_sha256 -ne [string]$sourceFiles["operator-observation.json"].sha256 -or
                [bool]$rawMatches[0].carrier_reuse_observed -ne [bool]$carrierObserved -or
                [string]$rawMatches[0].surface_color_space -ne [string]$scenario.surface_color_space -or
                [string]$rawMatches[0].transfer -ne [string]$scenario.transfer -or
                [string]$rawMatches[0].adapter_name -ne [string]$cell.environment.adapter_name -or
                [string]$rawMatches[0].adapter_vendor -ne [string]$cell.environment.adapter_vendor -or
                [string]$rawMatches[0].adapter_device_id -ne [string]$cell.environment.adapter_device_id -or
                [string]$rawMatches[0].renderer_driver -ne [string]$cell.environment.renderer_driver -or
                [string]$rawMatches[0].renderer_driver_info -ne [string]$cell.environment.renderer_driver_info -or
                ($scenarioId -eq "managed_icc" -and
                    ([string]$rawMatches[0].icc_profile_sha256 -ne [string]$calibrationReplay.profile_fingerprint_sha256 -or
                     [string]$rawMatches[0].icc_processor_sha256 -ne [string]$calibrationReplay.calibration_identity_sha256 -or
                     [string]$rawMatches[0].icc_rendering_intent -ne [string]$calibrationReplay.rendering_intent)) -or
                $rawMatches[0].viewer_ready -ne $true -or $rawMatches[0].display_contract_valid -ne $true -or
                $rawMatches[0].external_texture_presented -ne $true -or $rawMatches[0].zero_readback_stages -ne $true) {
                throw "Viewer source '$scenarioId' does not derive the normalized scenario receipt."
            }
        }
    }
}
