Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'phase-owner-closure.psm1') -Force
$fixtureRoot = Join-Path $PSScriptRoot '../../tests/validation/fixtures'
$script:caseCount = 0
function Seal-Report($Raw) {
    $json = $Raw | ConvertTo-Json -Depth 80 -Compress
    [pscustomobject]@{phase_id='fixture-phase';report_path='fixture-owner.json';canonical_json=$json;sha256=[Convert]::ToHexStringLower([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($json)))}
}
function Assert-Rejected($Raw, [string]$Kind, [string]$Context) {
    $receipt = Seal-Report $Raw
    $rejected = $false
    try { Assert-MondrianPhaseOwnerReceipt $receipt fixture-run fixture-phase 0 $Kind } catch { $rejected = $true }
    if (-not $rejected) { throw "Accepted rehashed $Context" }
    $script:caseCount++
}
foreach ($kind in @('continuous_export','playback_reference','concurrent_recovery')) {
    $app = Get-Content (Join-Path $fixtureRoot 'app-shutdown-closure.json') -Raw | ConvertFrom-Json
    $export = ($app.canonical_json | ConvertFrom-Json).export.json | ConvertFrom-Json
    $owners = if ($kind -ceq 'continuous_export') { [pscustomobject]@{AppOnly=$app} } else { [pscustomobject]@{Realtime=(Get-Content (Join-Path $fixtureRoot 'phase-owner-realtime.json') -Raw | ConvertFrom-Json)} }
    $verifier = if ($kind -ceq 'playback_reference') { $null } else { [pscustomobject]@{
        workers_started=2;workers_joined=2;workers_remaining=0;workers_abandoned=0;cancellation_requested=$true;deadline_exceeded=$false;failure=$null
        terminal_publications=[pscustomobject]@{workers_started=2;workers_joined=2;workers_remaining=0;workers_abandoned=0;failure=$null;last_worker=[pscustomobject]@{
            job_id='11111111-1111-4111-8111-111111111111';output_path='fixture-export.mp4';thread_joined=$true;evidence_persisted=$true;panic=$null;failure=$null
            terminal_snapshot_json=([pscustomobject]@{schema_version=1;job=[pscustomobject]@{id='11111111-1111-4111-8111-111111111111';generation=2;output_path='fixture-export.mp4';output_policy='create_only';preset_name='fixture';status=@{status='completed'};progress=@{};publication=@{};diagnostics=@{};created_at='2026-09-06T00:00:00Z';started_at='2026-09-06T00:00:00Z';completed_at='2026-09-06T00:00:01Z';terminal_evidence=$null;artifact_publication=$null;executed=$true}} | ConvertTo-Json -Depth 40 -Compress)
        }}
        last_worker=[pscustomobject]@{job_id='11111111-1111-4111-8111-111111111111';output_path='fixture-export.mp4';thread_joined=$true;evidence_persisted=$true;verification_failure=$null;panic=$null;failure=$null
            native_cleanup=[pscustomobject]@{native_exit_observed=$true;kill_error=$null;wait_error=$null;deadline_exceeded=$false;stdin_error=$null;stdout_error=$null;stderr_error=$null}}
    } }
    $original = [pscustomobject]@{schema_version=2;run_id='fixture-run';phase_id='fixture-phase';ordinal=0;terminal=[pscustomobject]@{phase_kind=$kind;closure=[pscustomobject]@{status='completed';playback_workers_terminated=$true;supervised_child_processes_remaining=0;export=$export};owners=$owners;export_verifier=$verifier;failures=@()}}
    Assert-MondrianPhaseOwnerReceipt (Seal-Report $original) fixture-run fixture-phase 0 $kind
    $ancillary = $original | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
    $ancillary | Add-Member ancillary_program_sha256 ('a' * 64)
    $ancillary | Add-Member ancillary_export_artifacts @()
    $ancillary | Add-Member wire_journals @()
    Assert-MondrianPhaseOwnerReceipt (Seal-Report $ancillary) fixture-run fixture-phase 0 $kind
    foreach ($mutation in @('digest-null','digest-uppercase','exports-missing','exports-null','journals-missing','journals-null','journal-hash','journal-duplicate','export-extra','export-duplicate')) {
        $changed = $ancillary | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
        switch ($mutation) {
            'digest-null' {$changed.ancillary_program_sha256=$null}
            'digest-uppercase' {$changed.ancillary_program_sha256='A' * 64}
            'exports-missing' {$changed.PSObject.Properties.Remove('ancillary_export_artifacts')}
            'exports-null' {$changed.ancillary_export_artifacts=$null}
            'journals-missing' {$changed.PSObject.Properties.Remove('wire_journals')}
            'journals-null' {$changed.wire_journals=$null}
            'journal-hash' {$changed.wire_journals=@(@{path='journal.jsonl';sha256='bad'})}
            'journal-duplicate' {$changed.wire_journals=@(@{path='journal.jsonl';sha256=('b'*64)},@{path='journal.jsonl';sha256=('b'*64)})}
            'export-extra' {$changed.ancillary_export_artifacts=@(@{artifact_id='endurance-export-11111111-1111-4111-8111-111111111111';verification_path='verify.json';verification_sha256=('b'*64);invented=$true})}
            'export-duplicate' {$entry=@{artifact_id='endurance-export-11111111-1111-4111-8111-111111111111';verification_path='verify.json';verification_sha256=('b'*64)}; $changed.ancillary_export_artifacts=@($entry,$entry)}
        }
        Assert-Rejected $changed $kind "ancillary $mutation"
    }
    $bmx = $original | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
    $bmx.terminal | Add-Member bmx_runtime ([pscustomobject]@{commands_released=$true;namespace_owned=$true;namespace_validation_error=$null;namespace_restore_error=$null;namespace_remove_error=$null;file_leases_released=$true;deadline_exceeded=$false;outstanding_owner_error=$null})
    Assert-MondrianPhaseOwnerReceipt (Seal-Report $bmx) fixture-run fixture-phase 0 $kind
    foreach ($mutation in @('commands_released','namespace_owned','file_leases_released','deadline_exceeded','namespace_validation_error','namespace_restore_error','namespace_remove_error','outstanding_owner_error','extra','missing','wrong-type')) {
        $changed = $bmx | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
        $closure = $changed.terminal.bmx_runtime
        switch ($mutation) {
            'extra' {$closure | Add-Member invented $true}
            'missing' {$closure.PSObject.Properties.Remove('commands_released')}
            'wrong-type' {$closure.commands_released='true'}
            'deadline_exceeded' {$closure.deadline_exceeded=$true}
            {$_ -in @('commands_released','namespace_owned','file_leases_released')} {$closure.$mutation=$false}
            default {$closure.$mutation='dirty'}
        }
        Assert-Rejected $changed $kind "BMX $mutation"
    }
    $missingVerifier = $original | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
    $missingVerifier.terminal.PSObject.Properties.Remove('export_verifier')
    Assert-Rejected $missingVerifier $kind 'missing verifier owner field'
    if ($null -ne $verifier) {
        foreach ($mutation in @('metadata-missing','metadata-unjoined','metadata-remaining','metadata-abandoned','metadata-count-string','metadata-failure','metadata-last-missing','metadata-thread','metadata-persist','metadata-panic','metadata-worker-failure','metadata-json-missing','metadata-json-malformed','metadata-json-oversize','metadata-job','metadata-path','metadata-status','metadata-field','metadata-schema','metadata-extra','missing','unjoined','remaining','abandoned','count-string','cancel-string','deadline','failure','last-missing','job-empty','path-empty','thread','persist','verification','panic','worker-failure','native-missing','native-exit','native-deadline','native-error','native-field','native-extra')) {
            $raw = $original | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
            $v = $raw.terminal.export_verifier
            switch ($mutation) {
                'metadata-missing' {$v.terminal_publications=$null}
                'metadata-unjoined' {$v.terminal_publications.workers_joined=1}
                'metadata-remaining' {$v.terminal_publications.workers_remaining=1}
                'metadata-abandoned' {$v.terminal_publications.workers_abandoned=1}
                'metadata-count-string' {$v.terminal_publications.workers_started='2'}
                'metadata-failure' {$v.terminal_publications.failure='dirty'}
                'metadata-last-missing' {$v.terminal_publications.last_worker=$null}
                'metadata-thread' {$v.terminal_publications.last_worker.thread_joined=$false}
                'metadata-persist' {$v.terminal_publications.last_worker.evidence_persisted=$false}
                'metadata-panic' {$v.terminal_publications.last_worker.panic='panic'}
                'metadata-worker-failure' {$v.terminal_publications.last_worker.failure='dirty'}
                'metadata-json-missing' {$v.terminal_publications.last_worker.terminal_snapshot_json=$null}
                'metadata-json-malformed' {$v.terminal_publications.last_worker.terminal_snapshot_json='{'}
                'metadata-json-oversize' {$v.terminal_publications.last_worker.terminal_snapshot_json=' ' * 2097153}
                'metadata-job' {$v.terminal_publications.last_worker.job_id='22222222-2222-4222-8222-222222222222'}
                'metadata-path' {$v.terminal_publications.last_worker.output_path='substitute.mp4'}
                'metadata-status' {$v.terminal_publications.last_worker.terminal_snapshot_json=$v.terminal_publications.last_worker.terminal_snapshot_json.Replace('"status":"completed"','"status":"pending"')}
                'metadata-field' {$job=$v.terminal_publications.last_worker.terminal_snapshot_json|ConvertFrom-Json; $job.job.PSObject.Properties.Remove('diagnostics'); $v.terminal_publications.last_worker.terminal_snapshot_json=$job|ConvertTo-Json -Depth 40 -Compress}
                'metadata-schema' {$v.terminal_publications.last_worker.terminal_snapshot_json=$v.terminal_publications.last_worker.terminal_snapshot_json.Replace('"schema_version":1','"schema_version":2')}
                'metadata-extra' {$v.terminal_publications|Add-Member invented_closed $true}
                'missing' {$raw.terminal.export_verifier=$null}
                'unjoined' {$v.workers_joined=1}
                'remaining' {$v.workers_remaining=1}
                'abandoned' {$v.workers_abandoned=1}
                'count-string' {$v.workers_started='2'}
                'cancel-string' {$v.cancellation_requested='true'}
                'deadline' {$v.deadline_exceeded=$true}
                'failure' {$v.failure='dirty'}
                'last-missing' {$v.last_worker=$null}
                'job-empty' {$v.last_worker.job_id=[guid]::Empty.ToString()}
                'path-empty' {$v.last_worker.output_path=''}
                'thread' {$v.last_worker.thread_joined=$false}
                'persist' {$v.last_worker.evidence_persisted=$false}
                'verification' {$v.last_worker.verification_failure=@{snapshot_removed=$false}}
                'panic' {$v.last_worker.panic='panic'}
                'worker-failure' {$v.last_worker.failure='dirty'}
                'native-missing' {$v.last_worker.native_cleanup=$null}
                'native-exit' {$v.last_worker.native_cleanup.native_exit_observed=$false}
                'native-deadline' {$v.last_worker.native_cleanup.deadline_exceeded=$true}
                'native-error' {$v.last_worker.native_cleanup.stderr_error='dirty'}
                'native-field' {$v.last_worker.native_cleanup.PSObject.Properties.Remove('kill_error')}
                'native-extra' {$v.last_worker.native_cleanup | Add-Member invented_closed $true}
            }
            Assert-Rejected $raw $kind "verifier $mutation"
        }
    }
    foreach ($mutation in @('schema','ordinal','phase','run','extra','missing','failed','bool-string','child-string','failures','export','raw-app','app-hash','app-worker','app-extra','app-export-join')) {
        $raw = $original | ConvertTo-Json -Depth 80 -Compress | ConvertFrom-Json
        switch ($mutation) {
            'schema' {$raw.schema_version='1'}
            'ordinal' {$raw.ordinal=1}
            'phase' {$raw.phase_id='other-phase'}
            'run' {$raw.run_id='other-run'}
            'extra' {$raw | Add-Member extra $false}
            'missing' {$raw.PSObject.Properties.Remove('terminal')}
            'failed' {$raw.terminal.closure.status='failed'}
            'bool-string' {$raw.terminal.closure.playback_workers_terminated='true'}
            'child-string' {$raw.terminal.closure.supervised_child_processes_remaining='0'}
            'failures' {$raw.terminal.failures=@('dirty')}
            'export' {$raw.terminal.closure.export.worker_terminated=$false}
            default {
                $appReceipt = if ($kind -ceq 'continuous_export') {$raw.terminal.owners.AppOnly} else {$raw.terminal.owners.Realtime.app}
                if ($mutation -ceq 'app-hash') {$appReceipt.sha256='0'*64}
                else {
                    $body = $appReceipt.canonical_json | ConvertFrom-Json
                    switch ($mutation) {
                        'raw-app' {$body.app_owner_consumed=$false}
                        'app-worker' {$body.workers.project_persistence.detached_workers=1}
                        'app-extra' {$body | Add-Member invented_resource_closed $true}
                        'app-export-join' {
                            $leaf = $body.export.json | ConvertFrom-Json
                            $leaf.worker_terminated=$false
                            $body.export.json=$leaf|ConvertTo-Json -Depth 80 -Compress
                            $body.export.sha256=[Convert]::ToHexStringLower([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($body.export.json)))
                        }
                    }
                    $appReceipt.canonical_json=$body|ConvertTo-Json -Depth 80 -Compress
                    $appReceipt.sha256=[Convert]::ToHexStringLower([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($appReceipt.canonical_json)))
                }
            }
        }
        Assert-Rejected $raw $kind "$kind $mutation"
    }
    if ($kind -cne 'continuous_export') {
        foreach ($mutation in @('gpu-wake','gpu-join','preview','waveform','snapshot')) {
            $raw=$original|ConvertTo-Json -Depth 80 -Compress|ConvertFrom-Json
            switch($mutation) {
                'gpu-wake' {$raw.terminal.owners.Realtime.gpu.wake_callbacks.registrations_retained=1}
                'gpu-join' {$raw.terminal.owners.Realtime.gpu.worker_shutdown='timed_out_detached'}
                'preview' {$raw.terminal.owners.Realtime.preview.workers_terminated=0}
                'waveform' {$raw.terminal.owners.Realtime.waveform.worker_owner_abandonments=1}
                'snapshot' {$raw.terminal.owners.Realtime.terminal_owner_snapshot.owned_resource_units=1}
            }
            Assert-Rejected $raw $kind "$kind $mutation"
        }
    }
}
Write-Output "PHASE_OWNER_REPLAY: three baselines passed; $script:caseCount rehashed mutations rejected"
