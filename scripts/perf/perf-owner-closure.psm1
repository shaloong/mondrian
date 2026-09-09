Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Explicit schema leaves, not PowerShell's coercive numeric conversions. Keep
# this inventory shared by all required-field checks, including zero counters.
$script:IntegerLeaves = @(
    "native_wake_failures", "wake_registration_rejections",
    "registrations_accepted", "registrations_released", "registrations_abandoned",
    "registrations_retained", "invocations_in_flight", "retirements_active",
    "invocation_panics", "destructor_panics", "opaque_payloads_abandoned",
    "schema_version", "shared_deadline_budget_ms", "preview_owner_count", "owner_slot",
    "requested_workers", "started_workers", "terminated_workers", "panicked_workers",
    "timed_out_workers", "detached_workers", "unexpected_worker_exits",
    "queued_work_remaining", "running_work_remaining", "owned_resources_remaining",
    "cumulative_failures", "workers_started", "workers_terminated", "worker_panics", "worker_panic_payloads_abandoned",
    "current_thread_detachments", "unverified_async_reaps", "worker_timeouts",
    "worker_deadline_detachments", "render_cache_schema_version",
    "retired_library_generations_remaining", "outstanding_frames_before_shutdown",
    "outstanding_frames", "outstanding_resources", "pending_jobs", "active_jobs",
    "activity_events", "audio_source_owners_started", "audio_source_owners_closed",
    "audio_source_owner_failures", "active_audio_source_owners", "observed_at_us",
    "admissions", "rejections", "completions", "failures", "cancellations",
    "rendered_frames", "durable_artifacts", "render_workers_started",
    "render_workers_terminated", "render_worker_panics", "render_worker_owner_abandonments",
    "render_worker_terminal_evidence_missing", "render_current_thread_detachments",
    "render_retirement_workers_started", "render_retirement_workers_terminated",
    "render_retirement_worker_panics", "render_retirement_worker_owner_abandonments",
    "render_retirement_worker_terminal_evidence_missing", "render_retirement_current_thread_detachments",
    "foreign_owner_panics", "foreign_owner_abandonments", "shutdown_coordinators_started",
    "shutdown_coordinators_terminated", "shutdown_coordinator_start_failures",
    "shutdown_coordinator_panics", "shutdown_coordinator_timeouts", "shutdown_coordinator_detachments",
    "shutdown_coordinator_spawner_panics", "shutdown_coordinator_owner_abandonments",
    "worker_start_failures", "worker_spawner_panics", "worker_owner_abandonments",
    "worker_terminal_evidence_missing", "strong_references_before_consumption",
    "strong_references_remaining", "in_flight_decodes_before", "pcm_entries_before",
    "pcm_bytes_before", "failure_entries_before", "external_pcm_buffer_references",
    "pcm_entries_remaining", "pcm_bytes_remaining", "failure_entries_remaining",
    "decoder_sessions_before", "decoder_sessions_remaining", "child_processes_observed",
    "child_processes_terminated", "child_process_termination_failures", "stdout_pump_threads_observed",
    "stdout_pump_threads_joined", "stdout_pump_threads_panicked", "stdout_pump_thread_owner_abandonments",
    "stderr_pump_threads_observed", "stderr_pump_threads_joined", "stderr_pump_threads_panicked",
    "stderr_pump_thread_owner_abandonments", "external_decoder_session_references",
    "decoder_resource_handles_remaining", "decoder_shutdown_workers_started",
    "decoder_shutdown_workers_terminated", "decoder_shutdown_worker_start_failures",
    "decoder_shutdown_worker_panics", "decoder_shutdown_worker_publication_missing",
    "decoder_shutdown_worker_owner_abandonments", "external_decoder_references",
    "iterations", "avg_ms", "max_ms", "threshold_ms"
)

function Assert-UnsignedInteger {
    param($Value, [string]$Context)
    if ($Value -isnot [byte] -and $Value -isnot [int16] -and
        $Value -isnot [uint16] -and $Value -isnot [int32] -and
        $Value -isnot [uint32] -and $Value -isnot [int64] -and
        $Value -isnot [uint64] -and $Value -isnot [bigint]) {
        throw "$Context must be a JSON integer"
    }
    if ($Value -lt 0 -or [bigint]$Value -gt [bigint]::Parse("340282366920938463463374607431768211455")) {
        throw "$Context is outside the unsigned 128-bit producer range"
    }
}

function Assert-BoolType {
    param($Value, [string]$Context)
    if ($Value -isnot [bool]) { throw "$Context must be a JSON boolean" }
}

