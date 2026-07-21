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

Export-ModuleMember -Function Invoke-BoundedPlaybackGateProcess
