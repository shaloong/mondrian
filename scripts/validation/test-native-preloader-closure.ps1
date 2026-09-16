Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'native-preloader-closure.psm1') -Force
$sha = 'a' * 64
$machine = [pscustomobject]@{ verifier_tools = [pscustomobject]@{ preloader = [pscustomobject]@{ path = 'C:\approval\launcher.exe'; sha256 = $sha }; runtime_files = @([pscustomobject]@{ path = 'C:\approval\avcodec-62.dll'; sha256 = $sha }) } }
$image = { param($source, $staged, $index) [pscustomobject]@{ source = [pscustomobject]@{ path = $source; sha256 = $sha }; staged_path = $staged; object = [pscustomobject]@{ volume_serial = 10; file_index = $index; length = 256 } } }
$report = [pscustomobject]@{
    schema_version = 1; exit_code = 0; deadline_exceeded = $false; capsule_removed = $true; descendants_reaped = $true; errors = @();
    child_manifest = [pscustomobject]@{ path = 'C:\evidence\manifest.json'; sha256 = $sha };
    attestation = [pscustomobject]@{
        schema_version = 1; launcher_pid = 100; child_pid = 101; launcher_sha256 = $sha; request_sha256 = $sha; machine_plan_sha256 = $sha;
        challenge = '12345678-1234-4234-8234-123456789abc';
        owned_images = @((& $image 'C:\approval\app.exe' 'C:\capsule\app.exe' 1), (& $image 'C:\approval\avcodec-62.dll' 'C:\capsule\avcodec-62.dll' 2));
        mapped_image_paths = @('C:\capsule\app.exe','C:\capsule\avcodec-62.dll')
    }
}
function Verify($value) { Assert-NativePreloaderReport $value 'C:\evidence\manifest.json' $sha $machine $sha $sha }
Verify $report
$mutations = @(
    { param($v) $v.descendants_reaped = $false },
    { param($v) $v.capsule_removed = $false },
    { param($v) $v.errors = @('native kill failed') },
    { param($v) $v.exit_code = '0' },
    { param($v) $v.attestation.child_pid = '101' },
    { param($v) $v.attestation.mapped_image_paths = @('C:\capsule\app.exe') },
    { param($v) $v.attestation.mapped_image_paths += 'C:\poison\evil.dll' },
    { param($v) $v.attestation.owned_images[1].object.file_index = 1 },
    { param($v) $v.attestation.owned_images[1].object.length = '256' },
    { param($v) $v.child_manifest.sha256 = 'b' * 64 },
    { param($v) $v.PSObject.Properties.Remove('attestation') },
    { param($v) $v.PSObject.Properties.Remove('descendants_reaped') }
)
foreach ($mutation in $mutations) {
    $copy = $report | ConvertTo-Json -Depth 15 | ConvertFrom-Json
    & $mutation $copy
    $rejected = $false
    try { Verify $copy } catch { $rejected = $true }
    if (-not $rejected) { throw 'Mutated native pre-loader evidence was accepted.' }
}
Write-Output "Native pre-loader verifier: 1 valid receipt and $($mutations.Count) adversarial rejections passed."
