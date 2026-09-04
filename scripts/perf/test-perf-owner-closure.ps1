param(
    [string]$ReportPath = "target/perf/col047-project.jsonl"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$ReportPath = if ([IO.Path]::IsPathRooted($ReportPath)) {
    [IO.Path]::GetFullPath($ReportPath)
}
else {
    [IO.Path]::GetFullPath((Join-Path $repositoryRoot $ReportPath))
}
Import-Module (Join-Path $PSScriptRoot "perf-owner-closure.psm1") -Force

function Copy-JsonValue {
    param([Parameter(Mandatory = $true)]$Value)
    return ($Value | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json)
}

function Assert-Rejected {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Probe,
        [Parameter(Mandatory = $true)][string]$Context
    )
    try {
        & $Probe
    }
    catch {
        return
    }
    throw "$Context was accepted"
}

$rows = @()
foreach ($line in Get-Content -LiteralPath $ReportPath) {
    $value = $line | ConvertFrom-Json -ErrorAction Stop
    if ($value -is [array]) {
        $rows += @($value)
    }
    else {
        $rows += $value
    }
}
if ($rows.Count -ne 3) {
    throw "Owner-closure regression fixture must contain the exact three Project rows"
}

foreach ($row in $rows) {
    Assert-MondrianCleanOwnerClosure `
        -Container $row `
        -Context "valid Project fixture '$($row.case)'" `
        -ExpectedPreviewOwners 0 `
        -GpuRequired $false
}
Assert-MondrianIdenticalOwnerClosures -Containers $rows -Context "valid Project fixture"

# Synthetic schema probes, not physical GPU qualification evidence.
$gpuFixture = Copy-JsonValue $rows[0]
$gpuFixture.owner_closure.gpu_owner_required = $true
$gpuFixture.owner_closure.gpu = [pscustomobject]@{
    worker_started = $true
    worker_terminated = $true
    worker_panicked = $false
    timed_out = $false
    retirement_requested = $true
    retirement_handoff_accepted = $true
    retirement_completed = $true
    generation_terminal_kind = $null
    renderer_retirement = [pscustomobject]@{
        cpu_yuv_upload = "returned"
        native_device_removed = $false
    }
    all_resources_released = $true
}
Assert-MondrianCleanOwnerClosure $gpuFixture "valid GPU schema fixture" 0 $true
foreach ($case in @("missing", "null", "panic", "native_removed", "unknown_exit", "aggregate")) {
    $tamperedGpu = Copy-JsonValue $gpuFixture
    $gpu = $tamperedGpu.owner_closure.gpu
    switch ($case) {
        "missing" { $gpu.PSObject.Properties.Remove("renderer_retirement") }
        "null" { $gpu.renderer_retirement = $null }
        "panic" { $gpu.renderer_retirement.cpu_yuv_upload = "panicked" }
        "native_removed" { $gpu.renderer_retirement.native_device_removed = $true }
        "unknown_exit" { $gpu.renderer_retirement.cpu_yuv_upload = "assumed_idle" }
        "aggregate" { $gpu.all_resources_released = $false }
    }
    Assert-Rejected {
        Assert-MondrianCleanOwnerClosure $tamperedGpu "GPU receipt $case" 0 $true
    } "GPU receipt $case"
}

$booleanOnly = [pscustomobject]@{
    owner_closure = [pscustomobject]@{
        schema_version = 1
        shared_deadline_budget_ms = 30000
        preview_owner_count = 0
        gpu_owner_required = $false
        previews = @()
        gpu = $null
        app = [pscustomobject]@{ all_resources_released = $true }
        all_resources_released = $true
    }
}
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $booleanOnly "boolean-only substitute" 0 $false
} "boolean-only owner-closure substitute"

$missingLeaf = Copy-JsonValue $rows[0]
$missingLeaf.owner_closure.app.audio.PSObject.Properties.Remove("output")
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $missingLeaf "missing leaf" 0 $false
} "owner closure with a missing Audio output receipt"

$contradictoryAggregate = Copy-JsonValue $rows[0]
$contradictoryAggregate.owner_closure.app.audio.all_workers_terminated = $false
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $contradictoryAggregate "contradictory aggregate" 0 $false
} "owner closure with a contradictory Audio aggregate"

