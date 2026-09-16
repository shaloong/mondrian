# Independent typed replay of Window Runtime and Host owner receipts.
# Shape and integer widths mirror Rust owner schemas; this is not hardware evidence.
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot '../perf/perf-owner-closure.psm1') -Force

$script:WindowOwnerShapes = @{
    GpuWakeCounters = @{
        native_wake_failures = 'u64'
        wake_registration_rejections = 'u64'
    }
    Runtime = @{
        configured_worker_threads = 'u64'
        runtime_handoff_completed = 'bool'
        shutdown_signal_delivered = 'bool'
        supervisor = 'string'
    }
    Host = @{
        auxiliary = 'object'
        preview = 'object'
    }
    Auxiliary = @{
        catalog = 'object'
        thumbnails = 'object'
        waveform = 'object'
    }
    Preview = @{
        current_thread_detachments = 'u32'
        schema_version = 'u32'
        timeline_render_cache = 'object'
        unverified_async_reaps = 'u32'
        visual_dependency_worker = 'string'
        work_callbacks = 'object'
        worker_deadline_detachments = 'u32'
        worker_panic_payloads_abandoned = 'u32'
        worker_panics = 'u32'
        worker_timeouts = 'u32'
        workers_started = 'u32'
        workers_terminated = 'u32'
    }
    Cache = @{
        aggregate_outcome = 'string'
        required = 'bool'
        schema_version = 'u32'
        start_failed = 'bool'
        worker = 'optional_object'
    }
    CacheWorker = @{
        current_thread_skipped = 'bool'
        detached = 'bool'
        timed_out = 'bool'
        worker_panicked = 'bool'
        worker_started = 'bool'
        worker_terminated = 'bool'
    }
    Callbacks = @{
        admission_closed = 'bool'
        deadline_met = 'bool'
        destructor_panics = 'u64'
        invocation_panics = 'u64'
        invocations_in_flight = 'u64'
        opaque_payloads_abandoned = 'u64'
        registrations_abandoned = 'u64'
        registrations_accepted = 'u64'
        registrations_released = 'u64'
        registrations_retained = 'u64'
        retirements_active = 'u64'
        schema_version = 'u32'
        shutdown_rejection = 'null'
        worker = 'string'
        worker_panics = 'u64'
        worker_start_failures = 'u64'
        worker_started = 'bool'
    }
    Waveform = @{
        awaiting_publication_before = 'u64'
        awaiting_publication_remaining = 'u64'
        deferred_requests_before = 'u64'
        deferred_requests_remaining = 'u64'
        external_source_cache_references = 'u64'
        pending_requests_before = 'u64'
        pending_requests_remaining = 'u64'
        running_requests_before = 'u64'
        running_requests_remaining = 'u64'
        schema_version = 'u32'
        source_cache = 'object'
        worker_detachments = 'u32'
        worker_failures = 'u32'
        worker_owner_abandonments = 'u32'
        worker_panics = 'u32'
        worker_start_failures = 'u32'
        worker_timeouts = 'u32'
        workers_configured = 'u32'
        workers_started = 'u32'
        workers_terminated = 'u32'
    }
    Source = @{
        child_process_termination_failures = 'u64'
        child_processes_observed = 'u64'
        child_processes_terminated = 'u64'
        decoder_resource_handles_remaining = 'u64'
        decoder_sessions_before = 'u64'
        decoder_sessions_remaining = 'u64'
        decoder_shutdown_worker_owner_abandonments = 'u64'
        decoder_shutdown_worker_panics = 'u64'
        decoder_shutdown_worker_publication_missing = 'u64'
        decoder_shutdown_worker_start_failures = 'u64'
        decoder_shutdown_workers_started = 'u64'
        decoder_shutdown_workers_terminated = 'u64'
        decoder_startup = 'object'
        external_decoder_references = 'u64'
        external_decoder_session_references = 'u64'
        external_pcm_buffer_references = 'u64'
        failure_entries_before = 'u64'
        failure_entries_remaining = 'u64'
        in_flight_decodes_before = 'u64'
        pcm_bytes_before = 'u64'
        pcm_bytes_remaining = 'u64'
        pcm_entries_before = 'u64'
        pcm_entries_remaining = 'u64'
        schema_version = 'u32'
        shutdown_coordinator_detachments = 'u64'
        shutdown_coordinator_owner_abandonments = 'u64'
        shutdown_coordinator_panics = 'u64'
        shutdown_coordinator_spawner_panics = 'u64'
        shutdown_coordinator_start_failures = 'u64'
        shutdown_coordinator_timeouts = 'u64'
        shutdown_coordinators_started = 'u64'
        shutdown_coordinators_terminated = 'u64'
        shutdown_owner_lifetime_unresolved_at_deadline = 'bool'
        shutdown_resource_facts_complete_at_deadline = 'bool'
        stderr_pump_thread_owner_abandonments = 'u64'
        stderr_pump_threads_joined = 'u64'
        stderr_pump_threads_observed = 'u64'
        stderr_pump_threads_panicked = 'u64'
        stdout_pump_thread_owner_abandonments = 'u64'
        stdout_pump_threads_joined = 'u64'
        stdout_pump_threads_observed = 'u64'
        stdout_pump_threads_panicked = 'u64'
    }
    SourceStartup = @{
        attempted = 'bool'
        canceled_before_spawn = 'u64'
        in_flight_remaining = 'u64'
        owner_abandonments = 'u64'
        panics = 'u64'
        producers_remaining = 'u64'
        publication_missing = 'u64'
        queued_remaining = 'u64'
        requests_admitted = 'u64'
        requests_claimed = 'u64'
        requests_retired = 'u64'
        required = 'bool'
        start_failures = 'u64'
        unclaimed_results_remaining = 'u64'
        unverified_native_owners = 'u64'
        workers_joined = 'u64'
        workers_started = 'u64'
    }
    Thumbnails = @{
        Ok = 'object'
    }
    Thumbnail = @{
        active_requests_remaining = 'u64'
        admission_closed = 'bool'
        awaiting_publication_remaining = 'u64'
        cached_bytes = 'u64'
        deferred_requests = 'u64'
        pending_requests = 'u64'
        publication_revoked = 'bool'
        schema_version = 'u32'
        startup_attempted = 'bool'
        transports_released = 'bool'
        worker = 'string'
        worker_start_failed = 'bool'
        worker_started = 'bool'
    }
    Catalog = @{
        admission_closed = 'bool'
        current_thread_detachments = 'u64'
        deadline_detachments = 'u64'
        initial_discovery_required = 'bool'
        panic_payloads_abandoned = 'u64'
        results_missing = 'u64'
        schema_version = 'u32'
        startup_attempts = 'u64'
        worker_panics = 'u64'
        worker_start_failures = 'u64'
        workers_joined = 'u64'
        workers_started = 'u64'
    }
}

