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
  explicit `ExportColorTarget`, encoded signal representation, and Alpha
  delivery policy;
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
- Basic Titles remain Sequence-local generated author data; their exact named
  font dependency is resolved by the shared renderer during execution and a
  missing/changed/undeclared fallback face fails the job closed;
- the admitted job remains valid after the live Project is edited or closed,
  because it owns the frozen closure rather than reading mutable `AppState`.

The snapshot is an execution capture, not a persisted replacement for the
Project document and not a compatibility boundary between releases.

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

The current product boundary intentionally distinguishes “not yet exposed” from
“supported with an encoder-selected guess.” Sequence settings already expose
raster, frame rate/time base, pixel aspect, field order, output color, bit-depth
and range defaults, audio sample rate, and channel layout. M1 export presets can
override raster, bit depth, range, chroma, codec profile, Alpha, rate control,
and audio codec. The product export form edits all of those currently
implemented preset-owned values through one materialized typed draft, including
GIF palette controls and PCM integer depth. Selecting a stable built-in preset
resets that draft; editing it never mutates the catalog or a previously admitted
job. Illegal intermediate combinations remain visible with the structured
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
- a nonempty file-like output path;
- an available worker and a unique nonzero monotonic attempt generation.

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
One Export visual Session is reused across every frame, nested Sequence, and
Transition endpoint so Basic Title font/raster identity and bounded cache
lifetime are job-scoped rather than global or frame-local.
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
