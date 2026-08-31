# Commercial Endurance Qualification

Commercial endurance is a qualification Module, not a longer benchmark. It
proves that one exact release candidate can continuously play, feed physical
reference output, export, recover, and return all owned work to quiescence
without unbounded resource growth.

## Ownership

The implementation is split at existing authority boundaries:

- `mondrian-platform-core::endurance_qualification` owns the versioned profile,
  strict evidence schema, bounded chunk chain, deterministic evaluator,
  three-state verdict, and self-verifying report. It performs no filesystem or
  product execution.
- `mondrian-platform` supplies complete native `ProductProcessTree` memory
  observations. Windows Private Commit, Linux anonymous resident memory, and
  macOS physical footprint remain non-interchangeable.
- Playback owns `PlaybackEvidenceReport`; Reference Output owns scheduled
  playout/accounting/hardware-clock evidence; Export owns queue/job/publication
  evidence and explicit worker retirement.
- `mondrian-app::app::endurance_qualification` is a validation-only composition
  Adapter and serial evidence supervisor. It copies owner snapshots into the
  neutral sample contract, verifies checked-in workload bytes, buffers at most
  one profile-bounded chunk, bounds both chunk receipts and producer events,
  publishes chunks and the run manifest create-only with file fsync, and
  derives terminal closure from typed Reference Output and Export shutdown
  evidence. It does not execute workloads or reinterpret gates.
- the PowerShell verifier owns external trust anchors, link-free file closure,
  immutable-byte checks, a bounded replay process, and create-only output.

This direction prevents a validation harness from becoming a second Playback,
Reference Output, Export, or process-resource implementation.

## Commercial profile

`tests/validation/commercial-endurance-qualification.json` fixes three serial
24-hour phases:

1. playback plus physical reference output;
2. continuous, independently verified Export publication;
3. concurrent playback/reference/export with controlled recovery cycles.

The total is 72 wall-clock hours. This is intentionally distinct from the
Realtime Performance Matrix's 120-minute *Program-scale* authoring workload,
which describes Timeline extent and operation scale rather than a 120-minute
wall-clock soak.

Each profile phase binds one raw checked-in contract under
`tests/validation/endurance-workloads/`, the `mondrian-app` owner, the
`mondrian-app-endurance-capture-v1` supervisor, and producer report schema 1.
The reference-asset validator recomputes every workload file digest, so an
opaque or missing workload cannot be admitted by editing only the profile.

Each phase declares exact minimum duration, warmup, cadence, maximum sample and
per-domain Playback/Reference/Export/recovery progress gaps, native memory
backend/metric, absolute footprint, settled growth, integer least-squares
slope, domain counter budgets, queue bound, terminal quiescence, worker return,
and descendant-process reap. Missing hardware or execution is `Incomplete`;
an executed violation is `Failed`; only complete passing evidence is
`Qualified`.

## Samples and fixed-space capture

Every sample carries three monotonic instants:

- scheduled capture time;
- native probe start;
- complete snapshot publication.

The evaluator rejects missing/duplicate/out-of-order sequences, impossible
profile capacity, excessive
sample gaps or probe latency, counter regression, process-tree inventory
failure, backend/metric drift, arithmetic overflow, Reference Output imbalance,
and Export job/publication imbalance. It also requires per-sample physical
provider and external-lock facts, normalized hardware-clock gaps, and terminal
Export worker shutdown. UTC may be retained by raw producer reports for
audit, but it is never duration authority.

Samples are written in create-only bounded chunks. Each chunk hashes its exact
contents and the preceding chunk digest; the manifest fixes file name, index,
sample count, and first/last sequence. The checked-in commercial profile keeps
at most 120 samples per chunk, 48 chunks per phase, and 256 typed producer
events per phase. App memory therefore has an exact profile-derived ceiling
rather than retaining an unbounded sample, receipt, event, or channel tail.

The evaluator replays chunks one at a time. It retains only the previous
sample, cumulative maxima, first/last settled values, and checked integer
regression sums. No raw-sample vector, GPU timestamp vector, or unbounded event
tail is introduced.

## Domain closure

Reference Output now preserves explicit provider hardware-clock ticks plus tick
rate, adjacent maximum gap, regressions/rate changes, callback count, reference
lock-loss transitions, current outstanding depth, and aborted frames. Its
invariant is:

```text
scheduled = completed + late + dropped + flushed + aborted + outstanding
```

Stop, block, and post-consume ANC/hardware-time failure classify every remaining
frame instead of silently clearing it.

Export publishes a constant-size `ExportEnduranceSnapshot` with cumulative
admissions, failures, cancellations, rendered frames, durable artifacts,
activity events, active gauges, and worker lifecycle. `shutdown_and_wait`
cancels reversible work, terminalizes pending jobs, waits within an explicit
bound, and reports whether the worker returned and queues reached zero.
Independent finished-artifact re-open/validation remains an App capture fact;
durable namespace publication alone is not relabeled as content verification.
The supervisor accepts only typed artifact receipts containing the artifact,
independent validator, and validator-report digests. Concurrent recovery must
record `seek -> surface_device_reopen -> export_cancel_retry -> cache_pressure`
for every complete cycle. Event order, time, count, and terminal counters close
twice: before App publication and again in the external PowerShell verifier.

## Identity and replay

One run binds:

- canonical profile digest;
- clean source revision;
- release candidate, package, and actually executed runtime image;
- build provenance and machine report;
- one admitted COL-046 platform/driver/display cell;
- one externally hash-approved single-use capture-authority manifest;
- equal before/after environment identity;
- exact workload, normalized owner-report, and raw-evidence files for every phase.

`mondrian-endurance-replay` reads bounded regular files, replays the exact chunk
closure, requires a complete `Qualified` report, and creates a new report file.
The external verifier independently pins the replay binary, profile bytes, and
capture authority; checks its challenge and release/phase bindings; parses and
re-derives typed producer-event summaries; requires
the evidence directory to contain exactly every declared chunk, owner report,
and raw evidence file; rejects links/extra files/hash drift; re-enumerates the
directory after replay; and rehashes both inputs and output around inspection.
It also rejects reparse-point ancestors and applies explicit JSON size bounds.

## Qualification boundary

Fast synthetic tests prove schema, hashing, deterministic verdicts, accounting,
leak detection, chunk tamper rejection, and worker retirement. They do not
prove 8/24/72-hour stability, physical reference lock, DeckLink/AJA callback
cadence, monitor behavior, or a platform/driver campaign. Those facts remain
HITL and must be captured on the approved physical rig; `NotRun` can never be
promoted to success.
