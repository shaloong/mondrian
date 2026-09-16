param([Parameter(Mandatory = $true)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot 'verify-commercial-endurance-qualification.ps1'
$parseErrors = $null
$tokens = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($source, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw 'Verifier syntax failed.' }
foreach ($function in $ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($function.Extent.Text))
}
Import-Module (Join-Path $PSScriptRoot 'window-owner-closure.psm1') -Force
$script:ObservedJsonHashes = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
$script:ObservedAncillaryHashes = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw 'Create-only test output already exists.' }
$null = [IO.Directory]::CreateDirectory($output)
$script:rejections = 0
function Expect-Rejected([scriptblock]$Action, [string]$Label) {
    $rejected = $false
    try { & $Action } catch { $rejected = $true }
    if (-not $rejected) { throw "Accepted adversarial case: $Label" }
    $script:rejections++
}
$sha = 'a' * 64
$plain = [pscustomobject]@{schema_version=1}
$bound = [pscustomobject]@{ancillary_program_sha256=$sha}
Assert-EnduranceAncillaryBinding $plain '' 'legacy baseline'
Assert-EnduranceAncillaryBinding $bound $sha 'bound baseline'
Expect-Rejected { Assert-EnduranceAncillaryBinding $bound '' 'invented' } 'invented program'
Expect-Rejected { Assert-EnduranceAncillaryBinding $plain $sha 'missing' } 'missing program'
Expect-Rejected { Assert-EnduranceAncillaryBinding $bound ('b' * 64) 'different' } 'wrong program'
Expect-Rejected { Assert-EnduranceAncillaryBinding ([pscustomobject]@{ancillary_program_sha256=$null}) $sha 'null' } 'null digest'
foreach ($json in @('{"a":1,"a":2}', '{"a":1,"A":2}', '{"frames":[{"id":1,"id":2}]}', '{"a":NaN}', '{"a":1,}')) {
    Expect-Rejected { $null = ConvertFrom-EnduranceStrictJson $json } 'ambiguous JSON'
}
$timestamp = ConvertFrom-EnduranceStrictJson '{"created_at":"2026-09-06T00:00:00Z","frames":[0,1]}'
if ($timestamp.created_at -isnot [string] -or $timestamp.frames.Count -ne 2) { throw 'Strict reader changed evidence types.' }
$path = Join-Path $output 'program.json'
[IO.File]::WriteAllText($path, '{"frame_count":2}', [Text.UTF8Encoding]::new($false))
$null = Read-BoundedJson $path 'bounded baseline' 64
if ($script:ObservedJsonHashes[$path] -cne (Get-LowerSha256 $path)) { throw 'Parsed-byte hash differs.' }
Expect-Rejected { $null = Read-BoundedJson $path 'oversize' 4 } 'oversize program'
$artifact = Join-Path $output 'fixture.mxf'
[IO.File]::WriteAllText($artifact, 'structural-test-only; no encoded-media qualification')
$artifactSha = Get-LowerSha256 $artifact
$artifactId = 'endurance-export-11111111-1111-4111-8111-111111111111'
$event = [pscustomobject]@{kind='export_artifact_verified';artifact_id=$artifactId;artifact_sha256=$artifactSha;validation_report_sha256=('b' * 64)}
$sidecarPath = $artifact + '.independent-verification.json'
$sidecar = [pscustomobject]@{schema_version=1;status='verified';request=@{artifact_id=$artifactId;path=$artifact;ancillary_program_sha256=$sha};ancillary_mxf_rescan=@{frames_verified=2;ancillary_program_sha256=$sha};evidence=@{validation_report_sha256=$event.validation_report_sha256;report=@{artifact_id=$artifactId;artifact_sha256=$artifactSha}}}
function Write-Sidecar($Value) { [IO.File]::WriteAllText($sidecarPath, ($Value | ConvertTo-Json -Depth 30), [Text.UTF8Encoding]::new($false)) }
Write-Sidecar $sidecar
$raw = [pscustomobject]@{ancillary_program_sha256=$sha;ancillary_export_artifacts=@([pscustomobject]@{artifact_id=$artifactId;verification_path=$sidecarPath;verification_sha256=(Get-LowerSha256 $sidecarPath)});wire_journals=@()}
$plan = [pscustomobject]@{exports=@([pscustomobject]@{phase_id='fixture-phase';output_directory=$output})}
$program = [pscustomobject]@{frame_count=2}
$paths = @(Assert-EnduranceAncillaryFiles $raw @($event) $plan $program 'fixture-phase' 'continuous_export')
if ($paths.Count -ne 2) { throw 'Structural artifact baseline did not retain both files.' }
Expect-Rejected { $null = Assert-EnduranceAncillaryFiles $raw @() $plan $program 'fixture-phase' 'continuous_export' } 'orphan sidecar'
Expect-Rejected { $null = Assert-EnduranceAncillaryFiles $raw @($event,$event) $plan $program 'fixture-phase' 'continuous_export' } 'repeated event'
foreach ($mutation in @(
    { param($value) $value.request.ancillary_program_sha256 = 'c' * 64 },
    { param($value) $value.ancillary_mxf_rescan.ancillary_program_sha256 = 'c' * 64 },
    { param($value) $value.ancillary_mxf_rescan.frames_verified = 1 },
    { param($value) $value.ancillary_mxf_rescan.frames_verified = 2.5 },
    { param($value) $value.status = 'failed' },
    { param($value) $value.evidence.validation_report_sha256 = 'c' * 64 },
    { param($value) $value.evidence.report.artifact_sha256 = 'c' * 64 },
    { param($value) $value.request.artifact_id = 'wrong-artifact' }
)) {
    $changed = $sidecar | ConvertTo-Json -Depth 30 | ConvertFrom-Json
    & $mutation $changed
    Write-Sidecar $changed
    $raw.ancillary_export_artifacts[0].verification_sha256 = Get-LowerSha256 $sidecarPath
    Expect-Rejected { $null = Assert-EnduranceAncillaryFiles $raw @($event) $plan $program 'fixture-phase' 'continuous_export' } 'fully rehashed sidecar mutation'
}
Write-Sidecar $sidecar
$raw.ancillary_export_artifacts[0].verification_sha256 = Get-LowerSha256 $sidecarPath
[IO.File]::AppendAllText($artifact, 'changed final artifact')
Expect-Rejected { $null = Assert-EnduranceAncillaryFiles $raw @($event) $plan $program 'fixture-phase' 'continuous_export' } 'changed final MXF'
$wirePath = Join-Path $output 'wire.jsonl'
[IO.File]::WriteAllText($wirePath, ('{"schema_version":1,"event":"session_identity","phase_id":"fixture-phase","ancillary_program_sha256":"' + $sha + '"}' + "`n"))
$identity = Read-EnduranceWireIdentity $wirePath 65536
Assert-EnduranceAncillaryBinding $identity $sha 'wire identity baseline'
$identity | Add-Member output_device 'output-card'
$identity | Add-Member output_generation 1
$identity | Add-Member receiver_device 'input-card'
$identity | Add-Member receiver_generation 2
[IO.File]::WriteAllText($wirePath, ($identity | ConvertTo-Json -Compress) + "`n")
$wireRaw = [pscustomobject]@{ancillary_program_sha256=$sha;ancillary_export_artifacts=@();wire_journals=@([pscustomobject]@{path=$wirePath;sha256=(Get-LowerSha256 $wirePath)})}
$plan | Add-Member reference_output ([pscustomobject]@{device_id='output-card';device_generation=1;wire_readback=@{receipt_directory=$output;maximum_receipt_bytes=65536;device_id='input-card';device_generation=2}})
$wirePaths = @(Assert-EnduranceAncillaryFiles $wireRaw @() $plan $program 'fixture-phase' 'playback_reference')
if ($wirePaths.Count -ne 1) { throw 'Structural wire baseline omitted its journal.' }
foreach ($mutation in @(
    { param($value) $value.phase_id = 'other-phase' },
    { param($value) $value.ancillary_program_sha256 = 'c' * 64 },
    { param($value) $value.receiver_device = 'another-card' },
    { param($value) $value.output_generation = 9 }
)) {
    $changed = $identity | ConvertTo-Json -Compress | ConvertFrom-Json
    & $mutation $changed
    [IO.File]::WriteAllText($wirePath, ($changed | ConvertTo-Json -Compress) + "`n")
    $wireRaw.wire_journals[0].sha256 = Get-LowerSha256 $wirePath
    Expect-Rejected { $null = Assert-EnduranceAncillaryFiles $wireRaw @() $plan $program 'fixture-phase' 'playback_reference' } 'fully rehashed wire identity'
}
Expect-Rejected { $null = Read-EnduranceWireIdentity $wirePath 4 } 'oversize wire journal'
[IO.File]::WriteAllText($wirePath, 'x' * 65537)
Expect-Rejected { $null = Read-EnduranceWireIdentity $wirePath 100000 } 'unbounded wire identity line'
[IO.File]::WriteAllText($wirePath, '{"a":1,"a":2}' + "`n")
Expect-Rejected { $null = Read-EnduranceWireIdentity $wirePath 100000 } 'duplicate wire identity fields'
$result = [ordered]@{schema_version=1;scope='structural-verifier-regression';qualified=$false;rejected=$script:rejections;verifier_sha256=(Get-LowerSha256 $source)}
$result | ConvertTo-Json | Set-Content (Join-Path $output 'result.json')
$result | ConvertTo-Json -Compress
