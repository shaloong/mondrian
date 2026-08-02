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

`ExportConfig` contains exactly four authorities:

- `ExportPreset`: container, video/audio codec settings, output resolution, and
  explicit `ExportColorTarget`, encoded signal representation, and Alpha
  delivery policy;
- `TimelineExportSnapshot`: the immutable authoring and dependency closure;
- `output_path`: the final deliverable name reserved by the queue;
- `ExportOutputPolicy`: the final namespace policy frozen at admission.

`ExportOutputPolicy::CreateNew` is the safe default. It requires the route to
remain absent through the final atomic namespace operation, so an existing file
or a file created by another actor during a long export wins the route.
`OverwriteExisting` is explicit last-writer-wins authority over the direct file
present at final publication time, or creates the route if it is still absent.
It is not compare-and-swap against an object observed at admission. FFmpeg
never receives either policy as `-y`/`-n`; it writes only the reserved sibling,
and Mondrian applies the frozen policy after validation.

`TimelineExportSnapshot` contains one selected root Sequence and range, the
exact range-selected `PreparedVisualRangeClosure`, the selected Audio Program
Output plus its exact compiled nested Program occurrences, the one Effect
Definition Registry revision and Program set those captures bind, Project
color management, and one internally consistent `ExportMediaDependency` per
file-backed Asset selected by the union of visual and encoded-audio demand.
The visual closure is derived from Prepared Visual Schedule interval queries,
Transition/temporal expansion, and exact nesting/retime projection; it is not a
second Export-owned Clip walker. Audio reachability follows routing and
post-mute/output gating rather than media presence or visual visibility.

Each media dependency binds the resolved path, `MediaFileFingerprint`, detected
color evidence, persistent user interpretation, structured color diagnostic,
and optional full source picture extent in one record. The extent is absent
only for audio-only dependencies; a selected picture plan without one fails
closed. Parallel path/color/interpretation/geometry maps are forbidden because
they permit internally inconsistent snapshots.

Snapshot capture is an App-domain transaction performed before queue admission:

- missing or cyclic selected internal references, mixed Program/registry
  revisions, and recursive depth overflow fail closed;
- unrelated Project Sequences are not copied into the submission;
- off-range, visually hidden, disabled, post-mute-gated, and non-encoded-audio
  sources do not poison admission; missing or offline selected media does;
- synthetic Adjustment Layers remain Timeline authoring data and do not create
  a filesystem dependency;
- Basic Titles remain Sequence-local generated author data. Queue admission
  resolves and byte-freezes every selected exact named face under a separate
  font grant; workers never query the live system catalog, and a
  missing/changed/undeclared fallback face fails closed;
- the admitted job remains valid after the live Project is edited or closed,
  the Effect Registry changes, or fonts are installed/removed, because it owns
  the frozen Programs, media facts, and font bytes rather than reading mutable
  `AppState`.

The snapshot is an execution capture, not a persisted replacement for the
Project document and not a compatibility boundary between releases.

Authored Clip transforms remain expressed against the frozen source extent and
the owning Sequence raster. When export decodes at a different sampled size or
uses a delivery raster override, the renderer projects the affine exactly once
from authoring extents to sampled extents. The target decode width and height
are part of cache identity. Media, nested Sequences, generated layers, and
Transition endpoints use the same projection, so reduced-resolution delivery
cannot double-apply an auto-fit transform or reuse a frame prepared for another
extent.

## Delivery contract and parameter ownership

Sequence authoring owns working/input/Program Output color intent and separate
authored delivery defaults. Its `delivery.video_range` and
`delivery.bit_depth` values are used only when a preset parameter selects
`FollowSequence`.

`ExportPreset` owns the concrete representation of one deliverable:

- container and typed codec profile;
- `ExportColorTarget`: follow Sequence Program Output, explicit colorimetric
  output, or an explicit Project-engine Rendering View;
- explicit or Sequence-default sample depth and range;
- explicit chroma sampling and Alpha policy;
- output raster override or exact Sequence raster;
- rate control and audio codec/disable policy.