$duplicateDomain = Copy-JsonValue $rows[0]
$duplicateDomain.owner_closure.app.workers[1].domain =
    $duplicateDomain.owner_closure.app.workers[0].domain
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $duplicateDomain "duplicate worker domain" 0 $false
} "owner closure with a duplicated worker domain"

$differentReceipts = @($rows | ForEach-Object { Copy-JsonValue $_ })
$differentReceipts[2].owner_closure.shared_deadline_budget_ms++
Assert-Rejected {
    Assert-MondrianIdenticalOwnerClosures $differentReceipts "different Project receipts"
} "Project rows with different owner-closure receipts"

Assert-MondrianPerfCases $rows "project"

foreach ($invalid in @($null, $false, "0", -1, 0.5)) {
    $bad = Copy-JsonValue $rows[0]
    $bad.owner_closure.app.audio.render_workers_started = $invalid
    $bad.owner_closure.app.audio.render_workers_terminated = $invalid
    Assert-Rejected {
        Assert-MondrianCleanOwnerClosure $bad "invalid counter type" 0 $false
    } "invalid numeric counter '$invalid'"
}
$bad = Copy-JsonValue $rows[0]
$bad.owner_closure.app.workers[0].startup_attempted = $true
$bad.owner_closure.app.workers[0].requested_workers = 1
$bad.owner_closure.app.workers[0].started_workers = 0
$bad.owner_closure.app.workers[0].terminated_workers = 0
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $bad "startup failure" 0 $false
} "unstarted required worker"

foreach ($leaf in @("required", "spawned", "joined")) {
    $bad = Copy-JsonValue $rows[0]
    $bad.owner_closure.app.reference_output.session.coordinator.$leaf = $null
    Assert-Rejected {
        Assert-MondrianCleanOwnerClosure $bad "null coordinator boolean" 0 $false
    } "null coordinator.$leaf"
}
$bad = Copy-JsonValue $rows[0]
$bad.owner_closure.app.reference_output.session.coordinator.required = $false
$bad.owner_closure.app.reference_output.session.coordinator.spawned = $true
$bad.owner_closure.app.reference_output.session.coordinator.joined = $false
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $bad "unjoined coordinator" 0 $false
} "unjoined optional coordinator"

$bad = Copy-JsonValue $rows[0]
$bad.owner_closure.app.export_terminal_snapshot.audio_source_owners_started += 9
$bad.owner_closure.app.export_terminal_snapshot.audio_source_owners_closed += 9
Assert-Rejected {
    Assert-MondrianCleanOwnerClosure $bad "contradictory export receipts" 0 $false
} "mismatched terminal export counters"

foreach ($invalid in @($false, "false", $null)) {
    $badCases = @($rows | ForEach-Object { Copy-JsonValue $_ })
    $badCases[0].passed = $invalid
    Assert-Rejected { Assert-MondrianPerfCases $badCases "project" } "non-passing typed verdict"
}
$badCases = @($rows | ForEach-Object { Copy-JsonValue $_ })
$badCases[0].max_ms++
Assert-Rejected { Assert-MondrianPerfCases $badCases "project" } "contradictory measurement"
Assert-Rejected {
    Assert-MondrianPerfCases @([pscustomobject]@{ passed = $true }) "app_ui_scale"
} "anonymous passing case"
Assert-Rejected { Assert-MondrianPerfCases @($rows[0], $rows[0], $rows[2]) "project" } "duplicated case"

