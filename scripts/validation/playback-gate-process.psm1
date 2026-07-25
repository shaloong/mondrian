Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-BoundedPlaybackGateProcess {
    <#
    .SYNOPSIS
    Runs one gate process under an external wall-clock deadline.

    .DESCRIPTION
    Standard output and error are drained asynchronously so child pipe
    backpressure cannot deadlock the supervisor. A timeout terminates the
    complete descendant process tree before the function returns. The caller
    receives structured timing/termination evidence and one durable log.
    #>
    param(
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][ValidateRange(1, [int]::MaxValue)][int]$TimeoutSeconds,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$LogPath
    )

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { [void]$startInfo.ArgumentList.Add($argument) }

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    try {
        if (-not $process.Start()) { throw "Failed to start process: $FilePath" }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $timeoutMilliseconds = [Math]::Min([int]::MaxValue, [int64]$TimeoutSeconds * 1000)
        $exited = $process.WaitForExit([int]$timeoutMilliseconds)
        $timedOut = -not $exited
        if ($timedOut) {
            try {
                $process.Kill($true)
            } catch {
                if (-not $process.HasExited) { $process.Kill() }
            }
            $process.WaitForExit()
        }
        $stdout = $stdoutTask.Result
        $stderr = $stderrTask.Result
        $exitCode = if ($timedOut) { -1 } else { $process.ExitCode }
    } finally {
        $stopwatch.Stop()
        $process.Dispose()
    }

    $log = if ([string]::IsNullOrEmpty($stderr)) {
        $stdout
    } elseif ([string]::IsNullOrEmpty($stdout)) {
        $stderr
    } else {
        "$stdout`r`n--- STDERR ---`r`n$stderr"
    }
    [IO.File]::WriteAllText($LogPath, $log, [Text.UTF8Encoding]::new($false))
    if (-not [string]::IsNullOrEmpty($stdout)) { Write-Host $stdout -NoNewline }
    if (-not [string]::IsNullOrEmpty($stderr)) { Write-Host $stderr -NoNewline }
    return [pscustomobject]@{
        exit_code = $exitCode
        timed_out = $timedOut
        elapsed_ms = [int64]$stopwatch.ElapsedMilliseconds
    }
}

function Invoke-TerminalReportGateProcess {
    <#
    .SYNOPSIS
    Runs one gate until it exits or emits a terminal structured report.

    .DESCRIPTION
    Heavy GPU processes can finish their product work yet remain blocked in
    third-party DLL process-detach hooks. A complete `pass` or `fail` report is
    therefore the semantic terminal signal. The supervisor grants a short
    natural-exit window, then terminates the descendant process tree without
    changing the report result. Missing or malformed reports never count as
    terminal and remain subject to the full wall-clock deadline.
    #>
    param(
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][ValidateRange(1, [int]::MaxValue)][int]$TimeoutSeconds,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$LogPath,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ReportPath,
        [ValidateRange(0, 60)][int]$NaturalExitGraceSeconds = 5
    )

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { [void]$startInfo.ArgumentList.Add($argument) }

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    $terminalReportObserved = $false
    $forcedAfterReport = $false
    $timedOut = $false
    try {
        if (-not $process.Start()) { throw "Failed to start process: $FilePath" }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
            if (Test-Path -LiteralPath $ReportPath -PathType Leaf) {
                try {
                    $candidate = Get-Content -LiteralPath $ReportPath -Raw | ConvertFrom-Json
                    $terminalReportObserved = (
                        $candidate.status -in @("pass", "fail") -and
                        $candidate.complete_golden_project -is [bool]
                    )
                } catch {
                    $terminalReportObserved = $false
                }
                if ($terminalReportObserved) { break }
            }
            Start-Sleep -Milliseconds 100
        }

        if (-not $terminalReportObserved -and (Test-Path -LiteralPath $ReportPath -PathType Leaf)) {
            try {
                $candidate = Get-Content -LiteralPath $ReportPath -Raw | ConvertFrom-Json
                $terminalReportObserved = (
                    $candidate.status -in @("pass", "fail") -and
                    $candidate.complete_golden_project -is [bool]
                )
            } catch {
                $terminalReportObserved = $false
            }
        }

        if ($terminalReportObserved -and -not $process.HasExited) {
            $graceMilliseconds = [Math]::Min(
                [int]::MaxValue,
                [int64]$NaturalExitGraceSeconds * 1000
            )
            if (-not $process.WaitForExit([int]$graceMilliseconds)) {
                $forcedAfterReport = $true
                try {
                    $process.Kill($true)
                } catch {
                    if (-not $process.HasExited) { $process.Kill() }
                }
                $process.WaitForExit()
            }
        } elseif (-not $process.HasExited) {
            $timedOut = $true
            try {
                $process.Kill($true)
            } catch {
                if (-not $process.HasExited) { $process.Kill() }
            }
            $process.WaitForExit()
        }

        $stdout = $stdoutTask.Result
        $stderr = $stderrTask.Result
        $exitCode = $process.ExitCode
    } finally {
        $stopwatch.Stop()
        $process.Dispose()
    }

    $log = if ([string]::IsNullOrEmpty($stderr)) {
        $stdout
    } elseif ([string]::IsNullOrEmpty($stdout)) {
        $stderr
    } else {
        "$stdout`r`n--- STDERR ---`r`n$stderr"
    }
    [IO.File]::WriteAllText($LogPath, $log, [Text.UTF8Encoding]::new($false))
    if (-not [string]::IsNullOrEmpty($stdout)) { Write-Host $stdout -NoNewline }
    if (-not [string]::IsNullOrEmpty($stderr)) { Write-Host $stderr -NoNewline }
    return [pscustomobject]@{
        exit_code = $exitCode
        timed_out = $timedOut
        terminal_report_observed = $terminalReportObserved
        forced_after_report = $forcedAfterReport
        elapsed_ms = [int64]$stopwatch.ElapsedMilliseconds
    }
}

Export-ModuleMember -Function @(
    "Invoke-BoundedPlaybackGateProcess",
    "Invoke-TerminalReportGateProcess"
)
