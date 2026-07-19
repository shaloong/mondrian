# Export Execution Specification

## Scope

Mondrian Export is an offline Timeline execution Module. It freezes one valid
authoring state, renders that state through the production Timeline/audio/color
semantics, validates the encoded deliverable, and publishes it without exposing
an incomplete file at the requested output path.

Direct file-to-file transcoding is not an Export input model. If a future
product workflow needs media conversion, it must be a separately named Module
with its own intent and validation contract rather than a second branch inside
`ExportConfig`.

## Immutable submission

`ExportConfig` contains exactly three authorities:

- `ExportPreset`: container, video/audio codec settings, output resolution, and
  explicit Alpha delivery policy;
- `TimelineExportSnapshot`: the immutable authoring and dependency closure;
- `output_path`: the final deliverable name reserved by the queue.

`TimelineExportSnapshot` contains the selected root Sequence, only the nested
Sequences reachable from enabled Clips, the requested range, Project color
management, and one `ExportMediaDependency` per reachable real media Asset.
Each media dependency binds the resolved path, `MediaFileFingerprint`, detected
color evidence, persistent user interpretation, and structured color
diagnostic in one record. Parallel path/color/interpretation maps are forbidden
because they permit internally inconsistent snapshots.

Snapshot capture is an App-domain transaction performed before queue admission:

- missing nested Sequence references and recursive Sequence nesting fail
  closed;
- unrelated Project Sequences are not copied into the submission;
- missing or offline real media fails before admission;
- synthetic Adjustment Layers remain Timeline authoring data and do not create
  a filesystem dependency;
- the admitted job remains valid after the live Project is edited or closed,
  because it owns the frozen closure rather than reading mutable `AppState`.

The snapshot is an execution capture, not a persisted replacement for the
Project document and not a compatibility boundary between releases.

## Range

`TimelineExportRange` has three explicit forms:

- `SequenceInOut`: the Sequence's authored In/Out range;
- `EntireSequence`: all authored Timeline content;
- `WorkArea { start_frame, end_frame_exclusive }`: one validated half-open frame
  interval.

Range lowering uses the Sequence rational frame grid. An empty, reversed, or
unrepresentable range fails before encoder publication; no fallback duration is
invented.

## Queue admission and ownership

Each `RenderQueue` instance owns one dedicated offline worker and an independent
bounded state machine. It does not share Preview, Thumbnail, Waveform, Proxy, or
realtime audio capacity.

Admission requirements are:

- at most 64 Pending/Running/Cancelling jobs per queue;
- one active owner for each normalized final output path, including lexical and
  canonical-parent aliases where the filesystem can resolve them;
- a nonempty file-like output path;
- an available worker and a unique nonzero monotonic attempt generation.

Admission returns a structured `ExportAdmissionError`; it never reports success
after worker startup failure or capacity rejection. The heavy `RenderJob`
payload is immutable and consumed exactly once at dispatch. UI and Headless
observers receive only bounded `ExportJobSnapshot` values, so polling cannot
clone a Project-sized Timeline. Terminal snapshots retain no heavy payload and
are capped at 256 entries until explicit cleanup.

## Lifecycle, progress, and cancellation

The observable lifecycle is:

```text
Pending -> Running(phase) -> Completed | Failed | Cancelled
                      \-> Cancelling(phase) --^
```

Execution phases are Preparing, Rendering, Encoding, Validating, and
Publishing. Whole-job fraction, phase order, and phase-specific units are
monotonic. Rendering may report exact Frames; Encoding may report measured
media time. Unknown work is represented as no unit detail, never as fabricated
frame counts.

Cancellation uses `ExecutionCancellationToken`. A queued cancellation releases
the heavy payload without crossing the execution boundary. A running request
becomes `Cancelling` until the executor reaches a cooperative checkpoint.
`JobExecutionResult` owns the irreversible publication boundary:

- `Cancelled` means cancellation was observed before publication;
- `Failed` means no new deliverable was published;
- `Completed` means the validated deliverable crossed the publication point and
  must win over a cancellation request that arrived too late.

Every admitted terminal attempt records its generation, execution priority,
terminal disposition, deadline status, timestamps, and whether worker execution
actually began. Offline export currently has no implicit presentation deadline,
so terminal evidence records `NotApplicable` rather than fabricating one.
Executor panics are isolated as structured failures and the worker continues to
the next admitted job.

## Media revision and color correctness

Every real media dependency is checked against its admitted
`MediaFileFingerprint` before preparation and again after validation immediately
before publication. A missing, replaced, or modified source fails closed and the
temporary output is discarded. This prevents a long export from publishing a
mixture of author intent and silently changed source media.

Preview and Export may differ in scheduling, cache lifetime, readback, and
delivery transform. They must share Timeline ordering, nested Sequence
semantics, exact time evaluation, processor/effect interpretation, audio
schedule semantics, working-space compositing, and input-color resolution.
Detected metadata absence invokes the authored missing-metadata policy; it never
means implicit Rec.709. High-bit-depth output must cross the typed renderer
float boundary and fails closed rather than manufacturing nominal 10/12-bit
samples from RGBA8.

## Transactional deliverable publication

FFmpeg writes a unique sibling partial path, never the requested final path.
Mondrian waits with bounded stderr capture and cooperative cancellation, then
validates stream presence, codec signal, geometry, rate, duration, color/HDR
metadata, and other preset expectations against that partial deliverable.

Only a validated partial may enter Publishing:

- Windows uses the native write-through replace/move operation in the same
  directory;
- Unix-family platforms flush the temporary file, use same-filesystem atomic
  rename, and attempt a containing-directory durability sync after the
  irreversible commit; a directory-sync failure is reported as an operator
  warning because it cannot truthfully undo the visible publication;
- a missing/failed/cancelled partial never disturbs an existing deliverable;
- temporary audio and output artifacts are cleaned on every non-success path.

Cross-volume publication is structurally avoided by placing the partial beside
the final path. Validation success is not reported as job completion until the
publication operation succeeds.

## UI and Headless boundary

`AppState` is the shallow Adapter for authoring lookup, snapshot capture, queue
submission, cancellation, cleanup, diagnostics, and revision polling. Window
panels render immutable snapshots and dispatch typed actions only. They do not
own workers, inspect the heavy Timeline payload, infer terminal states, invent
progress, or mutate queue entries. Headless validation consumes the same queue
and evidence Interface.
