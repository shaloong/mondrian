# Fast independent replay regressions. No Cargo, GPU, App or physical device starts.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'window-owner-closure.psm1') -Force
$fixturePath = Join-Path $PSScriptRoot '../../tests/validation/fixtures/window-owner-closure.json'
$fixtureJson = Get-Content -LiteralPath $fixturePath -Raw
$script:caseCount = 0

function Assert-Rejected($Fixture, [string]$Description) {
    $rejected = $false
    try { Assert-MondrianWindowOwnerClosure $Fixture.runtime_shutdown $Fixture.host_shutdown }
    catch { $rejected = $true }
    if (-not $rejected) { throw "Accepted incomplete owner evidence: $Description" }
    $script:caseCount++
}

function Get-ObjectPaths($Value, [string]$Path, $Paths) {
    if ($Value -is [pscustomobject]) {
        $Paths.Add($Path)
        foreach ($property in $Value.PSObject.Properties) {
            Get-ObjectPaths $property.Value "$Path/$($property.Name)" $Paths
        }
    }
}

function Get-AtPath($Value, [string]$Path) {
    $current = $Value
    foreach ($segment in $Path.Split('/', [StringSplitOptions]::RemoveEmptyEntries)) { $current = $current.$segment }
    return $current
}

$original = $fixtureJson | ConvertFrom-Json
Assert-MondrianWindowOwnerClosure $original.runtime_shutdown $original.host_shutdown
foreach ($owner in @('runtime_shutdown', 'host_shutdown')) {
    $paths = [Collections.Generic.List[string]]::new()
    Get-ObjectPaths $original.$owner "/$owner" $paths
    foreach ($path in $paths) {
        $object = Get-AtPath $original $path
        foreach ($field in @($object.PSObject.Properties.Name)) {
            foreach ($mutation in @('remove', 'wrong-type', 'extra', 'case')) {
                $changed = $fixtureJson | ConvertFrom-Json
                $target = Get-AtPath $changed $path
                switch ($mutation) {
                    'remove' { $target.PSObject.Properties.Remove($field) }
                    'wrong-type' { $target.$field = @() }
                    'extra' { $target | Add-Member -NotePropertyName unexpected_owner -NotePropertyValue $false }
                    'case' {
                        $old = $target.$field
                        $target.PSObject.Properties.Remove($field)
                        $target | Add-Member -NotePropertyName $field.ToUpperInvariant() -NotePropertyValue $old
                    }
                }
                Assert-Rejected $changed "$path/$field $mutation"
            }
        }
    }
}
foreach ($mutation in @(
    @('/runtime_shutdown/supervisor', 'not_started'),
    @('/runtime_shutdown/supervisor', 'timed_out_detached'),
    @('/runtime_shutdown/runtime_handoff_completed', $false),
    @('/runtime_shutdown/shutdown_signal_delivered', $false),
    @('/runtime_shutdown/configured_worker_threads', 0),
    @('/host_shutdown/preview/worker_panics', 1),
    @('/host_shutdown/preview/workers_terminated', 0),
    @('/host_shutdown/preview/workers_started', [uint64]4294967296),
    @('/host_shutdown/preview/timeline_render_cache/worker/worker_started', $false),
    @('/host_shutdown/preview/timeline_render_cache/aggregate_outcome', 'not_started'),
    @('/host_shutdown/preview/work_callbacks/registrations_retained', 1),
    @('/host_shutdown/auxiliary/waveform/external_source_cache_references', 1),
    @('/host_shutdown/auxiliary/waveform/source_cache/decoder_sessions_remaining', 1),
    @('/host_shutdown/auxiliary/waveform/source_cache/decoder_startup/producers_remaining', 1),
    @('/host_shutdown/auxiliary/thumbnails/Ok/active_requests_remaining', 1),
    @('/host_shutdown/auxiliary/catalog/results_missing', 1),
    @('/host_shutdown/auxiliary/catalog/startup_attempts', [uint64]::MaxValue)
)) {
    $changed = $fixtureJson | ConvertFrom-Json
    $split = $mutation[0].LastIndexOf('/')
    $target = Get-AtPath $changed $mutation[0].Substring(0, $split)
    $field = $mutation[0].Substring($split + 1)
    $target.$field = $mutation[1]
    Assert-Rejected $changed $mutation[0]
}
$gpuWake = [pscustomobject]@{
    worker_shutdown = 'terminated'
    wake_callbacks = $original.host_shutdown.preview.work_callbacks
    native_wake_failures = 0
    wake_registration_rejections = 0
}
Assert-MondrianGpuWakeClosure $gpuWake $true
$unregistered = $gpuWake | ConvertTo-Json -Depth 20 -Compress | ConvertFrom-Json
$unregistered.wake_callbacks.registrations_accepted = 0
$unregistered.wake_callbacks.registrations_released = 0
$unregistered.wake_callbacks.worker_started = $false
$unregistered.wake_callbacks.worker = 'not_started'
# An unregistered headless owner closes cleanly but cannot prove native Window wake admission.
Assert-MondrianGpuWakeClosure $unregistered
$rejected = $false
try { Assert-MondrianGpuWakeClosure $unregistered $true } catch { $rejected = $true }
if (-not $rejected) { throw 'Accepted a Window GPU without native wake registration' }
$script:caseCount++
foreach ($mutation in @(
    @('/worker_shutdown', 'timed_out_detached'),
    @('/worker_shutdown', 'panicked_payload_abandoned'),
    @('/native_wake_failures', 1),
    @('/wake_registration_rejections', 1),
    @('/native_wake_failures', '0'),
    @('/wake_callbacks/registrations_retained', 1),
    @('/wake_callbacks/invocation_panics', 1),
    @('/wake_callbacks/opaque_payloads_abandoned', 1),
    @('/wake_callbacks/deadline_met', $false),
    @('/wake_callbacks/worker', 'timed_out_detached'),
    @('/wake_callbacks/registrations_accepted', [bigint]::Parse('18446744073709551616'))
)) {
    $changed = $gpuWake | ConvertTo-Json -Depth 20 -Compress | ConvertFrom-Json
    $split = $mutation[0].LastIndexOf('/')
    $target = Get-AtPath $changed $mutation[0].Substring(0, $split)
    $field = $mutation[0].Substring($split + 1)
    $target.$field = $mutation[1]
    $rejected = $false
    try { Assert-MondrianGpuWakeClosure $changed } catch { $rejected = $true }
    if (-not $rejected) { throw "Accepted dirty GPU wake evidence: $($mutation[0])" }
    $script:caseCount++
}
Write-Output "WINDOW_OWNER_REPLAY: baseline passed, $script:caseCount adversarial mutations rejected"