function Assert-Fields {
    param(
        [Parameter(Mandatory = $true)]$Value,
        [Parameter(Mandatory = $true)][string[]]$Names,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if ($null -eq $Value) {
        throw "$Context is missing"
    }
    $actual = @($Value.PSObject.Properties.Name)
    foreach ($name in $Names) {
        if ($name -notin $actual) {
            throw "$Context is missing required leaf '$name'"
        }
        if ($name -in $script:IntegerLeaves) {
            Assert-UnsignedInteger $Value.$name "$Context.$name"
        }
    }
}

function Assert-Bool {
    param($Value, [bool]$Expected, [string]$Context)
    if ($Value -isnot [bool] -or $Value -ne $Expected) {
        throw "$Context must be the boolean '$Expected'"
    }
}

function Assert-Zero {
    param($Value, [string]$Context)
    Assert-UnsignedInteger $Value $Context
    if ($Value -ne 0) {
        throw "$Context must be zero"
    }
}

function Assert-Null {
    param($Value, [string]$Context)
    if ($null -ne $Value) {
        throw "$Context must be null"
    }
}

function Assert-CleanWorker {
    param($Worker, [string]$Context)
    Assert-Fields $Worker @(
        "domain", "requested_workers", "startup_attempted", "started_workers",
        "terminated_workers", "panicked_workers", "timed_out_workers",
        "detached_workers", "unexpected_worker_exits", "queued_work_remaining",
        "running_work_remaining", "owned_resources_remaining", "cumulative_failures",
        "lifecycle_closed"
    ) $Context
    Assert-BoolType $Worker.startup_attempted "$Context.startup_attempted"
    if (($Worker.startup_attempted -and $Worker.requested_workers -ne $Worker.started_workers) -or
        $Worker.started_workers -ne $Worker.terminated_workers) {
        throw "$Context did not terminate every started worker"
    }
    foreach ($leaf in @(
        "panicked_workers", "timed_out_workers", "detached_workers",
        "unexpected_worker_exits", "queued_work_remaining", "running_work_remaining",
        "owned_resources_remaining", "cumulative_failures"
    )) {
        Assert-Zero $Worker.$leaf "$Context.$leaf"
    }
    Assert-Bool $Worker.lifecycle_closed $true "$Context.lifecycle_closed"
}

function Assert-CleanPreviewCallbacks {
    param($Callbacks, [string]$Context)
    $fields = @(
        "schema_version", "admission_closed", "registrations_accepted", "registrations_released",
        "registrations_abandoned", "registrations_retained", "invocations_in_flight", "retirements_active",
        "invocation_panics", "destructor_panics", "opaque_payloads_abandoned", "worker_start_failures",
        "worker_started", "worker_panics", "worker", "deadline_met", "shutdown_rejection"
    )
    Assert-Fields $Callbacks $fields $Context
    $actual = @($Callbacks.PSObject.Properties.Name)
    if ($actual.Count -ne $fields.Count) { throw "$Context has unexpected callback fields" }
    foreach ($field in $fields) {
        if ($actual -cnotcontains $field) { throw "$Context requires exact field '$field'" }
    }
    if ($Callbacks.schema_version -ne 1 -or
        [uint64]$Callbacks.registrations_accepted -ne [uint64]$Callbacks.registrations_released) {
        throw "$Context has unknown schema or unreleased registrations"
    }
    foreach ($leaf in @(
        "registrations_abandoned", "registrations_retained", "invocations_in_flight", "retirements_active",
        "invocation_panics", "destructor_panics", "opaque_payloads_abandoned", "worker_start_failures", "worker_panics"
    )) { Assert-Zero $Callbacks.$leaf "$Context.$leaf" }
    Assert-Bool $Callbacks.admission_closed $true "$Context.admission_closed"
    Assert-Bool $Callbacks.deadline_met $true "$Context.deadline_met"
    Assert-BoolType $Callbacks.worker_started "$Context.worker_started"
    Assert-Null $Callbacks.shutdown_rejection "$Context.shutdown_rejection"
    if ($Callbacks.worker -isnot [string]) { throw "$Context.worker must be a scalar string" }
    if ($Callbacks.worker_started) {
        if ($Callbacks.worker -cne 'terminated' -or $Callbacks.registrations_accepted -eq 0) {
            throw "$Context worker was not joined or owns no accepted registration"
        }
    } elseif ($Callbacks.worker -cne 'not_started' -or $Callbacks.registrations_accepted -ne 0) {
        throw "$Context has unproved unstarted callback ownership"
    }
}

function Assert-CleanPreview {
    param($Preview, [string]$Context)
    Assert-Fields $Preview @(
        "owner_slot", "schema_version", "workers_started", "workers_terminated", "worker_panics",
        "worker_panic_payloads_abandoned", "work_callbacks",
        "current_thread_detachments", "unverified_async_reaps", "worker_timeouts",
        "worker_deadline_detachments", "render_cache_schema_version",
        "render_cache_required", "render_cache_start_failed", "render_cache_worker", "visual_dependency_worker",
        "render_cache_aggregate_outcome", "all_resources_released"
    ) $Context
    if ([uint64]$Preview.schema_version -ne 4 -or
        [uint64]$Preview.render_cache_schema_version -ne 1 -or
        [uint64]$Preview.workers_started -ne [uint64]$Preview.workers_terminated) {
        throw "$Context has an unknown schema or incomplete worker inventory"
    }
    foreach ($leaf in @(
        "worker_panics", "current_thread_detachments", "unverified_async_reaps",
        "worker_timeouts", "worker_deadline_detachments"
    )) {
        Assert-Zero $Preview.$leaf "$Context.$leaf"
    }
    Assert-Zero $Preview.worker_panic_payloads_abandoned "$Context.worker_panic_payloads_abandoned"
    Assert-CleanPreviewCallbacks $Preview.work_callbacks "$Context.work_callbacks"
    if ([uint64]$Preview.workers_started -lt (2 + [int]$Preview.work_callbacks.worker_started)) {
        throw "$Context worker inventory omits a declared observer, render-cache or callback owner"
    }
    Assert-Bool $Preview.render_cache_required $true "$Context.render_cache_required"
    Assert-Bool $Preview.render_cache_start_failed $false "$Context.render_cache_start_failed"
    if ($Preview.visual_dependency_worker -cne 'terminated') { throw "$Context.visual_dependency_worker did not join cleanly" }
    if ([string]$Preview.render_cache_aggregate_outcome -ne "terminated") {
        throw "$Context render-cache aggregate did not terminate"
    }
    Assert-Fields $Preview.render_cache_worker @(
        "worker_started", "worker_terminated", "worker_panicked", "current_thread_skipped",
        "timed_out", "detached", "all_workers_terminated"
    ) "$Context.render_cache_worker"
    foreach ($leaf in @("worker_started", "worker_terminated", "all_workers_terminated")) {
        Assert-Bool $Preview.render_cache_worker.$leaf $true "$Context.render_cache_worker.$leaf"
    }
    foreach ($leaf in @("worker_panicked", "current_thread_skipped", "timed_out", "detached")) {
        Assert-Bool $Preview.render_cache_worker.$leaf $false "$Context.render_cache_worker.$leaf"
    }
    Assert-Bool $Preview.all_resources_released $true "$Context.all_resources_released"
}

function Assert-CleanGpu {
    param($Gpu, [string]$Context)
    Assert-Fields $Gpu @(
        "worker_shutdown", "wake_callbacks", "native_wake_failures", "wake_registration_rejections",
        "worker_started", "worker_terminated", "worker_panicked", "timed_out",
        "retirement_requested", "retirement_handoff_accepted", "retirement_completed",
        "generation_terminal_kind", "renderer_retirement", "all_resources_released"
    ) $Context
    if ($Gpu.worker_shutdown -isnot [string] -or $Gpu.worker_shutdown -cne 'terminated') {
        throw "$Context has no exact healthy progress-worker join"
    }
    Assert-CleanPreviewCallbacks $Gpu.wake_callbacks "$Context.wake_callbacks"
    Assert-Zero $Gpu.native_wake_failures "$Context.native_wake_failures"
    Assert-Zero $Gpu.wake_registration_rejections "$Context.wake_registration_rejections"
    foreach ($leaf in @(
        "worker_started", "worker_terminated", "retirement_requested",
        "retirement_handoff_accepted", "retirement_completed", "all_resources_released"
    )) {
        Assert-Bool $Gpu.$leaf $true "$Context.$leaf"
    }
    foreach ($leaf in @("worker_panicked", "timed_out")) {
        Assert-Bool $Gpu.$leaf $false "$Context.$leaf"
    }
    Assert-Null $Gpu.generation_terminal_kind "$Context.generation_terminal_kind"
    Assert-Fields $Gpu.renderer_retirement @("cpu_yuv_upload", "native_device_removed") "$Context.renderer_retirement"
    if ($Gpu.renderer_retirement.cpu_yuv_upload -cne "returned") {
        throw "$Context did not observe a normally returned and joined YUV upload worker"
    }
    Assert-Bool $Gpu.renderer_retirement.native_device_removed $false "$Context.renderer_retirement.native_device_removed"
}

function Assert-CleanProject {
    param($Project, [string]$Context)
    Assert-Fields $Project @(
        "session_was_open", "pending_close_was_active", "authoring_session_released",
        "pending_close_released", "runtime_lease_released",
        "retired_library_generations_remaining", "lifecycle_failure", "all_resources_released"
    ) $Context
    foreach ($leaf in @(
        "session_was_open", "pending_close_was_active", "authoring_session_released",
        "pending_close_released", "runtime_lease_released", "all_resources_released"
    )) {
        Assert-Bool $Project.$leaf $true "$Context.$leaf"
    }
    Assert-Zero $Project.retired_library_generations_remaining "$Context.retired_library_generations_remaining"
    Assert-Null $Project.lifecycle_failure "$Context.lifecycle_failure"
}

function Assert-CleanReferenceOutput {
    param($App, [string]$Context)
    Assert-Bool $App.reference_output_resources_released $true "$Context.reference_output_resources_released"
    $receipt = $App.reference_output
    Assert-Fields $receipt @(
        "schema_version", "session", "diagnostics", "outstanding_frames_before_shutdown",
        "module_failure"
    ) "$Context.reference_output"
    if ([uint64]$receipt.schema_version -ne 2) {
        throw "$Context.reference_output has an unknown schema"
    }
    Assert-Zero $receipt.outstanding_frames_before_shutdown "$Context.reference_output.outstanding_frames_before_shutdown"
    Assert-Null $receipt.module_failure "$Context.reference_output.module_failure"
    $session = $receipt.session
    Assert-Fields $session @(
        "schema_version", "session_present", "shutdown_request_completed", "playback_stopped",
        "callback_execution_terminated", "device_released", "outstanding_frames",
        "outstanding_resources", "provider_failure", "coordinator"
    ) "$Context.reference_output.session"
    Assert-BoolType $session.session_present "$Context.reference_output.session.session_present"
    if ([uint64]$session.schema_version -ne 2) {
        throw "$Context.reference_output.session has an unknown schema"
    }
    foreach ($leaf in @(
        "shutdown_request_completed", "playback_stopped", "callback_execution_terminated",
        "device_released"
    )) {
        Assert-Bool $session.$leaf $true "$Context.reference_output.session.$leaf"
    }
    foreach ($leaf in @("outstanding_frames", "outstanding_resources")) {
        Assert-Zero $session.$leaf "$Context.reference_output.session.$leaf"
    }
    Assert-Null $session.provider_failure "$Context.reference_output.session.provider_failure"
    Assert-Fields $session.coordinator @(
        "required", "spawned", "joined", "panicked", "timed_out", "detached",
        "owner_abandoned"
    ) "$Context.reference_output.session.coordinator"
    foreach ($leaf in @("panicked", "timed_out", "detached", "owner_abandoned")) {
        Assert-Bool $session.coordinator.$leaf $false "$Context.reference_output.session.coordinator.$leaf"
    }
    foreach ($leaf in @("required", "spawned", "joined")) {
        Assert-BoolType $session.coordinator.$leaf "$Context.reference_output.session.coordinator.$leaf"
    }
    if (($session.coordinator.required -and !$session.coordinator.spawned) -or
        $session.coordinator.spawned -ne $session.coordinator.joined) {
        throw "$Context.reference_output.session.coordinator did not close its required worker"
    }
    Assert-Fields $receipt.diagnostics @("schema_version", "outstanding_frames", "last_error") "$Context.reference_output.diagnostics"
    if ([uint64]$receipt.diagnostics.schema_version -ne 1) {
        throw "$Context.reference_output.diagnostics has an unknown schema"
    }
    Assert-Zero $receipt.diagnostics.outstanding_frames "$Context.reference_output.diagnostics.outstanding_frames"
    Assert-Null $receipt.diagnostics.last_error "$Context.reference_output.diagnostics.last_error"
}

function Assert-CleanExport {
    param($App, [string]$Context)
    Assert-Bool $App.export_resources_released $true "$Context.export_resources_released"
    $queue = $App.export
    Assert-Fields $queue @(
        "schema_version", "worker_started", "worker_start_failed", "worker_terminated",
        "worker_panicked", "worker_timed_out", "worker_detached", "worker_owner_abandoned",
        "pending_jobs", "active_jobs", "activity_events", "audio_source_owners_started",
        "audio_source_owners_closed", "audio_source_owner_failures", "active_audio_source_owners"
    ) "$Context.export"
    if ([uint64]$queue.schema_version -ne 4) { throw "$Context.export has an unknown schema" }
    Assert-Bool $queue.worker_started $true "$Context.export.worker_started"
    Assert-Bool $queue.worker_terminated $true "$Context.export.worker_terminated"
    foreach ($leaf in @(
        "worker_start_failed", "worker_panicked", "worker_timed_out", "worker_detached",
        "worker_owner_abandoned"
    )) { Assert-Bool $queue.$leaf $false "$Context.export.$leaf" }
    foreach ($leaf in @("pending_jobs", "active_jobs", "audio_source_owner_failures", "active_audio_source_owners")) {
        Assert-Zero $queue.$leaf "$Context.export.$leaf"
    }
    if ([uint64]$queue.audio_source_owners_started -ne [uint64]$queue.audio_source_owners_closed) {
        throw "$Context.export did not close every audio-source owner"
    }
    $terminal = $App.export_terminal_snapshot
    Assert-Fields $terminal @(
        "schema_version", "observed_at_us", "shutdown_requested", "worker_running",
        "worker_terminated", "activity_events", "admissions", "rejections", "completions",
        "failures", "cancellations", "rendered_frames", "durable_artifacts", "pending_jobs",
        "active_jobs", "worker_failed", "audio_source_owners_started",
        "audio_source_owners_closed", "audio_source_owner_failures", "active_audio_source_owners"
    ) "$Context.export_terminal_snapshot"
    if ([uint64]$terminal.schema_version -ne 2) { throw "$Context.export_terminal_snapshot has an unknown schema" }
    Assert-Bool $terminal.shutdown_requested $true "$Context.export_terminal_snapshot.shutdown_requested"
    Assert-Bool $terminal.worker_running $false "$Context.export_terminal_snapshot.worker_running"
    Assert-Bool $terminal.worker_terminated $true "$Context.export_terminal_snapshot.worker_terminated"
    Assert-Bool $terminal.worker_failed $false "$Context.export_terminal_snapshot.worker_failed"
    foreach ($leaf in @("pending_jobs", "active_jobs", "audio_source_owner_failures", "active_audio_source_owners")) {
        Assert-Zero $terminal.$leaf "$Context.export_terminal_snapshot.$leaf"
    }
    if ([uint64]$terminal.audio_source_owners_started -ne [uint64]$terminal.audio_source_owners_closed) {
        throw "$Context.export_terminal_snapshot did not close every audio-source owner"
    }
    foreach ($leaf in @("activity_events", "pending_jobs", "active_jobs",
        "audio_source_owners_started", "audio_source_owners_closed",
        "audio_source_owner_failures", "active_audio_source_owners")) {
        if ($terminal.$leaf -ne $queue.$leaf) {
            throw "$Context.export and terminal snapshot disagree on '$leaf'"
        }
    }
}

function Assert-CleanAudio {
    param($Audio, [string]$Context)
    Assert-Fields $Audio @(
        "schema_version", "render_workers_started", "render_workers_terminated",
        "render_worker_panics", "render_worker_owner_abandonments",
        "render_worker_terminal_evidence_missing", "render_current_thread_detachments",
        "render_retirement_workers_started", "render_retirement_workers_terminated",
        "render_retirement_worker_panics", "render_retirement_worker_owner_abandonments",
        "render_retirement_worker_terminal_evidence_missing",
        "render_retirement_current_thread_detachments", "output", "foreign_owner_panics",
        "foreign_owner_abandonments", "shutdown_coordinators_started",
        "shutdown_coordinators_terminated", "shutdown_coordinator_start_failures",
        "shutdown_coordinator_panics", "shutdown_coordinator_timeouts",
        "shutdown_coordinator_detachments", "shutdown_coordinator_spawner_panics",
        "shutdown_coordinator_owner_abandonments",
        "shutdown_resource_facts_complete_at_deadline",
        "shutdown_owner_lifetime_unresolved_at_deadline", "all_workers_terminated"
    ) $Context
    if ([uint64]$Audio.schema_version -ne 3 -or
        [uint64]$Audio.render_workers_started -ne [uint64]$Audio.render_workers_terminated -or
        [uint64]$Audio.render_retirement_workers_started -ne [uint64]$Audio.render_retirement_workers_terminated -or
        [uint64]$Audio.shutdown_coordinators_started -ne [uint64]$Audio.shutdown_coordinators_terminated) {
        throw "$Context has an unknown schema or incomplete worker inventory"
    }
    foreach ($leaf in @(
        "render_worker_panics", "render_worker_owner_abandonments",
        "render_worker_terminal_evidence_missing", "render_current_thread_detachments",
        "render_retirement_worker_panics", "render_retirement_worker_owner_abandonments",
        "render_retirement_worker_terminal_evidence_missing",
        "render_retirement_current_thread_detachments", "foreign_owner_panics",
        "foreign_owner_abandonments", "shutdown_coordinator_start_failures",
        "shutdown_coordinator_panics", "shutdown_coordinator_timeouts",
        "shutdown_coordinator_detachments", "shutdown_coordinator_spawner_panics",
        "shutdown_coordinator_owner_abandonments"
    )) { Assert-Zero $Audio.$leaf "$Context.$leaf" }
    Assert-Bool $Audio.shutdown_resource_facts_complete_at_deadline $true "$Context.shutdown_resource_facts_complete_at_deadline"
    Assert-Bool $Audio.shutdown_owner_lifetime_unresolved_at_deadline $false "$Context.shutdown_owner_lifetime_unresolved_at_deadline"
    Assert-Bool $Audio.all_workers_terminated $true "$Context.all_workers_terminated"
    $output = $Audio.output
    Assert-Fields $output @(
        "schema_version", "workers_started", "workers_terminated", "worker_start_failures",
        "worker_spawner_panics", "worker_panics", "current_thread_detachments",
        "worker_owner_abandonments", "worker_terminal_evidence_missing", "all_workers_terminated"
    ) "$Context.output"
    if ([uint64]$output.schema_version -ne 2 -or
        [uint64]$output.workers_started -ne [uint64]$output.workers_terminated) {
        throw "$Context.output has an unknown schema or incomplete worker inventory"
    }
    foreach ($leaf in @(
        "worker_start_failures", "worker_spawner_panics", "worker_panics",
        "current_thread_detachments", "worker_owner_abandonments",
        "worker_terminal_evidence_missing"
    )) { Assert-Zero $output.$leaf "$Context.output.$leaf" }
    Assert-Bool $output.all_workers_terminated $true "$Context.output.all_workers_terminated"
}

function Assert-CleanAudioStartup {
    param($Startup, [string]$Context)
    $counters = @(
        'workers_started', 'workers_joined', 'start_failures', 'panics',
        'publication_missing', 'owner_abandonments', 'unverified_native_owners',
        'requests_admitted', 'requests_claimed', 'requests_retired', 'canceled_before_spawn',
        'queued_remaining', 'in_flight_remaining', 'unclaimed_results_remaining', 'producers_remaining'
    )
    $fields = @('required', 'attempted') + $counters
    if ($null -eq $Startup -or $Startup -isnot [pscustomobject]) {
        throw "$Context must be one startup inventory object"
    }
    $actual = @($Startup.PSObject.Properties.Name)
    if ($actual.Count -ne $fields.Count) { throw "$Context has a noncanonical shape" }
    foreach ($field in $fields) {
        if ($field -cnotin $actual) { throw "$Context is missing canonical field '$field'" }
    }
    Assert-Bool $Startup.required $true "$Context.required"
    Assert-Bool $Startup.attempted $true "$Context.attempted"
    foreach ($field in $counters) {
        Assert-UnsignedInteger $Startup.$field "$Context.$field"
        if ([bigint]$Startup.$field -gt [bigint][uint64]::MaxValue) {
            throw "$Context.$field exceeds its producer range"
        }
    }
    if ($Startup.workers_started -ne 1 -or $Startup.workers_joined -ne 1 -or
        [bigint]$Startup.requests_admitted -ne ([bigint]$Startup.requests_claimed + [bigint]$Startup.requests_retired) -or
        [bigint]$Startup.canceled_before_spawn -gt [bigint]$Startup.requests_admitted) {
        throw "$Context has incomplete worker or request ownership"
    }
    foreach ($field in @(
        'start_failures', 'panics', 'publication_missing', 'owner_abandonments',
        'unverified_native_owners', 'queued_remaining', 'in_flight_remaining',
        'unclaimed_results_remaining', 'producers_remaining'
    )) { Assert-Zero $Startup.$field "$Context.$field" }
}

function Assert-CleanAudioSource {
    param($Source, [string]$Context)
    Assert-Fields $Source @(
        "strong_references_before_consumption", "strong_references_remaining", "cache",
        "all_resources_released"
    ) $Context
    if ([uint64]$Source.strong_references_before_consumption -lt 1) {
        throw "$Context did not consume an Audio Source Cache owner"
    }
    Assert-Zero $Source.strong_references_remaining "$Context.strong_references_remaining"
    Assert-Bool $Source.all_resources_released $true "$Context.all_resources_released"
    Assert-CleanAudioSourceCache $Source.cache "$Context.cache"
}

# Raw owner receipt; shared by performance and independent Window replay.
function Assert-CleanAudioSourceCache {
    param($cache, [string]$Context)
    Assert-Fields $cache @(
        "schema_version", "decoder_startup", "in_flight_decodes_before", "pcm_entries_before", "pcm_bytes_before",
        "failure_entries_before", "external_pcm_buffer_references", "pcm_entries_remaining",
        "pcm_bytes_remaining", "failure_entries_remaining", "decoder_sessions_before",
        "decoder_sessions_remaining", "child_processes_observed", "child_processes_terminated",
        "child_process_termination_failures", "stdout_pump_threads_observed",
        "stdout_pump_threads_joined", "stdout_pump_threads_panicked",
        "stdout_pump_thread_owner_abandonments", "stderr_pump_threads_observed",
        "stderr_pump_threads_joined", "stderr_pump_threads_panicked",
        "stderr_pump_thread_owner_abandonments", "external_decoder_session_references",
        "decoder_resource_handles_remaining", "decoder_shutdown_workers_started",
        "decoder_shutdown_workers_terminated", "decoder_shutdown_worker_start_failures",
        "decoder_shutdown_worker_panics", "decoder_shutdown_worker_publication_missing",
        "decoder_shutdown_worker_owner_abandonments", "external_decoder_references",
        "shutdown_coordinators_started", "shutdown_coordinators_terminated",
        "shutdown_coordinator_start_failures", "shutdown_coordinator_panics",
        "shutdown_coordinator_timeouts", "shutdown_coordinator_detachments",
        "shutdown_coordinator_spawner_panics", "shutdown_coordinator_owner_abandonments",
        "shutdown_resource_facts_complete_at_deadline",
        "shutdown_owner_lifetime_unresolved_at_deadline"
    ) "$Context"
    Assert-CleanAudioStartup $cache.decoder_startup "$Context.decoder_startup"
    if ([uint64]$cache.schema_version -ne 6 -or
        [uint64]$cache.decoder_shutdown_workers_started -ne 1 -or
        [uint64]$cache.child_processes_observed -ne [uint64]$cache.child_processes_terminated -or
        [uint64]$cache.stdout_pump_threads_observed -ne [uint64]$cache.stdout_pump_threads_joined -or
        [uint64]$cache.stderr_pump_threads_observed -ne [uint64]$cache.stderr_pump_threads_joined -or
        [uint64]$cache.decoder_shutdown_workers_started -ne [uint64]$cache.decoder_shutdown_workers_terminated -or
        [uint64]$cache.shutdown_coordinators_started -ne [uint64]$cache.shutdown_coordinators_terminated) {
        throw "$Context has an unknown schema or incomplete worker inventory"
    }
    foreach ($leaf in @(
        "external_pcm_buffer_references", "pcm_entries_remaining", "pcm_bytes_remaining",
        "failure_entries_remaining", "decoder_sessions_remaining",
        "child_process_termination_failures", "stdout_pump_threads_panicked",
        "stdout_pump_thread_owner_abandonments", "stderr_pump_threads_panicked",
        "stderr_pump_thread_owner_abandonments", "external_decoder_session_references",
        "decoder_resource_handles_remaining", "decoder_shutdown_worker_start_failures",
        "decoder_shutdown_worker_panics", "decoder_shutdown_worker_publication_missing",
        "decoder_shutdown_worker_owner_abandonments", "external_decoder_references",
        "shutdown_coordinator_start_failures", "shutdown_coordinator_panics",
        "shutdown_coordinator_timeouts", "shutdown_coordinator_detachments",
        "shutdown_coordinator_spawner_panics", "shutdown_coordinator_owner_abandonments"
    )) { Assert-Zero $cache.$leaf "$Context.$leaf" }
    Assert-Bool $cache.shutdown_resource_facts_complete_at_deadline $true "$Context.shutdown_resource_facts_complete_at_deadline"
    Assert-Bool $cache.shutdown_owner_lifetime_unresolved_at_deadline $false "$Context.shutdown_owner_lifetime_unresolved_at_deadline"
}

function Assert-CleanApp {
    param($App, [string]$Context)
    Assert-Fields $App @(
        "app_owner_consumed", "project", "reference_output",
        "reference_output_resources_released", "export", "export_terminal_snapshot",
        "export_resources_released", "audio", "audio_source_cache", "workers",
        "all_resources_released"
    ) $Context
    Assert-Bool $App.app_owner_consumed $true "$Context.app_owner_consumed"
    Assert-CleanProject $App.project "$Context.project"
    Assert-CleanReferenceOutput $App $Context
    Assert-CleanExport $App $Context
    Assert-CleanAudio $App.audio "$Context.audio"
    Assert-CleanAudioSource $App.audio_source_cache "$Context.audio_source_cache"
    $expectedDomains = @(
        "execution_memory_observer", "project_persistence", "audio_idle_warmup",
        "media_import", "media_asset_mutation", "visual_tracking", "proxy_generation"
    )
    $workers = @($App.workers)
    $actualDomains = @($workers | ForEach-Object { [string]$_.domain } | Sort-Object)
    if ($workers.Count -ne $expectedDomains.Count -or
        (Compare-Object -ReferenceObject ($expectedDomains | Sort-Object) -DifferenceObject $actualDomains)) {
        throw "$Context has an incomplete or duplicated App worker-domain inventory"
    }
    foreach ($worker in $workers) {
        Assert-CleanWorker $worker "$Context.workers[$($worker.domain)]"
    }
    Assert-Bool $App.all_resources_released $true "$Context.all_resources_released"
}

function Assert-MondrianCleanOwnerClosure {
    param(
        [Parameter(Mandatory = $true)]$Container,
        [Parameter(Mandatory = $true)][string]$Context,
        [Parameter(Mandatory = $true)][int]$ExpectedPreviewOwners,
        [Parameter(Mandatory = $true)][bool]$GpuRequired
    )

    Assert-Fields $Container @("owner_closure") $Context
    $closure = $Container.owner_closure
    Assert-Fields $closure @(
        "schema_version", "shared_deadline_budget_ms", "preview_owner_count",
        "gpu_owner_required", "previews", "gpu", "app", "all_resources_released"
    ) "$Context.owner_closure"
    if ([uint64]$closure.schema_version -ne 1 -or
        [uint64]$closure.shared_deadline_budget_ms -eq 0 -or
        [int]$closure.preview_owner_count -ne $ExpectedPreviewOwners) {
        throw "$Context has an unknown schema, deadline, or Preview inventory"
    }
    Assert-Bool $closure.gpu_owner_required $GpuRequired "$Context.owner_closure.gpu_owner_required"
    $previews = @($closure.previews)
    if ($previews.Count -ne $ExpectedPreviewOwners) {
        throw "$Context Preview owner inventory is incomplete"
    }
    for ($index = 0; $index -lt $previews.Count; $index++) {
        Assert-CleanPreview $previews[$index] "$Context.owner_closure.previews[$index]"
        if ($previews[$index].owner_slot -ne $index) {
            throw "$Context Preview owner slots are missing, duplicated, or reordered"
        }
    }
    if ($GpuRequired) {
        Assert-CleanGpu $closure.gpu "$Context.owner_closure.gpu"
    }
    else {
        Assert-Null $closure.gpu "$Context.owner_closure.gpu"
    }
    Assert-CleanApp $closure.app "$Context.owner_closure.app"
    Assert-Bool $closure.all_resources_released $true "$Context.owner_closure.all_resources_released"
}

function Assert-MondrianIdenticalOwnerClosures {
    param(
        [Parameter(Mandatory = $true)]$Containers,
        [Parameter(Mandatory = $true)][string]$Context
    )

    $rows = @($Containers)
    if ($rows.Count -eq 0) {
        throw "$Context contains no owner-closure receipts"
    }
    $expected = $null
    foreach ($row in $rows) {
        Assert-Fields $row @("owner_closure") $Context
        $candidate = $row.owner_closure | ConvertTo-Json -Depth 40 -Compress
        if ($null -eq $expected) {
            $expected = $candidate
        }
        elseif ($candidate -ne $expected) {
            throw "$Context does not carry one identical owner-closure receipt"
        }
    }
}

function Assert-MondrianPerfCases {
    param($Cases, [string]$Scenario)
    $expected = switch ($Scenario) {
        "project" { @("project.create_new_project", "project.open_existing", "project.save_existing") }
        "app_ui_scale" { @("app_ui.root_build_large_project", "app_ui.refresh_large_project",
            "app_ui.resize_loop", "app_ui.paint_large_project", "app_ui.sustained_playback_refresh",
            "app_ui.preview_diagnostics_probe", "app_ui.preview_playback_refresh") }
        "preview_media_decode_cache" { @("preview_media.first_frame_ready", "preview_media.cached_frame_refresh",
            "preview_media.persistent_cache_verified_hit", "preview_media.active_scrub_ready_window",
            "preview_media.random_access_still_ready_window", "preview_media.gpu_candidate_ready") }
        "preview_media_continuous_playback" { @("preview_media.continuous_playback_readiness",
            "preview_media.playback_gpu_candidate_ready") }
        default { throw "Unknown default performance scenario '$Scenario'" }
    }
    $rows = @($Cases)
    $names = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($row in $rows) {
        Assert-Fields $row @("case", "iterations", "samples_ms", "avg_ms", "max_ms", "threshold_ms", "passed") $Scenario
        if ($row.case -isnot [string] -or $row.case -cnotin $expected -or !$names.Add($row.case)) {
            throw "$Scenario has an unknown or duplicated case"
        }
        Assert-Bool $row.passed $true "$Scenario.$($row.case).passed"
        if ($row.samples_ms -isnot [array] -or $row.iterations -eq 0 -or
            $row.samples_ms.Count -ne $row.iterations -or $row.threshold_ms -eq 0) {
            throw "$Scenario.$($row.case) has an incomplete measurement inventory"
        }
        $sum = [bigint]0
        $maximum = [bigint]0
        foreach ($sample in $row.samples_ms) {
            Assert-UnsignedInteger $sample "$Scenario.$($row.case).samples_ms"
            $sum += $sample
            if ($sample -gt $maximum) { $maximum = $sample }
        }
        if ($row.max_ms -ne $maximum -or $row.avg_ms -ne [bigint]::Divide($sum, [bigint]$row.iterations) -or
            $maximum -gt $row.threshold_ms) {
            throw "$Scenario.$($row.case) has contradictory timing or passing evidence"
        }
    }
    if ($names.Count -ne $expected.Count) { throw "$Scenario is missing measured cases" }
}

function Assert-ExactReceiptFields($Value, [string[]]$Names, [string]$Context) {
    if ($Value -isnot [pscustomobject]) { throw "$Context must be an object" }
    $actual = @($Value.PSObject.Properties.Name)
    if ($actual.Count -ne $Names.Count) { throw "$Context has missing or extra receipt fields" }
    foreach ($name in $Names) { if ($actual -cnotcontains $name) { throw "$Context omits exact $name" } }
}

function Read-CanonicalOwnerLeaf($Leaf, [string]$Context, [string]$JsonField = 'json') {
    Assert-ExactReceiptFields $Leaf @($JsonField, 'sha256') $Context
    $json = $Leaf.$JsonField
    if ($json -isnot [string] -or $json.Length -gt 2097152 -or $Leaf.sha256 -isnot [string]) { throw "$Context exceeds its bounded JSON shape" }
    $digest = [Convert]::ToHexStringLower([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($json)))
    if ($digest -cne $Leaf.sha256) { throw "$Context raw receipt hash mismatch" }
    return ($json | ConvertFrom-Json -Depth 80 -ErrorAction Stop)
}

function Assert-MondrianCanonicalAppClosure($Receipt) {
    $app = Read-CanonicalOwnerLeaf $Receipt 'Phase App' 'canonical_json'
    Assert-ExactReceiptFields $app @('schema_version','app_owner_consumed','project','reference_output','export','export_terminal_snapshot','audio','audio_source_cache','workers') 'Canonical App'
    Assert-UnsignedInteger $app.schema_version 'Canonical App schema'
    if ($app.schema_version -ne 1) { throw 'Unknown canonical App schema' }
    Assert-Bool $app.app_owner_consumed $true 'App consumed'
    Assert-ExactReceiptFields $app.project @('session_was_open','pending_close_was_active','authoring_session_released','pending_close_released','runtime_lease_released','retired_library_generations_remaining','lifecycle_failure') 'Canonical project'
    foreach ($field in @('session_was_open','pending_close_was_active')) { Assert-BoolType $app.project.$field "App project $field" }
    foreach ($field in @('authoring_session_released','pending_close_released','runtime_lease_released')) { Assert-Bool $app.project.$field $true "App project $field" }
    Assert-Zero $app.project.retired_library_generations_remaining 'App retired libraries'
    Assert-Null $app.project.lifecycle_failure 'App project close failure'
    $projection = [pscustomobject]@{
        reference_output = Read-CanonicalOwnerLeaf $app.reference_output 'Reference owner'
        reference_output_resources_released = $true
        export = Read-CanonicalOwnerLeaf $app.export 'Export owner'
        export_terminal_snapshot = Read-CanonicalOwnerLeaf $app.export_terminal_snapshot 'Export terminal'
        export_resources_released = $true
    }
    Assert-CleanReferenceOutput $projection 'Phase App'
    Assert-CleanExport $projection 'Phase App'
    $audio = Read-CanonicalOwnerLeaf $app.audio 'Audio owner'
    $audio | Add-Member all_workers_terminated $true
    $audio.output | Add-Member all_workers_terminated $true
    Assert-CleanAudio $audio 'Phase Audio'
    Assert-ExactReceiptFields $app.audio_source_cache @('strong_references_before_consumption','strong_references_remaining','cache') 'App source cache'
    $source = [pscustomobject]@{
        strong_references_before_consumption = $app.audio_source_cache.strong_references_before_consumption
        strong_references_remaining = $app.audio_source_cache.strong_references_remaining
        cache = Read-CanonicalOwnerLeaf $app.audio_source_cache.cache 'App raw source cache'
        all_resources_released = $true
    }
    Assert-CleanAudioSource $source 'Phase App source'
    $domains = @('execution_memory_observer','project_persistence','audio_idle_warmup','media_import','media_asset_mutation','visual_tracking','proxy_generation')
    Assert-ExactReceiptFields $app.workers $domains 'App workers'
    foreach ($domain in $domains) {
        $worker = $app.workers.$domain
        Assert-ExactReceiptFields $worker @('requested_workers','startup_attempted','started_workers','terminated_workers','panicked_workers','timed_out_workers','detached_workers','unexpected_worker_exits','queued_work_remaining','running_work_remaining','owned_resources_remaining','cumulative_failures') "App $domain"
        $worker | Add-Member domain $domain
        $worker | Add-Member lifecycle_closed $true
        Assert-CleanWorker $worker "Phase App $domain"
    }
}

function Assert-MondrianRawGpuClosure($Gpu) {
    $copy = $Gpu | ConvertTo-Json -Depth 30 -Compress | ConvertFrom-Json
    $copy | Add-Member all_resources_released $true
    Assert-CleanGpu $copy 'Phase GPU'
}
Export-ModuleMember -Function @(
    "Assert-MondrianCanonicalAppClosure", "Assert-MondrianRawGpuClosure", "Assert-ExactReceiptFields", "Read-CanonicalOwnerLeaf",
    "Assert-CleanAudioSourceCache",
    "Assert-CleanPreviewCallbacks",
    "Assert-MondrianPerfCases",
    "Assert-MondrianCleanOwnerClosure",
    "Assert-MondrianIdenticalOwnerClosures"
)
