param(
    [Parameter(Mandatory = $true)][string]$LauncherPath,
    [Parameter(Mandatory = $true)][string]$ExpectedLauncherSha256,
    [Parameter(Mandatory = $true)][string]$LaunchPlanPath,
    [Parameter(Mandatory = $true)][string]$ExpectedLaunchPlanSha256
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$launcher = [IO.FileStream]::new([IO.Path]::GetFullPath($LauncherPath), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
$plan = $null
$process = $null
try {
    $plan = [IO.FileStream]::new([IO.Path]::GetFullPath($LaunchPlanPath), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    foreach ($check in @(@($launcher, $ExpectedLauncherSha256), @($plan, $ExpectedLaunchPlanSha256))) {
        if ($check[1] -cnotmatch '^[0-9a-f]{64}$') { throw 'Expected digest must be lowercase SHA-256.' }
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try { $actual = [Convert]::ToHexString($algorithm.ComputeHash($check[0])).ToLowerInvariant() }
        finally { $algorithm.Dispose() }
        if ($actual -cne $check[1]) { throw 'Exact launcher/launch-plan bytes differ from external approval.' }
    }
    $plan.Position = 0
    if ($plan.Length -le 0 -or $plan.Length -gt 1048576) { throw 'Launch plan exceeds one MiB.' }
    $reader = [IO.StreamReader]::new($plan, [Text.UTF8Encoding]::new($false, $true), $false, 4096, $true)
    try { $contract = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
    if ($contract.launcher.sha256 -cne $ExpectedLauncherSha256 -or
        [IO.Path]::GetFullPath($contract.launcher.path) -cne [IO.Path]::GetFullPath($LauncherPath)) {
        throw 'Launch plan does not bind this approved launcher.'
    }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = [IO.Path]::GetFullPath($LauncherPath)
    $start.ArgumentList.Add([IO.Path]::GetFullPath($LaunchPlanPath))
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($start)
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Native launcher failed ($($process.ExitCode)); inspect its raw create-only report." }
    Write-Output "MONDRIAN_NATIVE_PRELOADER_REPORT=$($contract.report_path)"
    Write-Output "MONDRIAN_NATIVE_PRELOADER_REPORT_SHA256=$((Get-FileHash -LiteralPath $contract.report_path -Algorithm SHA256).Hash.ToLowerInvariant())"
}
finally {
    if ($null -ne $process) { $process.Dispose() }
    if ($null -ne $plan) { $plan.Dispose() }
    $launcher.Dispose()
}