The product boundary distinguishes “not represented” from “supported with an
encoder-selected guess.” Sequence settings expose raster, frame rate/time base,
pixel aspect, field order, output color, bit-depth and range defaults, audio
sample rate, and channel layout. M1 export presets can override raster, bit
depth, range, chroma, codec profile, Alpha, rate control, audio codec, and the
typed color target. The export form edits all currently implemented
preset-owned values through one materialized typed draft, including GIF palette
controls and PCM integer depth. Color target mode and endpoint are separate:
Rendering View lists display-referred spaces, while Colorimetric lists encoded
display and scene-log spaces. Selecting a stable built-in preset resets the
draft; editing it never mutates the catalog or a previously admitted job.
Illegal intermediate combinations remain visible with the structured
delivery-admission reason and cannot enqueue.

Export frame-rate conversion, audio sample-rate/layout conversion, GOP/B-frame
control, CBR/ABR/two-pass modes, hardware-encoder profiles, and image-sequence
formats are not represented by placeholder scalars. Each needs its own typed
policy plus scheduling/resampling or encoder capability evidence before the UI
may expose it. In particular, an export frame-rate override must define exact
video cadence and A/V duration semantics; it cannot merely replace the FFmpeg
`-r` value.

`FollowSequence` is a UI authoring convenience, not an execution-time `Auto`.
`resolve_export_delivery(...)` lowers every preset choice to one
`ResolvedExportDeliveryContract` before admission. The resolved contract has an
exact color target/output-transform intent, raster, bit depth, range, chroma
sampling, and FFmpeg pixel format.
Execution, internal frame precision, FFmpeg arguments, and post-encode probe
expectations consume that same result. Nested Sequences contribute
working-domain pixels but cannot replace the root job's resolved delivery
contract.

Sequence Program Output remains display/delivery-referred and valid on its own.
Camera Log intermediates use an explicit colorimetric `ExportColorTarget`;
export admission then requires 10-bit-or-higher MOV/MXF ProRes. A codec name
never changes Program Output, selects a Rendering View, or authorizes relabeling
unchanged samples.

Renderer working precision is not an export-form parameter. Working-space CPU
and composite targets remain the fixed 32-bit-float correctness contract;
encoded 8/10/12-bit delivery depth and the derived renderer-to-encoder transport
are separate. The form never labels a float texture or `rgba64le` pipe as a
user-visible 16-bit delivery.

The product-owned preset catalog has stable identities. M1 includes
`h264-aac-sdr` (H.264 High, 8-bit 4:2:0 Legal, AAC) and `hevc-main10` (HEVC
Main10, 10-bit 4:2:0 Legal, AAC). ProRes profiles are typed values rather than
free-form strings. A ProRes profile, H.264/HEVC profile, bit depth, or chroma
combination that has no verified lowering is unrepresentable or rejected; it
does not fall back to another profile.

Single-pass CRF quality may optionally add a complete VBV pair
(`max_bitrate_kbps` and `buffer_size_kbits`). Supplying only one is invalid.
The encoder receives `-maxrate/-bufsize`; Mondrian does not mix CRF with an
ambiguous average `-b:v` request. GOP structure, two-pass encoding, additional
professional profiles, and other advanced controls must be added as typed
contracts with argument and roundtrip evidence before a UI can expose them.

Subsampled raster constraints are checked at admission: 4:2:0 requires even
width and height, and 4:2:2 requires even width. Mondrian rejects invalid
dimensions instead of silently cropping or rounding the requested deliverable.

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

- preset, root Sequence, and Project color policy resolve to one legal delivery
  contract;
- at most 64 Pending/Running/Cancelling jobs per queue;
- one active owner for each normalized final output path, including lexical and
  canonical-parent aliases where the filesystem can resolve them;
- a nonempty file-like output path whose parent already exists, is a directory,
  and can be canonicalized;
