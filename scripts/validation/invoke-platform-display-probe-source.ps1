param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$RuntimeProfilePath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CellObservationPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineReportPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CapturePlanPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$AuthorityChallengeManifestPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$TransitionAcknowledgementDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{40}$")][string]$ExpectedSourceSha,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CaptureId,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Read-BoundedJson([string]$Path, [long]$MaximumBytes, [string]$Label) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $MaximumBytes -or $item.Length -gt [int]::MaxValue) {
        throw "$Label is not a bounded regular non-link file: $Path"
    }
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $bytes = [byte[]]::new([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -le 0) { throw "$Label ended before its admitted length." }
            $offset += $read
        }
    } finally { $stream.Dispose() }
    try { $value = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff) | ConvertFrom-Json }
    catch { throw "$Label is invalid JSON: $($_.Exception.Message)" }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $hash = (($sha256.ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join "") }
    finally { $sha256.Dispose() }
    return [pscustomobject]@{ value = $value; bytes = $bytes; sha256 = $hash; length = [long]$bytes.Length }
}

function Write-CreateOnlyJson([string]$Path, [object]$Value) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Value | ConvertTo-Json -Depth 32 -Compress))
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
    finally { $stream.Dispose() }
}

function Write-CreateOnlyBytes([string]$Path, [byte[]]$Bytes) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($Bytes, 0, $Bytes.Length); $stream.Flush($true) }
    finally { $stream.Dispose() }
}

function Get-StringSha256([string]$Value) {
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        return (($sha256.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)) |
            ForEach-Object { $_.ToString("x2") }) -join "")
    } finally { $sha256.Dispose() }
}