function Assert-WindowOwnerShape($Value, [string]$Shape) {
    if ($Value -isnot [pscustomobject]) { throw "$Shape must be one object" }
    $expected = $script:WindowOwnerShapes[$Shape]
    $actual = @($Value.PSObject.Properties.Name)
    if ($actual.Count -ne $expected.Count) { throw "$Shape has incomplete or extra owner fields" }
    foreach ($field in $expected.Keys) {
        if ($field -cnotin $actual) { throw "$Shape omits exact field $field" }
        $leafValue = $Value.$field
        switch ($expected[$field]) {
            'bool' { if ($leafValue -isnot [bool]) { throw "$Shape.$field must be boolean" } }
            'string' { if ($leafValue -isnot [string]) { throw "$Shape.$field must be a string" } }
            'object' { if ($leafValue -isnot [pscustomobject]) { throw "$Shape.$field must be an object" } }
            'optional_object' { if ($null -ne $leafValue -and $leafValue -isnot [pscustomobject]) { throw "$Shape.$field must be an object or null" } }
            'null' { if ($null -ne $leafValue) { throw "$Shape.$field must be null for clean closure" } }
            default {
                if ($leafValue -isnot [int] -and $leafValue -isnot [long] -and $leafValue -isnot [uint64] -and $leafValue -isnot [bigint]) {
                    throw "$Shape.$field must be an integer"
                }
                $maximum = if ($expected[$field] -ceq 'u32') { [bigint][uint32]::MaxValue } else { [bigint][uint64]::MaxValue }
                if ([bigint]$leafValue -lt 0 -or [bigint]$leafValue -gt $maximum) { throw "$Shape.$field is outside its owner type" }
            }
        }
    }
}

function Assert-WindowZeroFields($Value, [string[]]$Fields, [string]$Context) {
    foreach ($field in $Fields) { if ($Value.$field -ne 0) { throw "$Context.$field retains a failure or owner" } }
}

function Assert-MondrianWindowOwnerClosure($Runtime, $Host) {
    Assert-WindowOwnerShape $Runtime 'Runtime'
    if (-not $Runtime.runtime_handoff_completed -or $Runtime.configured_worker_threads -ne 4 -or
        -not $Runtime.shutdown_signal_delivered -or $Runtime.supervisor -cne 'terminated') {
        throw 'Window Runtime did not start and terminate its normal supervisor'
    }
    Assert-WindowOwnerShape $Host 'Host'
    Assert-MondrianPreviewOwnerClosure $Host.preview
    $aux = $Host.auxiliary
    Assert-WindowOwnerShape $aux 'Auxiliary'
    Assert-MondrianWaveformOwnerClosure $aux.waveform
    Assert-WindowOwnerShape $aux.thumbnails 'Thumbnails'
    $thumbnail = $aux.thumbnails.Ok
    Assert-WindowOwnerShape $thumbnail 'Thumbnail'
    if ($thumbnail.schema_version -ne 1 -or -not $thumbnail.startup_attempted -or -not $thumbnail.worker_started -or
        $thumbnail.worker_start_failed -or $thumbnail.worker -cne 'terminated' -or -not $thumbnail.admission_closed -or
        -not $thumbnail.transports_released -or -not $thumbnail.publication_revoked) { throw 'Window Thumbnail did not close' }
    Assert-WindowZeroFields $thumbnail @('pending_requests', 'deferred_requests', 'cached_bytes',
        'active_requests_remaining', 'awaiting_publication_remaining') 'Thumbnail'
    $catalog = $aux.catalog
    Assert-WindowOwnerShape $catalog 'Catalog'
    if ($catalog.schema_version -ne 1 -or -not $catalog.admission_closed -or
        ($catalog.initial_discovery_required -and $catalog.startup_attempts -eq 0) -or
        $catalog.startup_attempts -ne $catalog.workers_started -or $catalog.workers_started -ne $catalog.workers_joined) {
        throw 'Window device catalog has incomplete owner inventory'
    }
    Assert-WindowZeroFields $catalog @('worker_start_failures', 'worker_panics', 'panic_payloads_abandoned',
        'deadline_detachments', 'current_thread_detachments', 'results_missing') 'Catalog'
}