- an absent final route for `CreateNew`, or an absent/direct-file route for
  `OverwriteExisting`; links, directories, and other non-file entries fail
  closed;
- an available worker and a unique nonzero monotonic attempt generation.

Admission freezes the normalized route as an absolute path in both the heavy
job and lightweight snapshot, together with the output policy. Execution and
publication therefore cannot reinterpret a relative path after the process
working directory changes or silently upgrade a create-only request to
overwrite. Admission-time absence is only an early rejection: `CreateNew`
enforces no-overwrite again at the atomic publication boundary.

The App Adapter performs the same pure check before expensive media snapshot
capture so the panel can explain an invalid choice immediately. That check is
not authority: `RenderQueue` repeats it against the immutable snapshot before
reserving capacity or dispatching a worker, and execution validates again
before opening FFmpeg.

Admission returns a structured `ExportAdmissionError`; it never reports success
after delivery rejection, worker startup failure, or capacity rejection. The heavy `RenderJob`
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
The queue owns the irreversible publication boundary and exposes its state as
`ExportPublicationState`:

- `Reversible` may still accept cancellation or resource yield;
- `Committing` has atomically acquired irreversible publication authority;
- `Published` requires durable publication evidence for the exact admitted
  route;
- `DurabilityUnconfirmed` proves the final route names the new object but not
  that its containing-directory update survived a crash;
- `NotPublished` proves that no irreversible deliverable namespace operation
  completed, including a typed pre-namespace failure after the queue acquired
  commit authority;
- `OutcomeUnknown` conservatively represents a failure/panic after commit
  authority when final-path observation is required.

Cancellation in `Committing` returns
`ExportCancelOutcome::TooLateCommitting` without setting the shared token or
claiming cancellation success. An executor that reports `Published` without
first entering the queue publication Gate fails closed.
`JobExecutionResult::ReversibleWorkCompleted` is reserved for internal
render/encode/validation sub-operations and is rejected as a terminal queue
outcome; only `Published` can create a completed job. Likewise, an executor that
reports `Cancelled` after the Gate becomes a structured failure with
`OutcomeUnknown`, never false cancellation evidence.

The public `ExportArtifactPublicationEvidence` records the corresponding
artifact fact as exactly one of `Durable`, `BeforeNamespace`,
`DurabilityUnconfirmed`, or `NamespaceIndeterminate`. Only `Durable` can
authorize `Completed`. The other variants retain the absolute intended route
and, when provable, the exact surviving partial-object route needed for
diagnosis or an explicit retry policy.

Every admitted terminal attempt records its generation, execution priority,
terminal disposition, deadline status, timestamps, and whether worker execution
actually began. Offline export currently has no implicit presentation deadline,
so terminal evidence records `NotApplicable` rather than fabricating one.
Executor panics are isolated as structured failures and the worker continues to
the next admitted job.

## Exact partial-object and publication boundary

FFmpeg never receives the final route. Export first asks `mondrian-storage` for
one unique direct sibling file and releases that reservation to the external
writer while retaining its kernel object identity and cleanup authority. After
FFmpeg exits, Export reclaims the path only if it still names the reserved
object. A missing, substituted, linked, or otherwise different object fails
before validation and cannot be published.

Validation runs while the reclaimed guard owns that exact object. After the
Publishing Gate, the same guard maps `CreateNew` to Storage
`FilePublicationMode::CreateNew` and `OverwriteExisting` to
`FilePublicationMode::ReplaceExisting`. It delegates file flush, the atomic
namespace operation, postcondition classification, and parent-directory
durability to the single shared Storage primitive. Export contains no private
`ReplaceFileW`, rename, or directory-sync implementation.

If publication fails before the namespace boundary, the fully encoded and
validated partial object is deliberately retained and named in terminal
evidence; it is not silently deleted. A durability-unconfirmed result records
the final route because the new namespace is already observable. An
indeterminate result preserves every storage-observed possible new-object
route and forbids automatic cleanup. These are failed jobs, not `Completed`
jobs, even though retry/repair policy differs by variant.