function Get-SourceEntry([string]$Root, [string]$Role, [string]$Path, [string]$Format) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    return [ordered]@{
        role = $Role
        path = [IO.Path]::GetRelativePath($Root, $item.FullName).Replace('\', '/')
        sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        byte_length = [long]$item.Length
        format = $Format
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
        if (-not $process.Start()) { throw "Could not start platform display probe process." }
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderr)
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { $null = $process.WaitForExit(1000) }
        if (-not $process.HasExited) {
            try { $process.Kill($true); $process.WaitForExit() } catch { }
            throw "Platform display probe process exceeded its $TimeoutSeconds second deadline."
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

function Get-IccProfileFingerprint([byte[]]$Bytes) {
    $domain = [Text.Encoding]::ASCII.GetBytes("mondrian.icc-profile-fingerprint.v1")
    $domainLength = [BitConverter]::GetBytes([uint64]$domain.Length)
    $payloadLength = [BitConverter]::GetBytes([uint64]$Bytes.Length)
    if (-not [BitConverter]::IsLittleEndian) { [Array]::Reverse($domainLength); [Array]::Reverse($payloadLength) }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $null = $sha256.TransformBlock($domainLength, 0, $domainLength.Length, $null, 0)
        $null = $sha256.TransformBlock($domain, 0, $domain.Length, $null, 0)
        $null = $sha256.TransformBlock($payloadLength, 0, $payloadLength.Length, $null, 0)
        $null = $sha256.TransformFinalBlock($Bytes, 0, $Bytes.Length)
        return (($sha256.Hash | ForEach-Object { $_.ToString("x2") }) -join "")
    } finally { $sha256.Dispose() }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()
$profileAdmission = Read-BoundedJson (Resolve-RepositoryPath $RuntimeProfilePath) 1048576 "runtime profile"
$cellAdmission = Read-BoundedJson (Resolve-RepositoryPath $CellObservationPath) 8388608 "cell observation"
$machineAdmission = Read-BoundedJson (Resolve-RepositoryPath $MachineReportPath) 8388608 "machine report"
$planAdmission = Read-BoundedJson (Resolve-RepositoryPath $CapturePlanPath) 1048576 "platform capture plan"
$challengeAdmission = Read-BoundedJson (Resolve-RepositoryPath $AuthorityChallengeManifestPath) 1048576 "platform authority challenge"
$profile = $profileAdmission.value
$cell = $cellAdmission.value
$plan = $planAdmission.value
$challenge = $challengeAdmission.value
$matchingCells = @($profile.cells | Where-Object { [string]$_.cell_id -eq [string]$cell.cell_id })
if ($matchingCells.Count -ne 1 -or [string]$cell.source_revision -ne $sourceSha -or
    [string]$cell.machine_report_sha256 -ne [string]$machineAdmission.sha256) {
    throw "Platform source capture has no unique profile cell or machine binding."
}
$profileCell = $matchingCells[0]
if ($plan.schema_version -ne 1 -or [string]$plan.capture_id -ne $CaptureId -or
    [string]$plan.cell_id -ne [string]$cell.cell_id -or [string]$plan.cell_run_id -ne [string]$cell.cell_run_id -or
    [string]$plan.source_revision -ne $sourceSha -or [string]$plan.environment_sha256 -ne [string]$cell.environment_before_sha256 -or
    [string]$plan.native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
    [string]$plan.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    [uint32]$plan.target.width -eq 0 -or [uint32]$plan.target.height -eq 0) {
    throw "Platform capture plan is not bound to the exact row/display."
}
if ($challenge.schema_version -ne 1 -or [string]$challenge.capture_kind -ne "platform_probe" -or
    [string]$challenge.capture_id -ne $CaptureId -or [string]$challenge.cell_id -ne [string]$cell.cell_id -or
    [string]$challenge.cell_run_id -ne [string]$cell.cell_run_id -or [string]$challenge.source_revision -ne $sourceSha -or
    [string]$challenge.release_candidate_id -ne [string]$cell.release_candidate_id -or
    [string]$challenge.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    ([string]$challenge.nonce).Length -lt 32 -or [string]::IsNullOrWhiteSpace([string]$challenge.challenge_id) -or
    [string]::IsNullOrWhiteSpace([string]$challenge.transition_authority_id)) {
    throw "Platform authority challenge is not pre-bound to the exact capture."
}
$expectedCaptureKeys = [System.Collections.Generic.List[string]]::new()
foreach ($scenario in @($profileCell.required_scenarios)) {
    if ([string]$scenario.scenario -eq "managed_icc") {
        $expectedCaptureKeys.Add("managed_icc|icc")
        $expectedCaptureKeys.Add("managed_icc|hdr")
    } else { $expectedCaptureKeys.Add("$($scenario.scenario)|hdr") }
}
Assert-ExactStringSet @($expectedCaptureKeys) @($plan.captures | ForEach-Object { "$($_.scenario)|$($_.probe_kind)" }) `
    "platform capture plan scenario/probe closure"
foreach ($capture in @($plan.captures)) {
    if ([string]$capture.expected_backend -notin @($profileCell.required_probe_backends | ForEach-Object { [string]$_ }) -or
        [string]$capture.transition_id -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$' -or
        $null -eq $capture.required_state) {
        throw "Platform capture '$($capture.scenario)/$($capture.probe_kind)' uses an unapproved backend."
    }
    $scenarioRequirement = @($profileCell.required_scenarios | Where-Object {
        [string]$_.scenario -eq [string]$capture.scenario
    })
    if ($scenarioRequirement.Count -ne 1) { throw "Platform capture references an unknown scenario." }
    $expectedState = switch ([string]$capture.scenario) {
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
        throw "Platform capture '$($capture.scenario)/$($capture.probe_kind)' weakens its required display state."
    }
}
Assert-ExactStringSet @($plan.captures | ForEach-Object { [string]$_.transition_id }) `
    @($plan.captures | ForEach-Object { [string]$_.transition_id }) "platform transition identity closure"
$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Platform source capture requires the exact clean source revision."
}
$outputAbsolute = Resolve-RepositoryPath $OutputDirectory
if (Test-Path -LiteralPath $outputAbsolute) { throw "Platform source output already exists: $outputAbsolute" }
$transitionDirectory = Resolve-RepositoryPath $TransitionAcknowledgementDirectory
$transitionItem = Get-Item -LiteralPath $transitionDirectory -ErrorAction Stop
if (-not $transitionItem.PSIsContainer -or
    ($transitionItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
    @(Get-ChildItem -LiteralPath $transitionDirectory -Force).Count -ne 0) {
    throw "Transition acknowledgement directory must be an empty regular directory at capture start."
}
New-Item -ItemType Directory -Path $outputAbsolute -ErrorAction Stop | Out-Null
$planCopyPath = Join-Path $outputAbsolute "capture-plan.json"
$challengeCopyPath = Join-Path $outputAbsolute "authority-challenge.json"
[IO.File]::WriteAllBytes($planCopyPath, $planAdmission.bytes)
[IO.File]::WriteAllBytes($challengeCopyPath, $challengeAdmission.bytes)
$buildStdout = Join-Path $outputAbsolute "build.stdout"
$buildStderr = Join-Path $outputAbsolute "build.stderr"
$buildTarget = Join-Path $outputAbsolute ".build-target"
$previousTarget = [Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR", "Process")
$environmentNames = @(
    "MONDRIAN_QUALIFICATION_CHALLENGE_ID", "MONDRIAN_QUALIFICATION_CHALLENGE_MANIFEST_SHA256",
    "MONDRIAN_QUALIFICATION_CHALLENGE_NONCE", "MONDRIAN_QUALIFICATION_CELL_RUN_ID",
    "MONDRIAN_QUALIFICATION_SOURCE_REVISION", "MONDRIAN_QUALIFICATION_RUNTIME_IMAGE_SHA256",
    "MONDRIAN_QUALIFICATION_PRODUCER_SHA256"
)
$previousEnvironment = @{}
foreach ($name in $environmentNames) { $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process") }
$sourceEntries = [System.Collections.Generic.List[object]]::new()
$captureSummaries = [System.Collections.Generic.List[object]]::new()
$rawScenarios = [System.Collections.Generic.List[object]]::new()
try {
    [Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", $buildTarget, "Process")
    $cargo = (Get-Command cargo -ErrorAction Stop).Source
    $buildExit = Invoke-BoundedProcess $cargo @(
        "build", "--locked", "-p", "mondrian-platform", "--bin", "mondrian-platform-display-probe-source", "-j", "1"
    ) $repositoryRoot 1800 $buildStdout $buildStderr
    if ($buildExit -ne 0) { throw "Platform display probe producer build failed with exit code $buildExit." }
    $executableName = if ($IsWindows) { "mondrian-platform-display-probe-source.exe" } else { "mondrian-platform-display-probe-source" }
    $builtProducerExecutable = Join-Path (Join-Path $buildTarget "debug") $executableName
    $producerExecutable = Join-Path $outputAbsolute $executableName
    [IO.File]::Copy($builtProducerExecutable, $producerExecutable, $false)
    if (-not $IsWindows) {
        & chmod 500 $producerExecutable
        if ($LASTEXITCODE -ne 0) { throw "Could not make the captured platform producer executable." }
    }
    $producerSha256 = (Get-FileHash -LiteralPath $producerExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    $supervisorSha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CHALLENGE_ID", [string]$challenge.challenge_id, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CHALLENGE_MANIFEST_SHA256", [string]$challengeAdmission.sha256, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CHALLENGE_NONCE", [string]$challenge.nonce, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CELL_RUN_ID", [string]$cell.cell_run_id, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_SOURCE_REVISION", $sourceSha, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_RUNTIME_IMAGE_SHA256", [string]$cell.product_artifact.runtime_image_sha256, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_PRODUCER_SHA256", $supervisorSha256, "Process")
    $sequence = 0
    $previousTranscriptSha256 = ""
    $transcripts = @{}
    foreach ($capture in @($plan.captures)) {
        $sequence += 1
        $scenarioId = [string]$capture.scenario
        $probeKind = [string]$capture.probe_kind
        $base = "probe.$scenarioId.$probeKind"
        $requestPath = Join-Path $outputAbsolute "$base.request.json"
        $transcriptPath = Join-Path $outputAbsolute "$base.json"
        $iccPath = Join-Path $outputAbsolute "$base.icc-profile"
        $stdoutPath = Join-Path $outputAbsolute "$base.stdout"
        $stderrPath = Join-Path $outputAbsolute "$base.stderr"
        Write-CreateOnlyJson $requestPath ([ordered]@{
            schema_version = 1; capture_id = $CaptureId; sequence = $sequence
            scenario = $scenarioId; probe_kind = $probeKind; expected_backend = [string]$capture.expected_backend
            cell_id = [string]$cell.cell_id; cell_run_id = [string]$cell.cell_run_id; source_revision = $sourceSha
            release_candidate_id = [string]$cell.release_candidate_id
            runtime_image_sha256 = [string]$cell.product_artifact.runtime_image_sha256
            environment_sha256 = [string]$cell.environment_before_sha256
            display_identity = [string]$cell.environment.display_identity
            native_display_path_id = [string]$cell.environment.native_display_path_id
            display_inventory_sha256 = [string]$cell.environment.display_inventory_sha256
            target = $plan.target
        })
        $transitionRequestedAt = [DateTimeOffset]::UtcNow
        $transitionId = [string]$capture.transition_id
        $transitionSourcePath = Join-Path $transitionDirectory "$transitionId.json"
        $transitionAckPath = Join-Path $outputAbsolute "transition.$sequence.ack.json"
        Write-Host "Awaiting authority transition '$transitionId' for '$scenarioId/$probeKind'."
        $transitionDeadline = [DateTimeOffset]::UtcNow.AddHours(1)
        while (-not (Test-Path -LiteralPath $transitionSourcePath) -and [DateTimeOffset]::UtcNow -lt $transitionDeadline) {
            Start-Sleep -Milliseconds 500
        }
        if (-not (Test-Path -LiteralPath $transitionSourcePath)) {
            throw "Authority transition '$transitionId' was not acknowledged within one hour."
        }
        $transitionAdmission = Read-BoundedJson $transitionSourcePath 1048576 "transition acknowledgement '$transitionId'"
        $transitionAck = $transitionAdmission.value
        $acknowledgedAt = [DateTimeOffset]::MinValue
        if (-not [DateTimeOffset]::TryParseExact(
                [string]$transitionAck.acknowledged_at_utc, "O", [Globalization.CultureInfo]::InvariantCulture,
                [Globalization.DateTimeStyles]::RoundtripKind, [ref]$acknowledgedAt
            ) -or $acknowledgedAt.Offset -ne [TimeSpan]::Zero -or
            $acknowledgedAt -lt $transitionRequestedAt -or
            $acknowledgedAt -gt [DateTimeOffset]::UtcNow.AddMinutes(5) -or
            $transitionAck.schema_version -ne 1 -or [string]$transitionAck.transition_id -ne $transitionId -or
            [string]$transitionAck.capture_id -ne $CaptureId -or [uint32]$transitionAck.sequence -ne $sequence -or
            [string]$transitionAck.scenario -ne $scenarioId -or [string]$transitionAck.probe_kind -ne $probeKind -or
            [string]$transitionAck.cell_id -ne [string]$cell.cell_id -or
            [string]$transitionAck.cell_run_id -ne [string]$cell.cell_run_id -or
            [string]$transitionAck.challenge_id -ne [string]$challenge.challenge_id -or
            [string]$transitionAck.challenge_manifest_sha256 -ne [string]$challengeAdmission.sha256 -or
            [string]$transitionAck.challenge_nonce_sha256 -ne (Get-StringSha256 ([string]$challenge.nonce)) -or
            [string]$transitionAck.authority_id -ne [string]$challenge.transition_authority_id -or
            [string]$transitionAck.previous_transcript_sha256 -ne $previousTranscriptSha256 -or
            [string]$transitionAck.required_state.hdr_enabled -ne [string]$capture.required_state.hdr_enabled -or
            [string]$transitionAck.required_state.wide_color_active -ne [string]$capture.required_state.wide_color_active -or
            [string]$transitionAck.required_state.active_transfer_function -ne [string]$capture.required_state.active_transfer_function) {
            throw "Authority transition '$transitionId' is not a fresh exact scenario acknowledgement."
        }
        Write-CreateOnlyBytes $transitionAckPath $transitionAdmission.bytes
        $startedAt = [DateTimeOffset]::UtcNow.ToString("O", [Globalization.CultureInfo]::InvariantCulture)
        $exit = Invoke-BoundedProcess $producerExecutable @($requestPath, $transcriptPath, $iccPath) `
            $repositoryRoot 300 $stdoutPath $stderrPath
        $finishedAt = [DateTimeOffset]::UtcNow.ToString("O", [Globalization.CultureInfo]::InvariantCulture)
        if ($exit -ne 0) { throw "Native platform capture '$scenarioId/$probeKind' failed with exit code $exit." }
        $transcriptAdmission = Read-BoundedJson $transcriptPath 8388608 "native transcript '$scenarioId/$probeKind'"
        $transcript = $transcriptAdmission.value
        if ([string]$transcript.producer_executable_sha256 -ne $producerSha256 -or
            [string]$transcript.supervisor_script_sha256 -ne $supervisorSha256 -or
            [string]$transcript.result.backend -ne [string]$capture.expected_backend -or
            [string]$transcript.result.resolved_native_display_path_id -ne [string]$cell.environment.native_display_path_id -or
            $transcript.result.discovery_available -ne $true -or $null -ne $transcript.result.error) {
            throw "Native platform capture '$scenarioId/$probeKind' did not execute the approved producer/backend."
        }
        if ($probeKind -eq "hdr") {
            foreach ($stateField in @("hdr_enabled", "wide_color_active", "active_transfer_function")) {
                $requiredValue = $capture.required_state.$stateField
                if ($null -ne $requiredValue -and
                    [string]$transcript.result.details.$stateField -ne [string]$requiredValue) {
                    throw "Native platform capture '$scenarioId/$probeKind' does not match acknowledged state '$stateField'."
                }
            }
        }
        $transcripts["$scenarioId|$probeKind"] = [pscustomobject]@{
            value = $transcript; path = $transcriptPath; sha256 = $transcriptAdmission.sha256; icc_path = $iccPath
        }
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "$base.request.json" $requestPath "json"))
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "$base.json" $transcriptPath "json"))
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "transition.$sequence.ack.json" $transitionAckPath "json"))
        if ($probeKind -eq "icc") {
            $sourceEntries.Add((Get-SourceEntry $outputAbsolute "$base.icc-profile" $iccPath "binary"))
        }
        $captureSummaries.Add([ordered]@{
            scenario = $scenarioId; probe_kind = $probeKind; expected_backend = [string]$capture.expected_backend
            sequence = $sequence; started_at_utc = $startedAt; finished_at_utc = $finishedAt; exit_code = 0
            transition_requested_at_utc = $transitionRequestedAt.ToString("O", [Globalization.CultureInfo]::InvariantCulture)
            transcript_sha256 = [string]$transcriptAdmission.sha256; process_id = [uint64]$transcript.process_id
            transition_id = $transitionId; transition_ack_sha256 = [string]$transitionAdmission.sha256
        })
        $previousTranscriptSha256 = [string]$transcriptAdmission.sha256
    }
    foreach ($scenario in @($profileCell.required_scenarios)) {
        $scenarioId = [string]$scenario.scenario
        $hdr = $transcripts["$scenarioId|hdr"]
        $details = $hdr.value.result.details
        $iccFingerprint = ""
        $sourceBackend = [string]$hdr.value.result.backend
        $sourceHash = [string]$hdr.sha256
        if ($scenarioId -eq "managed_icc") {
            $icc = $transcripts["managed_icc|icc"]
            $iccFingerprint = Get-IccProfileFingerprint ([IO.File]::ReadAllBytes([string]$icc.icc_path))
            $sourceBackend = [string]$icc.value.result.backend
            $sourceHash = [string]$icc.sha256
        }
        $rawScenarios.Add([ordered]@{
            scenario = $scenarioId; status = "qualified"; source_backend = $sourceBackend; source_sha256 = $sourceHash
            surface_color_space = [string]$scenario.surface_color_space; transfer = [string]$scenario.transfer
            bits_per_color_channel = $details.bits_per_color_channel
            wide_color_supported = $details.wide_color_supported; wide_color_active = $details.wide_color_active
            hdr_supported = $details.hdr_supported; hdr_enabled = $details.hdr_enabled
            active_hdr_transfer = $details.active_transfer_function; peak_luminance_nits = $details.max_luminance_nits
            edr_headroom_ppm = $details.current_headroom_ppm; hdr_presentation = $scenario.hdr_presentation
            icc_profile_sha256 = $iccFingerprint
        })
    }
    $summaryPath = Join-Path $outputAbsolute "supervisor-summary.json"
    Write-CreateOnlyJson $summaryPath ([ordered]@{
        schema_version = 1; status = "passed"; source_revision = $sourceSha; capture_id = $CaptureId
        authority_challenge_id = [string]$challenge.challenge_id
        authority_challenge_manifest_sha256 = [string]$challengeAdmission.sha256
        capture_plan_sha256 = [string]$planAdmission.sha256
        producer_id = "mondrian-platform-display-probe-source-v1"
        producer_executable_sha256 = $producerSha256; supervisor_script_sha256 = $supervisorSha256
        captures = @($captureSummaries)
    })
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "capture-plan.json" $planCopyPath "json"))
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "authority-challenge.json" $challengeCopyPath "json"))
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "build.stdout" $buildStdout "text"))
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "build.stderr" $buildStderr "text"))
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "supervisor-summary.json" $summaryPath "json"))
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "producer-binary" $producerExecutable "binary"))
    $rawPath = Join-Path $outputAbsolute "normalized-raw.json"
    $probeResults = @($profileCell.required_probe_backends | ForEach-Object {
        $backend = [string]$_
        [ordered]@{
            backend = $backend; status = "qualified"; display_identity = [string]$cell.environment.display_identity
            native_display_path_id = [string]$cell.environment.native_display_path_id
            environment_sha256 = [string]$cell.environment_before_sha256
            source_sha256s = @($transcripts.Values | Where-Object { [string]$_.value.result.backend -eq $backend } | ForEach-Object { [string]$_.sha256 } | Sort-Object)
        }
    })
    Write-CreateOnlyJson $rawPath ([ordered]@{
        schema_version = 1; kind = "platform_probe"; owner = "mondrian-platform"
        verifier_id = "platform-display-probe-v1"; source_revision = $sourceSha
        cell_run_id = [string]$cell.cell_run_id; machine_report_sha256 = [string]$cell.machine_report_sha256
        product_artifact_sha256 = [string]$cell.product_artifact.sha256
        runtime_image_sha256 = [string]$cell.product_artifact.runtime_image_sha256
        release_candidate_id = [string]$cell.release_candidate_id; build_manifest_sha256 = [string]$cell.build_manifest_sha256
        build_provenance_sha256 = [string]$cell.product_artifact.build_provenance_sha256
        environment_sha256 = [string]$cell.environment_before_sha256; environment = $cell.environment
        probe_results = $probeResults; scenarios = @($rawScenarios)
    })
    $templatePath = Join-Path $outputAbsolute "source-evidence-template.json"
    Write-CreateOnlyJson $templatePath ([ordered]@{
        schema_version = 1; source_verifier_id = "platform-display-probe-source-replay-v1"; capture_id = $CaptureId
        bindings = [ordered]@{
            cell_id = [string]$cell.cell_id; cell_run_id = [string]$cell.cell_run_id; source_revision = $sourceSha
            release_candidate_id = [string]$cell.release_candidate_id; build_manifest_sha256 = [string]$cell.build_manifest_sha256
            build_provenance_sha256 = [string]$cell.product_artifact.build_provenance_sha256
            product_artifact_sha256 = [string]$cell.product_artifact.sha256
            runtime_image_sha256 = [string]$cell.product_artifact.runtime_image_sha256
            machine_report_sha256 = [string]$cell.machine_report_sha256
            environment_sha256 = [string]$cell.environment_before_sha256
            authority_challenge_id = [string]$challenge.challenge_id
            authority_challenge_sha256 = [string]$challengeAdmission.sha256
            producer_id = "mondrian-platform-display-probe-source-v1"
            producer_sha256 = $producerSha256
            session_transcript_role = "supervisor-summary.json"
        }
        entries = @($sourceEntries | Sort-Object { $_.role })
    })
} finally {
    [Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", $previousTarget, "Process")
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], "Process") }
    if (Test-Path -LiteralPath $buildTarget) { Remove-Item -LiteralPath $buildTarget -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Host "Platform native display source capture: passed; output: $outputAbsolute"