# Separate actual owners may have identical terminal facts, but never the same slot.
$preview = [pscustomobject]@{
    owner_slot = 0; schema_version = 3; workers_started = 2; workers_terminated = 2
    visual_dependency_worker = 'terminated'
    worker_panic_payloads_abandoned = 0
    worker_panics = 0; current_thread_detachments = 0; unverified_async_reaps = 0
    worker_timeouts = 0; worker_deadline_detachments = 0; render_cache_schema_version = 1
    render_cache_required = $true; render_cache_start_failed = $false
    render_cache_worker = [pscustomobject]@{
        worker_started = $true; worker_terminated = $true; worker_panicked = $false
        current_thread_skipped = $false; timed_out = $false; detached = $false
        all_workers_terminated = $true
    }
    render_cache_aggregate_outcome = "terminated"; all_resources_released = $true
}
$good = Copy-JsonValue $rows[0]
$second = Copy-JsonValue $preview
$second.owner_slot = 1
$good.owner_closure.preview_owner_count = 2
$good.owner_closure.previews = @($preview, $second)
Assert-MondrianCleanOwnerClosure $good "two distinct Preview owners" 2 $false
foreach ($abandoned in @(1, "0", $null)) {
    $bad = Copy-JsonValue $good
    $bad.owner_closure.previews[0].worker_panic_payloads_abandoned = $abandoned
    Assert-Rejected { Assert-MondrianCleanOwnerClosure $bad "opaque worker panic" 2 $false } "opaque worker panic counter $abandoned"
}
$historical = Copy-JsonValue $good
$historical.owner_closure.previews[0].PSObject.Properties.Remove("worker_panic_payloads_abandoned")
Assert-MondrianCleanOwnerClosure $historical "healthy schema-3 inventory before additive panic taxonomy" 2 $false
foreach ($outcome in @($null, 'not-started', 'panicked', 'timed-out-detached', 'current-thread-skipped')) {
    $bad = Copy-JsonValue $good
    $bad.owner_closure.previews[0].visual_dependency_worker = $outcome
    Assert-Rejected { Assert-MondrianCleanOwnerClosure $bad "invalid dependency observer" 2 $false } "dependency observer $outcome"
}
$bad = Copy-JsonValue $good
$bad.owner_closure.previews[0].schema_version = 2
Assert-Rejected { Assert-MondrianCleanOwnerClosure $bad "old Preview inventory" 2 $false } "old Preview schema"
$good.owner_closure.previews = @($preview, $preview)
Assert-Rejected { Assert-MondrianCleanOwnerClosure $good "duplicate Preview" 2 $false } "duplicate Preview slot"

$probeRoot = Join-Path $repositoryRoot ("target/perf/validator-mutex-" + [guid]::NewGuid().ToString("N"))
$originalWorkerOverride = $env:MONDRIAN_PREVIEW_DEMUX_WORKER_PATH
$originalIncremental = $env:CARGO_INCREMENTAL
try {
    $env:MONDRIAN_PREVIEW_DEMUX_WORKER_PATH = "intentional-invalid-worker"
    $rejected = $false
    try { & (Join-Path $PSScriptRoot "run-perf-suite.ps1") -OutputDir $probeRoot }
    catch { $rejected = $_.Exception.Message -like "Unset MONDRIAN_PREVIEW_DEMUX_WORKER_PATH*" }
    if (!$rejected) { throw "Suite did not reject an overridden worker before building" }
    # A same-thread WaitOne would recursively acquire even a leaked mutex.
    # A separate process proves failure cleanup without running any Cargo work.
    & (Get-Process -Id $PID).Path -NoProfile -Command '
        $mutex = [Threading.Mutex]::new($false, "Mondrian.PerfSuite.v1")
        if (!$mutex.WaitOne(0)) { exit 1 }
        $mutex.ReleaseMutex()
        $mutex.Dispose()
    '
    if ($LASTEXITCODE -ne 0) { throw "Suite prerequisite failure leaked mutex ownership" }
}
finally {
    $env:MONDRIAN_PREVIEW_DEMUX_WORKER_PATH = $originalWorkerOverride
    $env:CARGO_INCREMENTAL = $originalIncremental
    # Only these newly created, empty test directories are removed, never a
    # recursive/glob target or the caller's report directory.
    $probeLogs = Join-Path $probeRoot "cargo-logs"
    if (Test-Path -LiteralPath $probeLogs) { Remove-Item -LiteralPath $probeLogs }
    if (Test-Path -LiteralPath $probeRoot) { Remove-Item -LiteralPath $probeRoot }
}

Write-Host "Performance owner-closure, measured-case, and suite prerequisite regressions passed."