## Media revision and color correctness

Every picture dependency freezes its exact physical video-stream index together
with the admitted `MediaFileFingerprint`; missing bindings never fall back to
stream zero. The fingerprint is checked before preparation, before decoder
Session reuse/open, after frame materialization, and again after validation
immediately before publication. The stream index and complete revision are
also part of the decode-cache and returned-layer execution identity. A missing,
replaced, modified, or invalidly bound source fails closed and the temporary
output is discarded. This prevents a long export from publishing a mixture of
author intent, silently changed source media, or the wrong stream.

Preview and Export may differ in scheduling, cache lifetime, readback, and
delivery transform. They must share Timeline ordering, nested Sequence
semantics, exact time evaluation, processor/effect interpretation, audio
schedule semantics, working-space compositing, and input-color resolution.
One Export visual Session is reused across every frame, nested Sequence, and
Transition endpoint so Basic Title font/raster identity, explicit decoder
Session/DPB residency, and bounded cache lifetime are job-scoped rather than
global or frame-local. Terminal return and unwinding explicitly retire that
decoder context; production Export never uses the media thread-local
convenience decoder.
Detected metadata absence invokes the authored missing-metadata policy; it never
means implicit Rec.709. High-bit-depth output must cross the typed renderer
float boundary and fails closed rather than manufacturing nominal 10/12-bit
samples from RGBA8.

## Transactional deliverable publication

FFmpeg writes a unique sibling partial path, never the requested final path.
Mondrian waits with bounded stderr capture and cooperative cancellation, then
validates the typed preset-derived `ExportValidationExpectations` against that
partial deliverable. The mux family and MP4/QuickTime major brand must match.
Video and audio each use a closed `Required(exact constraints) / Forbidden`
presence contract. Required video proves codec/profile, pixel-format-derived
bit depth, exact rational frame rate, geometry, range/CICP and static HDR exact
metadata or required absence. Required audio proves codec, sample rate, channel
count and semantic layout. Duration must remain within the explicit delivery
tolerance. The same pass returns a typed `ExportOutputProbe` for Headless
evidence; acceptance code must not reconstruct a second ffprobe policy.

Only a validated partial may enter Publishing. The shared Storage primitive
flushes the exact source object, performs the frozen create-or-replace operation
in the same directory, verifies the observed object identities, and establishes
the containing-directory durability barrier. Platform mechanics remain
Storage-owned and are specified by
[Storage Publication](../architecture/storage-publication.md).

The result is not a boolean:

- `Durable` proves the exact admitted final route names the validated object and
  its namespace update crossed the durability barrier; only this result
  authorizes `Completed`;
- `BeforeNamespace` proves the new object did not become the final route. Export
  retains the validated partial and reports its exact path; under `CreateNew`,
  this includes a late target collision and the competing target is untouched;
- `DurabilityUnconfirmed` proves the final route names the validated object but
  not that the directory update survived a crash. The job fails and must not be
  reported as completed merely because the file is visible;
- `NamespaceIndeterminate` cannot prove whether the final route or a surviving
  sibling names the new object. Export records every verified surviving route
  available from Storage and forbids automatic cleanup or blind retry.

A partial that fails before validation, or is cancelled before the Publishing
Gate, is removed only while its retained object identity still proves ownership.
The validated sibling is deliberately preserved on `BeforeNamespace`; unknown
post-namespace state is quarantined rather than swept. Temporary audio is
cleaned independently because it is never publication evidence.

Cross-volume publication is structurally avoided by placing the partial beside
the final path. Validation success or namespace visibility is not reported as
job completion until durable publication evidence exists.

## UI and Headless boundary

`AppState` is the shallow Adapter for authoring lookup, snapshot capture, queue
submission, cancellation, cleanup, diagnostics, and revision polling. Window
panels render immutable snapshots and dispatch typed actions only. They do not
own workers, inspect the heavy Timeline payload, infer terminal states, invent
progress, or mutate queue entries. Headless validation consumes the same queue
and evidence Interface.
