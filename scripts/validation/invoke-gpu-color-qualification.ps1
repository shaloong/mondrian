param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineReportPath,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$MachineId,
    [Parameter(Mandatory = $true)][ValidatePattern("^[0-9a-fA-F]{40}$")][string]$ExpectedSourceSha,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$OutputDirectory,
    [string]$ProfilePath = "tests/validation/gpu-color-qualification.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if (-not $IsWindows) { throw "Sealed GPU color qualification requires Windows." }

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Read-JsonObject([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Label is missing: $Path" }
    try { return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json }
    catch { throw "$Label is not valid JSON: $($_.Exception.Message)" }
}

function Read-NestedProperty([object]$Value, [string]$Path) {
    $current = $Value
    foreach ($segment in $Path.Split('.')) {
        $property = $current.PSObject.Properties[$segment]
        if ($null -eq $property) { throw "Report property '$Path' is missing at '$segment'." }
        $current = $property.Value
    }
    return $current
}

function Invoke-BoundedCargoGate(
    [string]$GateId,
    [string[]]$Arguments,
    [int]$TimeoutSeconds,
    [string]$StdoutPath,
    [string]$StderrPath
) {
    $cargo = (Get-Command cargo -ErrorAction Stop).Source
    $process = Start-Process -FilePath $cargo -ArgumentList $Arguments `
        -WorkingDirectory $script:repositoryRoot -WindowStyle Hidden `
        -RedirectStandardOutput $StdoutPath -RedirectStandardError $StderrPath -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        $null = $process.WaitForExit(1000)
    }
    if (-not $process.HasExited) {
        try {
            $process.Kill($true)
            $process.WaitForExit()
        } catch { }
        throw "GPU color gate '$GateId' exceeded its $TimeoutSeconds second process deadline."
    }
    $process.WaitForExit()
    return $process.ExitCode
}

function Normalize-AdapterName([string]$Name) {
    return ($Name.ToLowerInvariant() -replace '[^a-z0-9]', '')
}