function Assert-MondrianGpuWakeClosure($Receipt, [bool]$RequireNativeRegistration = $false) {
    if ($Receipt.worker_shutdown -isnot [string] -or $Receipt.worker_shutdown -cne 'terminated') {
        throw 'GPU progress worker has no exact healthy native join'
    }
    Assert-WindowOwnerShape $Receipt.wake_callbacks 'Callbacks'
    Assert-CleanPreviewCallbacks $Receipt.wake_callbacks 'GPU wake callback owners'
    if ($RequireNativeRegistration -and $Receipt.wake_callbacks.registrations_accepted -eq 0) {
        throw 'Window GPU has no registered native wake owner'
    }
    Assert-WindowOwnerShape ([pscustomobject]@{
        native_wake_failures = $Receipt.native_wake_failures
        wake_registration_rejections = $Receipt.wake_registration_rejections
    }) 'GpuWakeCounters'
    foreach ($name in @('native_wake_failures', 'wake_registration_rejections')) {
        if ($Receipt.$name -ne 0) { throw "GPU $name is not clean" }
    }
}

function Assert-MondrianPreviewOwnerClosure($preview) {
    Assert-WindowOwnerShape $preview 'Preview'
    Assert-WindowOwnerShape $preview.work_callbacks 'Callbacks'
    Assert-CleanPreviewCallbacks $preview.work_callbacks 'Window Preview callbacks'
    $cache = $preview.timeline_render_cache
    Assert-WindowOwnerShape $cache 'Cache'
    if ($cache.schema_version -ne 1 -or $cache.start_failed) { throw 'Window Preview cache startup failed' }
    if ($cache.required) {
        Assert-WindowOwnerShape $cache.worker 'CacheWorker'
        if ($cache.aggregate_outcome -cne 'terminated' -or -not $cache.worker.worker_started -or
            -not $cache.worker.worker_terminated -or $cache.worker.worker_panicked -or
            $cache.worker.current_thread_skipped -or $cache.worker.timed_out -or $cache.worker.detached) {
            throw 'Window required cache worker did not start and terminate'
        }
    } elseif ($null -ne $cache.worker -or $cache.aggregate_outcome -cne 'not_started') {
        throw 'Window disabled cache has contradictory owner inventory'
    }
    if ($preview.schema_version -ne 4 -or $preview.workers_started -ne $preview.workers_terminated -or
        $preview.visual_dependency_worker -cne 'terminated' -or
        $preview.workers_started -lt (1 + [int]$cache.required + [int]$preview.work_callbacks.worker_started)) {
        throw 'Window Preview has incomplete worker inventory'
    }
    Assert-WindowZeroFields $preview @('worker_panics', 'worker_panic_payloads_abandoned',
        'current_thread_detachments', 'unverified_async_reaps', 'worker_timeouts', 'worker_deadline_detachments') 'Preview'
}

function Assert-MondrianWaveformOwnerClosure($waveform) {
    Assert-WindowOwnerShape $waveform 'Waveform'
    if ($waveform.schema_version -ne 1 -or $waveform.workers_configured -ne 1 -or
        $waveform.workers_started -ne 1 -or $waveform.workers_terminated -ne 1) { throw 'Window Waveform inventory did not close' }
    Assert-WindowZeroFields $waveform @('worker_start_failures', 'worker_panics', 'worker_failures', 'worker_timeouts',
        'worker_detachments', 'worker_owner_abandonments', 'pending_requests_remaining', 'deferred_requests_remaining',
        'running_requests_remaining', 'awaiting_publication_remaining', 'external_source_cache_references') 'Waveform'
    Assert-WindowOwnerShape $waveform.source_cache 'Source'
    Assert-WindowOwnerShape $waveform.source_cache.decoder_startup 'SourceStartup'
    Assert-CleanAudioSourceCache $waveform.source_cache 'Window Waveform source'
}

Export-ModuleMember -Function Assert-MondrianWindowOwnerClosure, Assert-MondrianGpuWakeClosure, Assert-MondrianPreviewOwnerClosure, Assert-MondrianWaveformOwnerClosure
