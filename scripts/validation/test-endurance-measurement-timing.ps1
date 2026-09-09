param([Parameter(Mandatory=$true)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$source=Join-Path $PSScriptRoot 'verify-commercial-endurance-qualification.ps1'
$tokens=$null; $errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($source,[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw 'Verifier syntax failed' }
foreach ($function in $ast.FindAll({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst]},$false)) {
    . ([scriptblock]::Create($function.Extent.Text))
}
Import-Module (Join-Path $PSScriptRoot 'phase-owner-closure.psm1') -Force
$out=[IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $out) { throw 'Create-only output already exists' }
[void][IO.Directory]::CreateDirectory($out)
$template='{"measurement_timing":{"startup_started_at_run_us":1000000,"startup_deadline_at_run_us":121000000,"owners_ready_at_run_us":9000000,"measurement_started_at_run_us":10000000,"measurement_deadline_at_run_us":86410000000}}'
$phase=[pscustomobject]@{started_at_run_us=10000000L;completed_at_run_us=86410000000L}
$requirement=[pscustomobject]@{minimum_duration_us=86400000000L}
$timeouts=[pscustomobject]@{startup_ms=120000L}
$baseline=Get-EnduranceMeasurementProjection ($template | ConvertFrom-Json) $phase $requirement $timeouts
if ((Get-EnduranceMeasurementProjection ($template | ConvertFrom-Json) $phase $requirement ([pscustomobject]@{})) -cne $baseline) { throw 'Independent legacy startup default differs' }
$script:rejected=0
function Reject([scriptblock]$Mutation,[string]$Label) {
    $candidate=$template | ConvertFrom-Json
    & $Mutation $candidate
    $caught=$false
    try { $null=Get-EnduranceMeasurementProjection $candidate $phase $requirement $timeouts } catch { $caught=$true }
    if (-not $caught) { throw "Accepted invalid timing: $Label" }
    $script:rejected++
}
Reject {param($x) $x.PSObject.Properties.Remove('measurement_timing')} 'missing all timing'
Reject {param($x) $x.measurement_timing=$null} 'null timing'
foreach ($field in @('startup_started_at_run_us','startup_deadline_at_run_us','owners_ready_at_run_us','measurement_started_at_run_us','measurement_deadline_at_run_us')) {
    Reject {param($x) $x.measurement_timing.PSObject.Properties.Remove($field)} "missing $field"
    foreach ($bad in @(-1,1.5,'1000000',$true)) {
        Reject {param($x) $x.measurement_timing.$field=$bad} "wrong scalar $field"
    }
}
Reject {param($x) $x.measurement_timing | Add-Member extra 0} 'unknown timing field'
Reject {param($x) $x.measurement_timing.owners_ready_at_run_us=999999L} 'ready before startup'
Reject {param($x) $x.measurement_timing.owners_ready_at_run_us=10000001L} 'ready after measurement'
Reject {param($x) $x.measurement_timing.startup_deadline_at_run_us=10000000L} 'expired startup'
Reject {param($x) $x.measurement_timing.startup_deadline_at_run_us++} 'renewed startup deadline'
Reject {param($x) $x.measurement_timing.measurement_deadline_at_run_us--} 'short measurement window'
Reject {param($x) $x.measurement_timing.measurement_started_at_run_us++} 'baseline moved'
Reject {param($x) $x.measurement_timing.measurement_deadline_at_run_us=[bigint]::Parse('18446744073709551616')} 'u64 overflow'
$phase.completed_at_run_us--
Reject {param($x)} 'phase ended before full window'
$phase.completed_at_run_us++
$record=[ordered]@{schema_version=1;qualified=$false;positive_startup_duration_us=9000000;measurement_duration_us=86400000000L;rejected_mutations=$script:rejected;verifier_sha256=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()}
$record | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $out 'result.json') -Encoding utf8
$record | ConvertTo-Json -Compress
