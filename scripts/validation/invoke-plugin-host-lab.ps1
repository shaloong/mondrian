# Executes frozen libtest artifacts; it never builds or infers standard compatibility.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Manifest,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$manifestBytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Manifest).Path)
if ($manifestBytes.Length -gt 1048576) { throw 'Lab manifest exceeds one MiB.' }
$manifestHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($manifestBytes))
$configuration = [Text.Encoding]::UTF8.GetString($manifestBytes).TrimStart([char]0xfeff) | ConvertFrom-Json
if ($configuration.schemaVersion -ne 1) { throw 'Unsupported lab manifest schema.' }
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Lab evidence directory already exists.' }
if (-not $configuration.artifacts -or -not $configuration.groups -or -not $configuration.sources -or -not $configuration.runner) {
    throw 'Explicit frozen artifacts, source hashes and test groups are required.'
}
$groupIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($group in $configuration.groups) {
    if ($group.id -notmatch '^[a-z0-9][a-z0-9-]*$' -or -not $groupIds.Add($group.id) -or $group.expectedPassed -lt 1 -or $group.timeoutSeconds -lt 1 -or $group.timeoutSeconds -gt 86400) {
        throw 'Groups require unique safe identities, positive exact test counts and bounded timeouts.'
    }
}
$sourceRoot = (Resolve-Path -LiteralPath $configuration.sourceRoot).Path
function Confirm-Source {
    foreach ($source in $configuration.sources) {
        $path = [IO.Path]::GetFullPath((Join-Path $sourceRoot $source.path))
        if (-not $path.StartsWith($sourceRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Source manifest path escapes its root.'
        }
        if ((Get-FileHash -LiteralPath $path).Hash -ne $source.sha256) { throw "Source mismatch: $($source.path)" }
    }
}
function Stop-LabRunner($process, $settlement) {
    if ($settlement.Attempted -or $settlement.ParentExitObserved) { return }
    $settlement.Attempted = $true
    $cleanupTimer = [Diagnostics.Stopwatch]::StartNew()
    try {
        # Kill only the runner. Its existing kill-on-close Job owns test children;
        # this fallback does not infer tree settlement without the native receipt.
        if (-not $process.HasExited) { $process.Kill() }
    } catch { $settlement.Errors += $_.Exception.ToString() }
    try {
        $remaining = [math]::Max(0, 2000 - $cleanupTimer.ElapsedMilliseconds)
        $settlement.ParentExitObserved = $process.HasExited -or ($remaining -gt 0 -and $process.WaitForExit([int]$remaining))
    } catch { $settlement.Errors += $_.Exception.ToString() }
    $settlement.ElapsedMs = $cleanupTimer.ElapsedMilliseconds
}
Confirm-Source
$leases = [Collections.Generic.List[IDisposable]]::new()
$artifacts = @{}
$results = [Collections.Generic.List[object]]::new()
$passed = $false
try {
    foreach ($artifact in $configuration.artifacts) {
        if ($artifacts.ContainsKey($artifact.id)) { throw 'Duplicate artifact identity.' }
        if (-not [IO.Path]::IsPathFullyQualified($artifact.path)) { throw 'Artifact paths must be absolute.' }
        $lease = [IO.File]::Open($artifact.path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $leases.Add($lease)
        if ((Get-FileHash -LiteralPath $artifact.path).Hash -ne $artifact.sha256) { throw "Artifact mismatch: $($artifact.id)" }
        $artifacts[$artifact.id] = $artifact
    }
    New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $OutputDirectory 'manifest.json'), $manifestBytes)
    $runner = $artifacts[$configuration.runner]
    if (-not $runner) { throw 'Frozen native lab runner artifact is required.' }
    foreach ($group in $configuration.groups) {
        if ($group.id -notmatch '^[a-z0-9][a-z0-9-]*$' -or $group.expectedPassed -lt 1 -or $group.timeoutSeconds -lt 1) {
            throw 'Each group requires a safe identity, positive exact test count and timeout.'
        }
        $artifact = $artifacts[$group.artifact]
        if (-not $artifact) { throw "Unknown test artifact: $($group.artifact)" }
        $ownerPath = [IO.Path]::GetFullPath((Join-Path $OutputDirectory "$($group.id)-owner.json"))
        $stdoutPath = [IO.Path]::GetFullPath((Join-Path $OutputDirectory "$($group.id)-stdout.log"))
        $stderrPath = [IO.Path]::GetFullPath((Join-Path $OutputDirectory "$($group.id)-stderr.log"))
        $planPath = [IO.Path]::GetFullPath((Join-Path $OutputDirectory "$($group.id)-launch.json"))
        [ordered]@{ schema_version=1; executable=@{path=$artifact.path;sha256=$artifact.sha256.ToLowerInvariant()}; arguments=@($group.arguments); working_directory=$sourceRoot; stdout_path=$stdoutPath; stderr_path=$stderrPath; deadline_ms=([long]$group.timeoutSeconds*1000); report_path=$ownerPath } |
            ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $planPath -Encoding utf8NoBOM
        $start = [Diagnostics.ProcessStartInfo]::new($runner.path)
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.WorkingDirectory = $sourceRoot
        $start.ArgumentList.Add($planPath)
        if ($configuration.environment) {
            foreach ($property in $configuration.environment.PSObject.Properties) { $start.Environment[$property.Name] = [string]$property.Value }
        }
        $process = [Diagnostics.Process]::new()
        $process.StartInfo = $start
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $started = $false
        $settlement = [ordered]@{ Attempted=$false; ParentExitObserved=$false; WaitBudgetMs=2000; ElapsedMs=0; Errors=@(); DescendantSettlement='RequiresNativeOwnerReceipt' }
        $resultCount = $results.Count
        try {
            if (-not $process.Start()) { throw 'Lab process did not start.' }
            $started = $true
            # Native Job accounting owns the actual test tree. The outer limit
            # allows its separate two-second reap budget and report publication.
            $outerTimedOut = -not $process.WaitForExit([int]($group.timeoutSeconds * 1000 + 10000))
            $settlement.ParentExitObserved = -not $outerTimedOut
            if ($outerTimedOut) { Stop-LabRunner $process $settlement }
            $runnerExitCode = if ($process.HasExited) { $process.ExitCode } else { $null }
            $owner = if (Test-Path -LiteralPath $ownerPath) { Get-Content -LiteralPath $ownerPath -Raw | ConvertFrom-Json } else { $null }
            $matches = @()
            if (Test-Path -LiteralPath $stdoutPath) {
                # Read line by line; large native diagnostics do not become one
                # unbounded in-memory string in the qualification controller.
                foreach ($line in [IO.File]::ReadLines($stdoutPath)) {
                    $summary = [regex]::Match($line, '^test result: ok\. (\d+) passed; 0 failed; 0 ignored;')
                    if ($summary.Success) { $matches += [int]$summary.Groups[1].Value }
                }
            }
            $accepted = -not $outerTimedOut -and $runnerExitCode -eq 0 -and $owner -and $owner.exit_code -eq 0 -and -not $owner.deadline_exceeded -and -not $owner.descendants_outlived_parent -and $owner.descendants_reaped -and $owner.errors.Count -eq 0 -and $settlement.Errors.Count -eq 0 -and $matches.Count -eq 1 -and $matches[0] -eq $group.expectedPassed
            $results.Add([ordered]@{ Id=$group.id; Accepted=[bool]$accepted; RunnerExitCode=$runnerExitCode; OuterTimedOut=$outerTimedOut; RunnerSettlement=$settlement; NativeOwner=$owner; ElapsedMs=$timer.ElapsedMilliseconds; StdoutSHA256=if(Test-Path -LiteralPath $stdoutPath){(Get-FileHash -LiteralPath $stdoutPath).Hash}else{$null}; StderrSHA256=if(Test-Path -LiteralPath $stderrPath){(Get-FileHash -LiteralPath $stderrPath).Hash}else{$null} })
            if (-not $accepted) { throw "Lab group failed its exact gate: $($group.id)" }
        } catch {
            if ($results.Count -eq $resultCount) {
                $results.Add([ordered]@{ Id=$group.id; Accepted=$false; Failure=$_.Exception.ToString(); RunnerSettlement=$settlement })
            }
            throw
        } finally {
            if ($started) { Stop-LabRunner $process $settlement }
            $process.Dispose()
        }
    }
    Confirm-Source
    $passed = $true
} finally {
    if (Test-Path -LiteralPath $OutputDirectory) {
        [ordered]@{ SchemaVersion=1; SelectedGatesPassed=$passed; FullStandardQualified=$false; FullPerformanceQualified=$false; ManifestSHA256=$manifestHash; SourceCount=$configuration.sources.Count; Groups=$results } |
            ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'receipt.json') -Encoding utf8
    }
    foreach ($lease in $leases) { $lease.Dispose() }
}
