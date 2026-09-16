param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CellObservationPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineReportPath,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{40}$")][string]$ExpectedSourceSha,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$CaptureId,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$AuthorityChallengeManifestPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$OutputDirectory,
    [string]$ProfilePath = "tests/validation/platform-gpu-color-gates.json"
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
    $bytes = [IO.File]::ReadAllBytes($item.FullName)
    if ($bytes.Length -ne $item.Length) { throw "$Label changed during admission." }
    try { $value = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff) | ConvertFrom-Json }
    catch { throw "$Label is invalid JSON: $($_.Exception.Message)" }
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $hash = (($sha256.ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join "") }
    finally { $sha256.Dispose() }
    return [pscustomobject]@{ value = $value; bytes = $bytes; sha256 = $hash; length = [long]$bytes.Length }
}

function Write-CreateOnlyJson([string]$Path, [object]$Value) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Value | ConvertTo-Json -Depth 16 -Compress))
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
    finally { $stream.Dispose() }
}

function Get-StringSha256([string]$Value) {
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        return (($sha256.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)) |
            ForEach-Object { $_.ToString("x2") }) -join "")
    } finally { $sha256.Dispose() }
}

function Invoke-BoundedCargo(
    [string[]]$Arguments,
    [int]$TimeoutSeconds,
    [string]$StdoutPath,
    [string]$StderrPath
) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Get-Command cargo -ErrorAction Stop).Source
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
        if (-not $process.Start()) { throw "Could not start Cargo qualification process." }
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderr)
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { $null = $process.WaitForExit(1000) }
        if (-not $process.HasExited) {
            try { $process.Kill($true); $process.WaitForExit() } catch { }
            throw "Cargo qualification process exceeded its $TimeoutSeconds second deadline."
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

function Get-SourceEntry([string]$Root, [string]$Role, [string]$Path, [string]$Format) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    $relative = [IO.Path]::GetRelativePath($Root, $item.FullName).Replace('\', '/')
    return [ordered]@{
        role = $Role
        path = $relative
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

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$cellAbsolute = Resolve-RepositoryPath $CellObservationPath
$machineAbsolute = Resolve-RepositoryPath $MachineReportPath
$profileAbsolute = Resolve-RepositoryPath $ProfilePath
$challengeAbsolute = Resolve-RepositoryPath $AuthorityChallengeManifestPath
$outputAbsolute = Resolve-RepositoryPath $OutputDirectory
$cellAdmission = Read-BoundedJson $cellAbsolute 8388608 "cell observation"
$machineAdmission = Read-BoundedJson $machineAbsolute 8388608 "machine report"
$profileAdmission = Read-BoundedJson $profileAbsolute 1048576 "platform GPU gate profile"
$challengeAdmission = Read-BoundedJson $challengeAbsolute 1048576 "GPU capture authority challenge"
$cell = $cellAdmission.value
$profile = $profileAdmission.value
$challenge = $challengeAdmission.value
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()

if ($profile.schema_version -ne 1 -or $profile.single_test_result_required -ne $true -or
    $profile.diagnostic_skip_forbidden -ne $true -or @($profile.gates).Count -ne 8 -or
    [string]$cell.source_revision -ne $sourceSha -or
    [string]$cell.machine_report_sha256 -ne [string]$machineAdmission.sha256) {
    throw "GPU source capture inputs do not satisfy the sealed profile/row contract."
}
if ($challenge.schema_version -ne 1 -or [string]$challenge.capture_kind -ne "gpu_color" -or
    [string]$challenge.capture_id -ne $CaptureId -or
    [string]$challenge.cell_id -ne [string]$cell.cell_id -or
    [string]$challenge.cell_run_id -ne [string]$cell.cell_run_id -or
    [string]$challenge.source_revision -ne $sourceSha -or
    [string]$challenge.release_candidate_id -ne [string]$cell.release_candidate_id -or
    [string]$challenge.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
    [string]::IsNullOrWhiteSpace([string]$challenge.authority_id) -or
    [string]::IsNullOrWhiteSpace([string]$challenge.challenge_id) -or
    [string]::IsNullOrWhiteSpace([string]$challenge.issued_at_utc) -or
    ([string]$challenge.nonce).Length -lt 32) {
    throw "GPU capture authority challenge is not pre-bound to this exact row/capture."
}
$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha -or
    @(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "GPU source capture requires the exact clean source revision."
}
if (Test-Path -LiteralPath $outputAbsolute) { throw "GPU source output already exists: $outputAbsolute" }
New-Item -ItemType Directory -Path $outputAbsolute -ErrorAction Stop | Out-Null
$profileCopyPath = Join-Path $outputAbsolute "profile.json"
[IO.File]::WriteAllBytes($profileCopyPath, $profileAdmission.bytes)
$challengeCopyPath = Join-Path $outputAbsolute "authority-challenge.json"
[IO.File]::WriteAllBytes($challengeCopyPath, $challengeAdmission.bytes)
$producerSha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()

$previousPolicy = [Environment]::GetEnvironmentVariable("MONDRIAN_GPU_COLOR_QUALIFICATION_POLICY", "Process")
$previousMeasurement = [Environment]::GetEnvironmentVariable("MONDRIAN_GPU_COLOR_GATE_MEASUREMENT_OUTPUT", "Process")
$previousIncremental = [Environment]::GetEnvironmentVariable("CARGO_INCREMENTAL", "Process")
$attestationEnvironmentNames = @(
    "MONDRIAN_QUALIFICATION_CHALLENGE_ID",
    "MONDRIAN_QUALIFICATION_CHALLENGE_MANIFEST_SHA256",
    "MONDRIAN_QUALIFICATION_CHALLENGE_NONCE",
    "MONDRIAN_QUALIFICATION_CELL_RUN_ID",
    "MONDRIAN_QUALIFICATION_SOURCE_REVISION",
    "MONDRIAN_QUALIFICATION_RUNTIME_IMAGE_SHA256",
    "MONDRIAN_QUALIFICATION_PRODUCER_SHA256"
)
$previousAttestationEnvironment = @{}
foreach ($name in $attestationEnvironmentNames) {
    $previousAttestationEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
$gateEvidence = [System.Collections.Generic.List[object]]::new()
$sourceEntries = [System.Collections.Generic.List[object]]::new()
$sourceEntries.Add((Get-SourceEntry $outputAbsolute "profile.json" $profileCopyPath "json"))
$sourceEntries.Add((Get-SourceEntry $outputAbsolute "authority-challenge.json" $challengeCopyPath "json"))
try {
    [Environment]::SetEnvironmentVariable("MONDRIAN_GPU_COLOR_QUALIFICATION_POLICY", "sealed-required", "Process")
    [Environment]::SetEnvironmentVariable("CARGO_INCREMENTAL", "0", "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CHALLENGE_ID", [string]$challenge.challenge_id, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CHALLENGE_MANIFEST_SHA256", [string]$challengeAdmission.sha256, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CHALLENGE_NONCE", [string]$challenge.nonce, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_CELL_RUN_ID", [string]$cell.cell_run_id, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_SOURCE_REVISION", $sourceSha, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_RUNTIME_IMAGE_SHA256", [string]$cell.product_artifact.runtime_image_sha256, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_QUALIFICATION_PRODUCER_SHA256", $producerSha256, "Process")
    $buildStdout = Join-Path $outputAbsolute "build.stdout"
    $buildStderr = Join-Path $outputAbsolute "build.stderr"
    $buildExit = Invoke-BoundedCargo @("test", "--locked", "-p", [string]$profile.package, "--all-features", "--no-run", "-j", "1") 3600 $buildStdout $buildStderr
    if ($buildExit -ne 0) { throw "GPU source qualification build failed with exit code $buildExit." }
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "build.stdout" $buildStdout "text"))
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "build.stderr" $buildStderr "text"))

    $qualifiedAdapter = $null
    foreach ($gate in @($profile.gates)) {
        $id = [string]$gate.id
        $stdoutPath = Join-Path $outputAbsolute "gate.$id.stdout"
        $stderrPath = Join-Path $outputAbsolute "gate.$id.stderr"
        $measurementPath = Join-Path $outputAbsolute "gate.$id.measurement.json"
        $reportPath = Join-Path $outputAbsolute "gate.$id.report.json"
        [Environment]::SetEnvironmentVariable("MONDRIAN_GPU_COLOR_GATE_MEASUREMENT_OUTPUT", $measurementPath, "Process")
        $arguments = [System.Collections.Generic.List[string]]::new()
        foreach ($argument in @("test", "--locked", "-p", [string]$profile.package, "--all-features", "-j", "1")) { $arguments.Add($argument) }
        if ([string]$gate.target -eq "lib") {
            $arguments.Add("--lib")
        } elseif ([string]$gate.target -eq "integration" -and -not [string]::IsNullOrWhiteSpace([string]$gate.integration_test)) {
            $arguments.Add("--test"); $arguments.Add([string]$gate.integration_test)
        } else {
            throw "GPU gate '$id' has an invalid target."
        }
        $arguments.Add([string]$gate.test)
        $arguments.Add("--"); $arguments.Add("--exact"); $arguments.Add("--nocapture")
        if ($gate.ignored -eq $true) { $arguments.Add("--ignored") }
        $startedAtUtc = [DateTime]::UtcNow.ToString("O")
        try { $exit = Invoke-BoundedCargo $arguments.ToArray() 1800 $stdoutPath $stderrPath }
        finally { [Environment]::SetEnvironmentVariable("MONDRIAN_GPU_COLOR_GATE_MEASUREMENT_OUTPUT", $null, "Process") }
        $finishedAtUtc = [DateTime]::UtcNow.ToString("O")
        $combined = [IO.File]::ReadAllText($stdoutPath) + "`n" + [IO.File]::ReadAllText($stderrPath)
        if ($exit -ne 0 -or $combined -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;' -or
            $combined -match '(?i)skipping diagnostic GPU color gate|skipping diagnostic native YUV color gate') {
            throw "GPU gate '$id' did not execute exactly one non-skipped passing test."
        }
        $measurementAdmission = Read-BoundedJson $measurementPath 1048576 "GPU gate '$id' measurement"
        $measurement = $measurementAdmission.value
        Assert-ExactStringSet @($gate.measurements | ForEach-Object { [string]$_.metric }) `
            @($measurement.measurements | ForEach-Object { [string]$_.metric }) "GPU gate '$id' measurement closure"
        if ($measurement.schema_version -ne 1 -or [string]$measurement.gate_id -ne $id -or
            [string]$measurement.adapter.name -ne [string]$cell.environment.adapter_name -or
            [string]$measurement.adapter.vendor_id -ne [string]$cell.environment.adapter_vendor -or
            [string]$measurement.adapter.device_id -ne [string]$cell.environment.adapter_device_id -or
            [string]$measurement.adapter.driver -ne [string]$cell.environment.renderer_driver -or
            [string]$measurement.adapter.driver_info -ne [string]$cell.environment.renderer_driver_info -or
            ([string]$measurement.adapter.backend).ToLowerInvariant() -ne
                ([string]$cell.environment.graphics_backend).ToLowerInvariant()) {
            throw "GPU gate '$id' ran on a different adapter or row."
        }
        $attestation = $measurement.attestation
        if ([string]$attestation.challenge_id -ne [string]$challenge.challenge_id -or
            [string]$attestation.challenge_manifest_sha256 -ne [string]$challengeAdmission.sha256 -or
            [string]$attestation.challenge_nonce_sha256 -ne (Get-StringSha256 ([string]$challenge.nonce)) -or
            [string]$attestation.cell_run_id -ne [string]$cell.cell_run_id -or
            [string]$attestation.source_revision -ne $sourceSha -or
            [string]$attestation.runtime_image_sha256 -ne [string]$cell.product_artifact.runtime_image_sha256 -or
            [string]$attestation.producer_script_sha256 -ne $producerSha256 -or
            [string]$attestation.test_executable_sha256 -notmatch '^[0-9a-f]{64}$' -or
            [uint64]$attestation.process_id -eq 0 -or [uint64]$attestation.recorded_unix_nanos -eq 0) {
            throw "GPU gate '$id' did not echo the pre-issued authority challenge from the test process."
        }
        $adapterFingerprint = "$($measurement.adapter.name)|$($measurement.adapter.vendor_id)|$($measurement.adapter.device_id)|$($measurement.adapter.device_type)|$($measurement.adapter.driver)|$($measurement.adapter.driver_info)|$($measurement.adapter.backend)"
        if ($null -eq $qualifiedAdapter) { $qualifiedAdapter = $adapterFingerprint }
        elseif ($qualifiedAdapter -ne $adapterFingerprint) { throw "GPU gates ran on different adapters." }
        $normalizedMeasurements = [System.Collections.Generic.List[object]]::new()
        foreach ($contract in @($gate.measurements)) {
            $observed = @($measurement.measurements | Where-Object { [string]$_.metric -eq [string]$contract.metric })
            if ($observed.Count -ne 1 -or -not [double]::IsFinite([double]$observed[0].value)) {
                throw "GPU gate '$id' emitted an invalid measurement '$($contract.metric)'."
            }
            $within = switch ([string]$contract.comparison) {
                "at_most" { [double]$observed[0].value -le [double]$contract.limit }
                "at_least" { [double]$observed[0].value -ge [double]$contract.limit }
                default { $false }
            }
            if (-not $within) { throw "GPU gate '$id' measurement '$($contract.metric)' failed." }
            $normalizedMeasurements.Add([ordered]@{
                metric = [string]$contract.metric
                value = [double]$observed[0].value
                comparison = [string]$contract.comparison
                limit = [double]$contract.limit
            })
        }
        $stdoutHash = (Get-FileHash -LiteralPath $stdoutPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $stderrHash = (Get-FileHash -LiteralPath $stderrPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $gateReport = [ordered]@{
            schema_version = 3
            gate_id = $id
            status = "qualified"
            skipped = $false
            source_revision = $sourceSha
            cell_run_id = [string]$cell.cell_run_id
            runtime_image_sha256 = [string]$cell.product_artifact.runtime_image_sha256
            adapter_name = [string]$cell.environment.adapter_name
            adapter_device_id = [string]$cell.environment.adapter_device_id
            cargo_test = [string]$gate.test
            exact_argv = @($arguments)
            started_at_utc = $startedAtUtc
            finished_at_utc = $finishedAtUtc
            authority_challenge_manifest_sha256 = [string]$challengeAdmission.sha256
            producer_script_sha256 = $producerSha256
            test_executable_sha256 = [string]$attestation.test_executable_sha256
            test_process_id = [uint64]$attestation.process_id
            stdout_sha256 = $stdoutHash
            stderr_sha256 = $stderrHash
            measurement_sha256 = [string]$measurementAdmission.sha256
            measurements = @($normalizedMeasurements)
        }
        Write-CreateOnlyJson $reportPath $gateReport
        $reportHash = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $gateEvidence.Add([ordered]@{
            id = $id
            test = [string]$gate.test
            target = [string]$gate.target
            integration_test = if ($null -eq $gate.integration_test) { $null } else { [string]$gate.integration_test }
            ignored = [bool]$gate.ignored
            exact_argv = @($arguments)
            started_at_utc = $startedAtUtc
            finished_at_utc = $finishedAtUtc
            test_executable_sha256 = [string]$attestation.test_executable_sha256
            test_process_id = [uint64]$attestation.process_id
            exit_code = 0
            timed_out = $false
            passed = $true
            stdout_sha256 = $stdoutHash
            stderr_sha256 = $stderrHash
            report_sha256 = $reportHash
        })
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "gate.$id.stdout" $stdoutPath "text"))
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "gate.$id.stderr" $stderrPath "text"))
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "gate.$id.measurement.json" $measurementPath "json"))
        $sourceEntries.Add((Get-SourceEntry $outputAbsolute "gate.$id.report.json" $reportPath "json"))
    }

    $adapterParts = $qualifiedAdapter.Split('|')
    $summaryPath = Join-Path $outputAbsolute "supervisor-summary.json"
    $summary = [ordered]@{
        schema_version = 2
        status = "passed"
        source_sha = $sourceSha
        machine = [ordered]@{ report_sha256 = [string]$machineAdmission.sha256 }
        profile = [ordered]@{ sha256 = [string]$profileAdmission.sha256 }
        authority_challenge = [ordered]@{
            authority_id = [string]$challenge.authority_id
            challenge_id = [string]$challenge.challenge_id
            manifest_sha256 = [string]$challengeAdmission.sha256
            nonce_sha256 = Get-StringSha256 ([string]$challenge.nonce)
        }
        producer = [ordered]@{
            id = "mondrian-platform-gpu-color-source-v1"
            script_sha256 = $producerSha256
        }
        build = [ordered]@{
            exit_code = 0
            timed_out = $false
            stdout_sha256 = (Get-FileHash -LiteralPath $buildStdout -Algorithm SHA256).Hash.ToLowerInvariant()
            stderr_sha256 = (Get-FileHash -LiteralPath $buildStderr -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        adapter = [ordered]@{
            name = $adapterParts[0]
            vendor_id = $adapterParts[1]
            device_id = $adapterParts[2]
            device_type = $adapterParts[3]
            driver = $adapterParts[4]
            driver_info = $adapterParts[5]
            backend = $adapterParts[6]
        }
        gates = @($gateEvidence)
    }
    Write-CreateOnlyJson $summaryPath $summary
    $sourceEntries.Add((Get-SourceEntry $outputAbsolute "supervisor-summary.json" $summaryPath "json"))

    $rawPath = Join-Path $outputAbsolute "normalized-raw.json"
    Write-CreateOnlyJson $rawPath ([ordered]@{
        schema_version = 1
        kind = "gpu_color"
        owner = "mondrian-renderer"
        verifier_id = "gpu-color-qualification-v1"
        source_revision = $sourceSha
        cell_run_id = [string]$cell.cell_run_id
        machine_report_sha256 = [string]$cell.machine_report_sha256
        product_artifact_sha256 = [string]$cell.product_artifact.sha256
        runtime_image_sha256 = [string]$cell.product_artifact.runtime_image_sha256
        release_candidate_id = [string]$cell.release_candidate_id
        build_manifest_sha256 = [string]$cell.build_manifest_sha256
        build_provenance_sha256 = [string]$cell.product_artifact.build_provenance_sha256
        environment_sha256 = [string]$cell.environment_before_sha256
        environment = $cell.environment
        gates = @($gateEvidence | ForEach-Object {
            [ordered]@{
                id = $_.id
                status = "qualified"
                report_sha256 = $_.report_sha256
                adapter_name = [string]$cell.environment.adapter_name
                adapter_device_id = [string]$cell.environment.adapter_device_id
                adapter_vendor = [string]$cell.environment.adapter_vendor
                renderer_driver = [string]$cell.environment.renderer_driver
                renderer_driver_info = [string]$cell.environment.renderer_driver_info
            }
        })
    })
    $templatePath = Join-Path $outputAbsolute "source-evidence-template.json"
    Write-CreateOnlyJson $templatePath ([ordered]@{
        schema_version = 1
        source_verifier_id = "gpu-color-source-replay-v1"
        capture_id = $CaptureId
        bindings = [ordered]@{
            cell_id = [string]$cell.cell_id
            cell_run_id = [string]$cell.cell_run_id
            source_revision = $sourceSha
            release_candidate_id = [string]$cell.release_candidate_id
            build_manifest_sha256 = [string]$cell.build_manifest_sha256
            build_provenance_sha256 = [string]$cell.product_artifact.build_provenance_sha256
            product_artifact_sha256 = [string]$cell.product_artifact.sha256
            runtime_image_sha256 = [string]$cell.product_artifact.runtime_image_sha256
            machine_report_sha256 = [string]$cell.machine_report_sha256
            environment_sha256 = [string]$cell.environment_before_sha256
            authority_challenge_id = [string]$challenge.challenge_id
            authority_challenge_sha256 = [string]$challengeAdmission.sha256
            producer_id = "mondrian-platform-gpu-color-source-v1"
            producer_sha256 = $producerSha256
            session_transcript_role = "supervisor-summary.json"
        }
        entries = @($sourceEntries | Sort-Object { $_.role })
    })
} finally {
    [Environment]::SetEnvironmentVariable("MONDRIAN_GPU_COLOR_QUALIFICATION_POLICY", $previousPolicy, "Process")
    [Environment]::SetEnvironmentVariable("MONDRIAN_GPU_COLOR_GATE_MEASUREMENT_OUTPUT", $previousMeasurement, "Process")
    [Environment]::SetEnvironmentVariable("CARGO_INCREMENTAL", $previousIncremental, "Process")
    foreach ($name in $attestationEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($name, $previousAttestationEnvironment[$name], "Process")
    }
}

Write-Host "Platform GPU color source capture: passed; output: $outputAbsolute"