function Assert-ExactStringSet([object[]]$Expected, [object[]]$Actual, [string]$Label) {
    $expectedValues = @($Expected | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    $actualValues = @($Actual | ForEach-Object { [string]$_ } | Sort-Object -Unique)
    if (@(Compare-Object $expectedValues $actualValues).Count -ne 0) {
        throw "$Label does not exactly match the sealed qualification contract."
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$profileAbsolute = Resolve-RepositoryPath $ProfilePath
$machineAbsolute = Resolve-RepositoryPath $MachineReportPath
$outputAbsolute = Resolve-RepositoryPath $OutputDirectory
$profile = Read-JsonObject $profileAbsolute "GPU color qualification profile"
$machine = Read-JsonObject $machineAbsolute "Windows reference machine report"
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()

if ($profile.schema_version -ne 1 -or $profile.execution_policy -ne "sealed-required") {
    throw "GPU color qualification profile must be schema 1 and sealed-required."
}
Assert-ExactStringSet `
    @("self-hosted", "Windows", "X64", "mondrian-reference", "mondrian-gpu-color") `
    @($profile.required_runner_labels) `
    "GPU color runner labels"
if ($profile.policy_environment -ne "MONDRIAN_GPU_COLOR_QUALIFICATION_POLICY") {
    throw "GPU color qualification profile uses an unknown policy environment binding."
}
if (
    $profile.required_adapter.backend -ne "Dx12" -or
    $profile.required_adapter.device_type -ne "DiscreteGpu" -or
    $profile.required_adapter.machine_inventory_name_match_required -ne $true -or
    $profile.required_adapter.driver_identity_required -ne $true -or
    $profile.required_adapter.same_adapter_across_reports_required -ne $true
) {
    throw "GPU color qualification profile must retain the sealed DX12 discrete-adapter identity contract."
}
if (
    $profile.acceptance.all_gates_must_run -ne $true -or
    $profile.acceptance.all_gate_processes_must_pass -ne $true -or
    $profile.acceptance.skipped_reports_forbidden -ne $true -or
    $profile.acceptance.report_hashes_required -ne $true -or
    $profile.acceptance.source_sha_required -ne $true -or
    $profile.acceptance.clean_machine_report_required -ne $true
) {
    throw "GPU color qualification profile has weakened a mandatory acceptance invariant."
}
if (@($profile.gates).Count -lt 1 -or @($profile.gates.id | Sort-Object -Unique).Count -ne @($profile.gates).Count) {
    throw "GPU color qualification profile requires a non-empty unique gate set."
}
if ($machine.schema_version -ne 3 -or $machine.machine_id -ne $MachineId) {
    throw "Machine report identity does not match the requested sealed runner."
}
if ($machine.git.revision -ne $sourceSha -or $machine.git.dirty -ne $false) {
    throw "Machine report is not bound to the expected clean source SHA."
}
if (@($machine.gpus).Count -lt 1) { throw "Machine report contains no GPU inventory." }

$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha) {
    throw "Checked-out source SHA '$headSha' does not match '$sourceSha'."
}
if (@(& git -C $repositoryRoot status --porcelain --untracked-files=normal).Count -ne 0) {
    throw "Sealed GPU color qualification requires a clean checkout."
}
if (Test-Path -LiteralPath $outputAbsolute) {
    throw "GPU color qualification output directory already exists: $outputAbsolute"
}
New-Item -ItemType Directory -Path $outputAbsolute -ErrorAction Stop | Out-Null

$profileHash = (Get-FileHash -LiteralPath $profileAbsolute -Algorithm SHA256).Hash.ToLowerInvariant()
$machineHash = (Get-FileHash -LiteralPath $machineAbsolute -Algorithm SHA256).Hash.ToLowerInvariant()
$gateEvidence = [System.Collections.Generic.List[object]]::new()
$adapterReports = [System.Collections.Generic.List[object]]::new()
$previousPolicy = [Environment]::GetEnvironmentVariable([string]$profile.policy_environment, "Process")
$previousCargoIncremental = [Environment]::GetEnvironmentVariable("CARGO_INCREMENTAL", "Process")
$buildStdoutPath = Join-Path $outputAbsolute "build.stdout.log"
$buildStderrPath = Join-Path $outputAbsolute "build.stderr.log"
$buildExitCode = $null

try {
    [Environment]::SetEnvironmentVariable(
        [string]$profile.policy_environment,
        [string]$profile.execution_policy,
        "Process"
    )
    [Environment]::SetEnvironmentVariable("CARGO_INCREMENTAL", "0", "Process")
    $buildExitCode = Invoke-BoundedCargoGate "build" `
        @("test", "-p", "mondrian-renderer", "--all-features", "--no-run") `
        ([int]$profile.build_timeout_seconds) $buildStdoutPath $buildStderrPath
    if ($buildExitCode -ne 0) {
        throw "GPU color qualification build failed with exit code $buildExitCode."
    }
    foreach ($gate in $profile.gates) {
        $gateId = [string]$gate.id
        $stdoutPath = Join-Path $outputAbsolute "$gateId.stdout.log"
        $stderrPath = Join-Path $outputAbsolute "$gateId.stderr.log"
        $arguments = [System.Collections.Generic.List[string]]::new()
        foreach ($argument in @("test", "-p", "mondrian-renderer", "--all-features")) {
            $arguments.Add($argument)
        }
        if ($gate.target -eq "lib") {
            $arguments.Add("--lib")
        } elseif ($gate.target -eq "integration") {
            if ([string]::IsNullOrWhiteSpace([string]$gate.integration_test)) {
                throw "Integration gate '$gateId' has no integration_test target."
            }
            $arguments.Add("--test")
            $arguments.Add([string]$gate.integration_test)
        } else {
            throw "Gate '$gateId' has unsupported target '$($gate.target)'."
        }
        $arguments.Add([string]$gate.test)
        $arguments.Add("--")
        $arguments.Add("--exact")
        $arguments.Add("--nocapture")
        if ($gate.ignored -eq $true) { $arguments.Add("--ignored") }

        $reportPath = $null
        $reportEnvironment = $null
        if ($null -ne $gate.PSObject.Properties["report"]) {
            $reportPath = Join-Path $outputAbsolute ([string]$gate.report.file)
            $reportEnvironment = [string]$gate.report.environment
            if (Test-Path -LiteralPath $reportPath) {
                throw "Gate '$gateId' report path unexpectedly exists before execution."
            }
            [Environment]::SetEnvironmentVariable($reportEnvironment, $reportPath, "Process")
        }

        try {
            $exitCode = Invoke-BoundedCargoGate $gateId $arguments.ToArray() `
                ([int]$gate.timeout_seconds) $stdoutPath $stderrPath
        } finally {
            if ($null -ne $reportEnvironment) {
                [Environment]::SetEnvironmentVariable($reportEnvironment, $null, "Process")
            }
        }
        $combinedOutput = (Get-Content -LiteralPath $stdoutPath -Raw) + "`n" + `
            (Get-Content -LiteralPath $stderrPath -Raw)
        if ($exitCode -ne 0) { throw "GPU color gate '$gateId' failed with exit code $exitCode." }
        if ($combinedOutput -notmatch 'test result: ok\. 1 passed; 0 failed;') {
            throw "GPU color gate '$gateId' did not prove execution of exactly one passing test."
        }
        if ($combinedOutput -match '(?i)skipping diagnostic GPU color gate|skipping diagnostic native YUV color gate') {
            throw "GPU color gate '$gateId' emitted a forbidden diagnostic skip."
        }

        $reportHash = $null
        if ($null -ne $reportPath) {
            if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
                throw "GPU color gate '$gateId' emitted no required report."
            }
            $reportLines = @(Get-Content -LiteralPath $reportPath | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            if ($reportLines.Count -ne 1) { throw "GPU color gate '$gateId' must emit exactly one report row." }
            $report = $reportLines[0] | ConvertFrom-Json
            if ($null -ne $report.PSObject.Properties["skipped"]) {
                $skipped = [string]$report.skipped
                if (-not [string]::IsNullOrWhiteSpace($skipped)) {
                    throw "GPU color gate '$gateId' emitted a skipped report: $skipped"
                }
            }
            if ((Read-NestedProperty $report ([string]$gate.report.schema_path)) -ne $gate.report.schema_version) {
                throw "GPU color gate '$gateId' report schema does not match the profile."
            }
            if ($report.scenario -ne $gate.report.scenario) {
                throw "GPU color gate '$gateId' report scenario does not match the profile."
            }
            if ($gateId -eq "output-smoke" -and (
                $report.health_report.verdict -ne "Pass" -or
                $report.health_report.summary.status -ne "passed" -or
                $report.health_report.summary.native_gpu_output_ready -ne $true
            )) {
                throw "GPU output smoke report is not a passing native GPU execution."
            }
            if ($null -eq $report.PSObject.Properties["adapter"] -or $null -eq $report.adapter) {
                throw "GPU color gate '$gateId' report has no adapter identity."
            }
            $adapterReports.Add($report.adapter)
            $reportHash = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }

        $gateEvidence.Add([ordered]@{
            id = $gateId
            test = [string]$gate.test
            ignored = [bool]$gate.ignored
            exit_code = $exitCode
            process_timed_out = $false
            stdout_sha256 = (Get-FileHash -LiteralPath $stdoutPath -Algorithm SHA256).Hash.ToLowerInvariant()
            stderr_sha256 = (Get-FileHash -LiteralPath $stderrPath -Algorithm SHA256).Hash.ToLowerInvariant()
            report_file = if ($null -eq $reportPath) { $null } else { [IO.Path]::GetFileName($reportPath) }
            report_sha256 = $reportHash
            passed = $true
        })
    }
} finally {
    [Environment]::SetEnvironmentVariable(
        [string]$profile.policy_environment,
        $previousPolicy,
        "Process"
    )
    [Environment]::SetEnvironmentVariable("CARGO_INCREMENTAL", $previousCargoIncremental, "Process")
}

if ($adapterReports.Count -lt 3) { throw "Qualification did not retain every required adapter-bearing report." }
$adapterFingerprints = @($adapterReports | ForEach-Object {
    "$($_.name)|$($_.backend)|$($_.device_type)|$($_.driver)|$($_.driver_info)"
} | Sort-Object -Unique)
if ($profile.required_adapter.same_adapter_across_reports_required -and $adapterFingerprints.Count -ne 1) {
    throw "GPU qualification reports were produced by different adapter identities."
}
$adapter = $adapterReports[0]
if ([string]::IsNullOrWhiteSpace([string]$adapter.name)) {
    throw "Observed wgpu adapter has no stable adapter name."
}
if ($adapter.backend -ne $profile.required_adapter.backend -or
    $adapter.device_type -ne $profile.required_adapter.device_type) {
    throw "Observed adapter backend/type does not match the sealed profile."
}
if ($profile.required_adapter.driver_identity_required -and
    [string]::IsNullOrWhiteSpace([string]$adapter.driver)) {
    throw "Observed wgpu adapter has no complete driver identity."
}
$adapterName = Normalize-AdapterName ([string]$adapter.name)
$machineGpuMatches = @($machine.gpus | Where-Object {
    $machineName = Normalize-AdapterName ([string]$_.name)
    -not [string]::IsNullOrWhiteSpace($machineName) -and
    ($machineName.Contains($adapterName) -or $adapterName.Contains($machineName)) -and
    -not [string]::IsNullOrWhiteSpace([string]$_.driver_version)
})
if ($profile.required_adapter.machine_inventory_name_match_required -and $machineGpuMatches.Count -ne 1) {
    throw "Observed wgpu adapter does not bind uniquely to the captured machine GPU/driver inventory."
}
if ($profile.required_adapter.driver_identity_required -and
    [string]$adapter.driver -ne [string]$machineGpuMatches[0].driver_version) {
    throw "Observed wgpu driver '$($adapter.driver)' does not match captured machine driver '$($machineGpuMatches[0].driver_version)'."
}

$report = [ordered]@{
    schema_version = 1
    profile = [ordered]@{ id = [string]$profile.id; sha256 = $profileHash }
    status = "passed"
    source_sha = $sourceSha
    machine = [ordered]@{
        id = $MachineId
        report_sha256 = $machineHash
        gpu_inventory_name = [string]$machineGpuMatches[0].name
        gpu_inventory_driver_version = [string]$machineGpuMatches[0].driver_version
    }
    adapter = $adapter
    execution_policy = [string]$profile.execution_policy
    build = [ordered]@{
        exit_code = $buildExitCode
        timed_out = $false
        stdout_sha256 = (Get-FileHash -LiteralPath $buildStdoutPath -Algorithm SHA256).Hash.ToLowerInvariant()
        stderr_sha256 = (Get-FileHash -LiteralPath $buildStderrPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    all_required_gates_ran = $gateEvidence.Count -eq @($profile.gates).Count
    skipped_gate_count = 0
    gates = @($gateEvidence)
    generated_at_utc = [DateTime]::UtcNow.ToString("o")
}
$qualificationPath = Join-Path $outputAbsolute "gpu-color-qualification.json"
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $qualificationPath -Encoding utf8
Write-Host "GPU color qualification: passed; report: $qualificationPath"
