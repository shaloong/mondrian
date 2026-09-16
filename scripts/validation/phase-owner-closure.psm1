Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'window-owner-closure.psm1')
Import-Module (Join-Path $PSScriptRoot '../perf/perf-owner-closure.psm1')

function Assert-PhaseInteger($Value, [string]$Context) {
    if ($Value -isnot [int] -and $Value -isnot [long] -and $Value -isnot [uint64] -and $Value -isnot [bigint]) { throw "$Context must be an integer" }
    if ([bigint]$Value -lt 0 -or [bigint]$Value -gt [bigint][uint64]::MaxValue) { throw "$Context is outside the u64 owner domain" }
}
function Assert-PhaseMeasurementTiming($Timing) {
    $fields=@('startup_started_at_run_us','startup_deadline_at_run_us','owners_ready_at_run_us','measurement_started_at_run_us','measurement_deadline_at_run_us')
    Assert-ExactReceiptFields $Timing $fields 'Phase measurement timing'
    foreach ($field in $fields) { Assert-PhaseInteger $Timing.$field "Measurement $field" }
    if ([bigint]$Timing.startup_started_at_run_us -gt [bigint]$Timing.owners_ready_at_run_us -or
        [bigint]$Timing.owners_ready_at_run_us -gt [bigint]$Timing.measurement_started_at_run_us -or
        [bigint]$Timing.measurement_started_at_run_us -ge [bigint]$Timing.startup_deadline_at_run_us -or
        [bigint]$Timing.measurement_deadline_at_run_us -le [bigint]$Timing.measurement_started_at_run_us) {
        throw 'Phase measurement does not follow bounded owner preparation'
    }
}
function Assert-PhaseBmxRuntimeClosure($Receipt) {
    if ($null -eq $Receipt) { return }
    Assert-ExactReceiptFields $Receipt @('commands_released','namespace_owned','namespace_validation_error','namespace_restore_error','namespace_remove_error','file_leases_released','deadline_exceeded','outstanding_owner_error') 'BMX runtime closure'
    foreach ($name in @('commands_released','namespace_owned','file_leases_released','deadline_exceeded')) {
        if ($Receipt.$name -isnot [bool]) { throw "BMX closure $name must be boolean" }
    }
    if (-not $Receipt.commands_released -or -not $Receipt.namespace_owned -or -not $Receipt.file_leases_released -or $Receipt.deadline_exceeded) { throw 'BMX runtime ownership did not close within its original deadline' }
    foreach ($name in @('namespace_validation_error','namespace_restore_error','namespace_remove_error','outstanding_owner_error')) {
        if ($null -ne $Receipt.$name) { throw "BMX runtime closure retained $name" }
    }
}
function Assert-PhaseVerifierClosure($Verifier, [string]$PhaseKind) {
    if ($PhaseKind -ceq 'playback_reference') {
        if ($null -ne $Verifier) { throw 'Playback-only phase invented an Export verifier' }
        return
    }
    Assert-ExactReceiptFields $Verifier @('workers_started','workers_joined','workers_remaining','workers_abandoned','cancellation_requested','deadline_exceeded','last_worker','failure','terminal_publications') 'Phase verifier'
    foreach ($name in @('workers_started','workers_joined','workers_remaining','workers_abandoned')) { Assert-PhaseInteger $Verifier.$name "Verifier $name" }
    if ($Verifier.workers_started -eq 0 -or $Verifier.workers_started -ne $Verifier.workers_joined -or $Verifier.workers_remaining -ne 0 -or $Verifier.workers_abandoned -ne 0) { throw 'Verifier worker inventory did not close' }
    if ($Verifier.cancellation_requested -isnot [bool] -or $Verifier.deadline_exceeded -isnot [bool] -or $Verifier.deadline_exceeded -or $null -ne $Verifier.failure) { throw 'Verifier owner failed or exceeded its deadline' }
    Assert-PhaseTerminalPublicationClosure $Verifier.terminal_publications
    $worker = $Verifier.last_worker
    Assert-ExactReceiptFields $worker @('job_id','output_path','thread_joined','evidence_persisted','native_cleanup','verification_failure','panic','failure') 'Verifier last worker'
    $job = [guid]::Empty
    if ($worker.job_id -isnot [string] -or -not [guid]::TryParseExact($worker.job_id, 'D', [ref]$job) -or $job -eq [guid]::Empty -or $worker.job_id -cne $job.ToString('D') -or $worker.output_path -isnot [string] -or [string]::IsNullOrWhiteSpace($worker.output_path)) { throw 'Verifier job or output identity is invalid' }
    if ($worker.thread_joined -isnot [bool] -or -not $worker.thread_joined -or $worker.evidence_persisted -isnot [bool] -or -not $worker.evidence_persisted -or $null -ne $worker.verification_failure -or $null -ne $worker.panic -or $null -ne $worker.failure) { throw 'Verifier result or evidence did not close' }
    $native = $worker.native_cleanup
    Assert-ExactReceiptFields $native @('native_exit_observed','kill_error','wait_error','deadline_exceeded','stdin_error','stdout_error','stderr_error') 'Verifier native cleanup'
    if ($native.native_exit_observed -isnot [bool] -or -not $native.native_exit_observed -or $native.deadline_exceeded -isnot [bool] -or $native.deadline_exceeded) { throw 'Verifier native exit was not observed within its deadline' }
    foreach ($name in @('kill_error','wait_error','stdin_error','stdout_error','stderr_error')) { if ($null -ne $native.$name) { throw "Verifier native $name retained an error" } }
}
function Assert-PhaseTerminalPublicationClosure($Receipt) {
    Assert-ExactReceiptFields $Receipt @('workers_started','workers_joined','workers_remaining','workers_abandoned','last_worker','failure') 'Terminal publication'
    foreach ($name in @('workers_started','workers_joined','workers_remaining','workers_abandoned')) { Assert-PhaseInteger $Receipt.$name "Terminal publication $name" }
    if ($Receipt.workers_started -eq 0 -or $Receipt.workers_started -ne $Receipt.workers_joined -or $Receipt.workers_remaining -ne 0 -or $Receipt.workers_abandoned -ne 0 -or $null -ne $Receipt.failure) { throw 'Terminal publication worker inventory did not close' }
    $worker = $Receipt.last_worker
    Assert-ExactReceiptFields $worker @('job_id','output_path','thread_joined','evidence_persisted','terminal_snapshot_json','panic','failure') 'Terminal publication last worker'
    $jobId = [guid]::Empty
    if ($worker.job_id -isnot [string] -or -not [guid]::TryParseExact($worker.job_id, 'D', [ref]$jobId) -or $jobId -eq [guid]::Empty -or $worker.job_id -cne $jobId.ToString('D') -or $worker.output_path -isnot [string] -or [string]::IsNullOrWhiteSpace($worker.output_path)) { throw 'Terminal publication identity is invalid' }
    if ($worker.thread_joined -isnot [bool] -or -not $worker.thread_joined -or $worker.evidence_persisted -isnot [bool] -or -not $worker.evidence_persisted -or $null -ne $worker.panic -or $null -ne $worker.failure) { throw 'Terminal publication did not join and persist' }
    if ($worker.terminal_snapshot_json -isnot [string] -or [string]::IsNullOrEmpty($worker.terminal_snapshot_json) -or [Text.Encoding]::UTF8.GetByteCount($worker.terminal_snapshot_json) -gt 2097152) { throw 'Terminal snapshot JSON is missing or exceeds its bound' }
    $raw = $worker.terminal_snapshot_json | ConvertFrom-Json -Depth 80 -DateKind String
    Assert-ExactReceiptFields $raw @('schema_version','job') 'Terminal snapshot'
    Assert-PhaseInteger $raw.schema_version 'Terminal snapshot schema'
    if ($raw.schema_version -ne 1) { throw 'Terminal snapshot schema differs' }
    $job = $raw.job
    Assert-ExactReceiptFields $job @('id','generation','output_path','output_policy','preset_name','status','progress','publication','diagnostics','created_at','started_at','completed_at','terminal_evidence','artifact_publication','executed') 'Terminal job snapshot'
    Assert-PhaseInteger $job.generation 'Terminal job generation'
    if ($job.id -cne $worker.job_id -or $job.output_path -cne $worker.output_path -or $job.status.status -cnotin @('completed','cancelled','failed') -or $job.executed -isnot [bool] -or $job.completed_at -isnot [string] -or [string]::IsNullOrEmpty($job.completed_at)) { throw 'Terminal job binding or lifecycle differs' }
}
function Assert-MondrianPhaseOwnerReceipt($Receipt, [string]$RunId, [string]$PhaseId, [int]$Ordinal, [string]$PhaseKind) {
    Assert-ExactReceiptFields $Receipt @('phase_id','report_path','canonical_json','sha256') 'Phase owner receipt'
    if ($Receipt.phase_id -cne $PhaseId -or $Receipt.report_path -isnot [string] -or [string]::IsNullOrWhiteSpace($Receipt.report_path)) { throw 'Phase owner identity or report path differs' }
    $raw = Read-CanonicalOwnerLeaf ([pscustomobject]@{ canonical_json=$Receipt.canonical_json; sha256=$Receipt.sha256 }) 'Phase owner report' 'canonical_json'
    $phaseFields = @('schema_version','run_id','phase_id','ordinal','terminal')
    if ('measurement_timing' -cin @($raw.PSObject.Properties.Name)) {
        $phaseFields += 'measurement_timing'
        Assert-PhaseMeasurementTiming $raw.measurement_timing
    }
    if ('ancillary_program_sha256' -cin @($raw.PSObject.Properties.Name)) {
        $phaseFields += @('ancillary_program_sha256','ancillary_export_artifacts','wire_journals')
        if ($raw.ancillary_program_sha256 -isnot [string] -or $raw.ancillary_program_sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Phase ancillary program digest is invalid' }
    }
    Assert-ExactReceiptFields $raw $phaseFields 'Phase report'
    if ('ancillary_program_sha256' -cin @($raw.PSObject.Properties.Name)) {
        if ($raw.ancillary_export_artifacts -isnot [array] -or $raw.ancillary_export_artifacts.Count -gt 256 -or $raw.wire_journals -isnot [array] -or $raw.wire_journals.Count -gt 256) { throw 'Phase ancillary inventories exceed their bounds' }
        $artifactIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        $paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($artifact in $raw.ancillary_export_artifacts) {
            Assert-ExactReceiptFields $artifact @('artifact_id','verification_path','verification_sha256') 'Phase ancillary artifact'
            $parsedArtifactId = [guid]::Empty
            if ($artifact.artifact_id -isnot [string] -or $artifact.artifact_id -cnotmatch '^endurance-export-[0-9a-f-]{36}$' -or -not [guid]::TryParseExact($artifact.artifact_id.Substring(17), 'D', [ref]$parsedArtifactId) -or $parsedArtifactId -eq [guid]::Empty -or -not $artifactIds.Add($artifact.artifact_id) -or $artifact.verification_path -isnot [string] -or [string]::IsNullOrWhiteSpace($artifact.verification_path) -or -not $paths.Add($artifact.verification_path) -or $artifact.verification_sha256 -isnot [string] -or $artifact.verification_sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Phase ancillary artifact identity is invalid or repeated' }
        }
        foreach ($journal in $raw.wire_journals) {
            Assert-ExactReceiptFields $journal @('path','sha256') 'Phase ancillary wire journal'
            if ($journal.path -isnot [string] -or [string]::IsNullOrWhiteSpace($journal.path) -or -not $paths.Add($journal.path) -or $journal.sha256 -isnot [string] -or $journal.sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Phase ancillary journal identity is invalid or repeated' }
        }
    }
    Assert-PhaseInteger $raw.schema_version 'Phase schema'
    Assert-PhaseInteger $raw.ordinal 'Phase ordinal'
    if ($raw.schema_version -ne 2 -or $raw.run_id -cne $RunId -or $raw.phase_id -cne $PhaseId -or $raw.ordinal -ne $Ordinal) { throw 'Phase owner report binding differs' }
    $terminal = $raw.terminal
    $terminalFields = @('phase_kind','closure','owners','export_verifier','failures')
    if ('bmx_runtime' -cin @($terminal.PSObject.Properties.Name)) {
        $terminalFields += 'bmx_runtime'
        Assert-PhaseBmxRuntimeClosure $terminal.bmx_runtime
    }
    Assert-ExactReceiptFields $terminal $terminalFields 'Phase terminal'
    Assert-PhaseVerifierClosure $terminal.export_verifier $PhaseKind
    if ($terminal.phase_kind -cne $PhaseKind -or $terminal.failures -isnot [array] -or $terminal.failures.Count -ne 0) { throw 'Phase terminal kind or failures are not clean' }
    Assert-ExactReceiptFields $terminal.closure @('status','playback_workers_terminated','supervised_child_processes_remaining','export') 'Phase closure'
    Assert-PhaseInteger $terminal.closure.supervised_child_processes_remaining 'Phase child processes'
    if ($terminal.closure.status -cne 'completed' -or $terminal.closure.playback_workers_terminated -isnot [bool] -or -not $terminal.closure.playback_workers_terminated -or $terminal.closure.supervised_child_processes_remaining -ne 0) { throw 'Phase closure did not consume all owners' }
    if ($PhaseKind -ceq 'continuous_export') {
        Assert-ExactReceiptFields $terminal.owners @('AppOnly') 'Continuous Export owners'
        $app = $terminal.owners.AppOnly
    } else {
        Assert-ExactReceiptFields $terminal.owners @('Realtime') 'Realtime phase owners'
        $owners = $terminal.owners.Realtime
        Assert-ExactReceiptFields $owners @('app','preview','gpu','waveform','owner_snapshot_failure','terminal_projection_failure','terminal_owner_snapshot') 'Realtime owners'
        if ($null -ne $owners.owner_snapshot_failure -or $null -ne $owners.terminal_projection_failure) { throw 'Realtime phase has a failed owner projection' }
        Assert-MondrianPreviewOwnerClosure $owners.preview
        Assert-MondrianWaveformOwnerClosure $owners.waveform
        Assert-MondrianRawGpuClosure $owners.gpu
        $snapshot = $owners.terminal_owner_snapshot
        Assert-ExactReceiptFields $snapshot @('playback_pending','other_queue_depth','owned_resource_units','gpu_device_losses','gpu_fatal_errors','other_fatal_errors','app_background','fatal_error_total','owner_capture_failed') 'Realtime terminal snapshot'
        foreach ($name in @('playback_pending','other_queue_depth','owned_resource_units','gpu_device_losses','gpu_fatal_errors','other_fatal_errors','fatal_error_total')) { Assert-PhaseInteger $snapshot.$name "Snapshot $name"; if ($snapshot.$name -ne 0) { throw "Realtime snapshot $name is not zero" } }
        if ($snapshot.owner_capture_failed -isnot [bool] -or $snapshot.owner_capture_failed) { throw 'Realtime owner capture failed' }
        Assert-ExactReceiptFields $snapshot.app_background @('audio_idle_warmup','media_import','media_asset_mutation','visual_tracking','proxy_generation','infrastructure') 'Realtime background'
        foreach ($domain in $snapshot.app_background.PSObject.Properties) {
            Assert-ExactReceiptFields $domain.Value @('queue_depth','owned_resource_units','cumulative_failures','worker_health_failures') 'Realtime background domain'
            foreach ($value in $domain.Value.PSObject.Properties.Value) { Assert-PhaseInteger $value "Background owner value"; if ($value -ne 0) { throw 'Realtime background owner did not close' } }
        }
        $app = $owners.app
    }
    Assert-MondrianCanonicalAppClosure $app
    $appRaw = Read-CanonicalOwnerLeaf $app 'Phase App' 'canonical_json'
    $exportRaw = Read-CanonicalOwnerLeaf $appRaw.export 'Phase Export'
    if (($terminal.closure.export | ConvertTo-Json -Depth 40 -Compress) -cne ($exportRaw | ConvertTo-Json -Depth 40 -Compress)) { throw 'Phase Export projection differs from the actual App receipt' }
}

Export-ModuleMember -Function Assert-MondrianPhaseOwnerReceipt,Assert-PhaseMeasurementTiming
