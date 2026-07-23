# Mondrian Editing Context

Mondrian edits time-based audiovisual projects while preserving deterministic timeline semantics and explicit runtime capability evidence.

## Language

**Playback Session**:
One contiguous transport run with a stable epoch, timeline mapping, rate, direction, and clock policy.
_Avoid_: Player instance, preview session

**Transport State**:
The user-visible stopped, paused, priming, playing, recovering, or ended condition of a Playback Session.
_Avoid_: Playback boolean

**Clock Master**:
The single authoritative elapsed-media-time source for a running Playback Session.
_Avoid_: Current video frame, UI timer

**Monotonic Runtime Clock**:
A process-local nondecreasing elapsed-time source used for lifecycle ages, expiration, and execution evidence. It is never authored/media time and never advances a Playback Session.
_Avoid_: Clock Master, Timeline Time, wall-clock timestamp, UI event-loop timer

**Frame Work Deadline**:
An opaque Adapter deadline paired with its remaining duration at admission, then lowered once into the Frame Work Broker's Monotonic Runtime Clock for queue, cancellation, and completion decisions.
_Avoid_: UI comparison closure, decode-completion `Instant`, renewed timeout after queueing

**Frame Demand**:
A versioned request for the best frame needed at a timeline position before a presentation deadline.
_Avoid_: Render request, preview refresh

**Frame Delivery**:
The observed outcome of a Frame Demand, including readiness, timing, execution path, degradation, and blocker evidence.
_Avoid_: Preview result

**Frame Presentation Ticket**:
Opaque authority binding one Frame Demand identity, its final presentation deadline, and the only allowed on-time quality outcome for a CPU or GPU Presentation Adapter.
_Avoid_: Preclassified ready result, UI-ready flag

**Frame Request Binding**:
The latest Frame Demand identity and Adapter deadline atomically attached to one opaque frame-work key; a reusable completion may satisfy this binding, while an obsolete cancellation cannot consume it.
_Avoid_: Worker-captured demand identity as final authority, separate pending and completion ownership

**Frame Work Broker**:
The Playback Module that atomically owns frame-work admission, queued transport, worker-lane-bound in-flight execution leases, generation invalidation, preemption, lowered deadlines, cancellation, worker-completion stamps, and completion binding while treating media payloads and absolute Adapter clock values as opaque data.
_Avoid_: Independent scheduler and worker queue, Adapter-owned worker-activity counters, key-only worker completion, UI-owned priority rollback, Adapter-composed freshness/preemption queries

**Frame Cancellation Evidence**:
Playback-owned, all-run evidence from an authoritative cancellation request through the first cooperative execution checkpoint to worker return, partitioned by Playback, Interactive, and Still frame-work class.
_Avoid_: UI counter families, full worker lifetime as cancellation latency, one cleanup budget for unlike work classes

**Preview Decode Cancellation Fact**:
The media Adapter's immutable first-observation fact for one canceled decode: one typed execution checkpoint plus an ordinary cooperative observation, FFmpeg blocking-I/O interruption, or parent-enforced isolated-demux termination. It complements Broker-owned cause and timing evidence; it does not own cancellation authority or infer timing.
_Avoid_: unit `Canceled`, parsing FFmpeg error text, reporting process termination as an FFmpeg callback return, last checkpoint instead of first observation, media-owned cancellation reason

**Execution Cancellation Token**:
Monotonic, cloneable generation authority shared by schedulers, runtimes, and concrete media Adapters. Cancellation never resets a token; a new generation receives a new token. Canceled execution may not populate success caches or terminal failure memory.
_Avoid_: Resettable flags, Adapter-owned generation truth, caching canceled results

**Execution Work Intent**:
The minimal cross-domain value language for priority, an optional Adapter-lowered deadline, generation cancellation, and terminal disposition. It is not a scheduler: Preview frames, waveform analysis, thumbnails, proxies, and Export retain domain-owned admission, capacity, worker, and resource policy.
_Avoid_: Universal media worker pool, cross-domain job enum, one global capacity or eviction policy

**Execution Terminal Evidence**:
One immutable terminal classification for a Module-local execution attempt: generation, cross-domain priority, completed/failed/canceled/superseded/rejected disposition, and deadline status. Domain diagnostics retain the detailed reason and resource evidence.
_Avoid_: Boolean success, tracing log as completion truth, UI-inferred cancellation, shared type owning domain failure taxonomy

**Thumbnail Execution Service**:
The UI-independent App Module that resolves exact source/color identity, admits and cancels deterministic still work, owns bounded raster/failure residency, and publishes terminal execution evidence. Its product output is a validated encoded RGBA raster; Widget payloads exist only in the Window Adapter.
_Avoid_: Widget-owned FFmpeg worker, unbounded completion channel, cache keyed only by AssetId, stale publication after relink or color-generation change, UI raster as execution payload

**Proxy Generation Service**:
The AppState-owned, lazily started deep Module that unifies import, user, and Playback-recovery proxy demand under one exact artifact key, bounded project generation, fair domain queues, cache-root execution capacity, recoverable failure memory, and terminal evidence. Media owns freshness, encoding, and cancellable FFmpeg artifact production; Window and Preview callers own no second dedupe registry.
_Avoid_: Process-global app dispatcher, unbounded FIFO, worker blocked on a limiter after falsely entering Running, permanent Preview request set, project-close orphan transcode, automatic retry storm

**Timeline Export Snapshot**:
One immutable execution capture containing a root Sequence, only its reachable nested Sequence closure, one internally consistent media-dependency record per reachable file-backed Media Asset, the selected range, and the exact Project Color Environment. Generated Asset Library identities do not become file dependencies. Capture rejects missing/cyclic internal references and offline media before admission; the snapshot then executes independently of later Project edits or closure.
_Avoid_: Whole-Project clone, four parallel asset maps, live Asset Library lookup from the worker, path without source revision, snapshot presented as persisted author state

**Export Execution Service**:
The instance-owned offline Module that admits immutable Timeline Export Snapshots into a bounded dedicated queue, reserves final output identities, owns attempt generations and cancellation, consumes each heavy payload once, and exposes only bounded lightweight lifecycle/evidence snapshots. It shares cross-domain execution value semantics but not worker capacity with Preview, Thumbnail, Waveform, Proxy, or realtime audio.
_Avoid_: Process-global queue, universal media worker pool, UI-mutated job status, unbounded terminal Project retention, enqueue success after worker failure

**Export Deliverable Publication**:
The irreversible boundary at which a fully encoded and validated sibling temporary artifact atomically replaces or creates the requested final path using the platform filesystem Adapter. `Completed` means this boundary was crossed; cancellation observed after it cannot rewrite history as `Cancelled`.
_Avoid_: FFmpeg writing the final path directly, moving the prior deliverable away before publication, validation success treated as publication success, late cancellation overriding a committed output

**Playback Quality Policy**:
The allowed temporary preview resolution and user-selected proxy/original policy for a Playback Session.
_Avoid_: Quality flag

**Playback Evidence**:
Structured events and aggregates proving clock, scheduling, delivery, degradation, synchronization, and recovery behavior.
_Avoid_: Debug log

**Process Memory Evidence**:
Bounded fixed-cadence observations from a native current-process probe. Private committed bytes are the ownership/plateau metric; resident or working-set bytes are diagnostic because the OS may reclaim them independently.
_Avoid_: Frame-cache byte totals presented as whole-process memory, one start/end sample, OS peak since process launch as gate-local growth

**Playback Memory Class**:
A versioned qualification of physically installed memory for one validation workload: `minimum-supported` (8 GiB correctness, boundedness, and explicit fallback), `standard-playback` (16 GiB M0 Video+Audio baseline), or `professional-large-project` (32 GiB large-project recommendation). OS-visible memory is separate evidence and cannot silently change the class.
_Avoid_: One universal RAM threshold, treating 8 GiB as native-4K performance proof, rejecting nominal memory because firmware reserved a small region, diagnostic override presented as baseline

**Professional Playback Acceptance**:
A versioned fail-closed contract that binds decoder-proven media identity to frame-local decode provenance, the exact Viewer candidate for a Frame Demand, and completed GPU presentation evidence.
_Avoid_: Filename-based fixture label, capability-only hardware claim

**Reference Playback Run**:
One immutable evidence bundle binding a versioned gate plan, source revision and cleanliness, an operator-assigned non-hardware machine identity, the required and observed Playback Memory Class plus validated machine report, corpus revision, generated-recipe identities, run-local artifact hashes, exact gate commands, and structured execution reports. A partial, dirty, or explicitly under-class run may diagnose behavior but cannot become a baseline.
_Avoid_: Loose log directory, filename-selected media, mutable “latest” fixture, machine serial capture, successful process exit without a passing execution report

**Reference Playback Gate Supervisor**:
The external validation Module that gives each gate a versioned wall-clock deadline, drains both child output pipes, terminates the complete descendant process tree on timeout, and preserves the log plus any independently flushed progress journal before classifying the gate. A timeout is terminal evidence and can never be represented as a passing or merely missing execution report.
_Avoid_: Test-internal timeout as the only process bound, synchronous `cargo` invocation, killing only the immediate child, absent report treated as an ordinary assertion failure

**Preview Frame Store**:
The bounded runtime store for decoded preview payloads, including CPU pixels or opaque native decoder-resource leases, UI-independent final Preview rasters, failure memory, the explicitly pinned current/stale Preview raster, and an oversize current-media exception that remains visible in evidence. CPU bytes, entry count, and decoder-resource units are independent budgets. After stopped transport has a registered GPU Viewer output proved for its exact Preview generation and all work is quiescent, media payloads/pins may be released independently of that output and failure memory so one retained AVFrame cannot pin an idle decoder surface pool. Widget payloads are constructed only by the final Window Presentation Adapter and are never cache state.
_Avoid_: Entry-count-only cache, treating zero-host-byte native surfaces as free, a residency budget smaller than the configured prefetch window, clearing media before final output is usable, retaining an idle hardware surface pool, unbounded last frame, dropped oversize current delivery, retaining `ViewerFrameImage` in the Store

**Color Frame Contract**:
The typed per-frame identity of extent, render-graph domain, color or working-space identity, sample encoding, CPU/GPU residency, and RGB/coverage alpha association. Public working frames carry straight or opaque coverage; premultiplied frames exist only inside an explicitly typed spatial operation and may not cross OCIO, effect, composite, display, or export seams.
_Avoid_: Bare RGBA buffer, texture format as color identity, shader-only premultiplied flag, alpha inferred from codec or pixel values

**Playback Preview Pump**:
The UI-independent App Module coordinator that samples one pending Frame Demand and current transport activity, applies bounded preview-work completions and expirations, submits their exact terminal Frame Deliveries, then observes current-epoch video preroll in that order.
_Avoid_: Window-owned result policy, Headless-only orchestration, separately sampled demand identities for completion and expiration

**Preview Execution Coordinator**:
The UI-independent App Module that atomically binds complete Viewer intent to one media generation, pending state, executed presentation quality, monotonic candidate identity, and the currently registered output together with the generation that proved it exact. A retained output from an older generation remains stale until full output-key resolution or a new presentation proves it under the active generation. Window and Headless presentation are Adapters over its opaque GPU execution contract; neither may reconstruct candidate/cache lifecycle.
_Avoid_: Window-owned candidate counter, separate Headless output identity, UI-owned generation/pending booleans, treating retained stale output as current after generation rotation, cache hit inferred from texture presence alone

**Preview Output Unavailability**:
The typed terminal contract carried unchanged from Timeline and media resolution through CPU/GPU execution, final presentation, Window/Headless Adapters, and evidence. It separates expected `NoContent`, fail-closed dependency or correctness `Blocked`, and admitted execution `Failed`, and always names the owning production stage; diagnostic text is never classification authority.
_Avoid_: Unit `Unavailable`, error string parsing, missing dependency reported as empty Timeline, execution failure reported as loading, stale-frame reuse across a terminal unavailable result

**Preview Viewer Plan**:
The UI-independent resolved element representation and pure lowering Module that owns stable cache identity, aggregate presentation quality and decode provenance, deferred-composite detection, and renderer GPU-layer admission. Window and Headless Adapters consume the same result and blocker semantics.
_Avoid_: Window-owned plan hashing, Headless-specific lowering, cache identity derived from rendered pixels, presentation side effects during lowering

**Preview CPU Execution**:
The UI-independent App Module that prepares source frames into working-linear inputs, composites one Preview Viewer Plan through renderer semantics, and applies the Program Output plus monitor adaptation for a final CPU raster. It returns pixels, complete color/composite facts, and stage durations; presentation Adapters only project those facts.
_Avoid_: Calling Window diagnostics from execution, Headless-specific color math, discarded input-transform evidence, Widget raster types in the execution result

**Preview Media Task**:
The UI-independent App Module that consumes admitted Preview media jobs, runs the concrete FFmpeg Preview Adapter, observes Broker cancellation at cooperative and blocking-I/O checkpoints, publishes one structured terminal result with the media Adapter's first-observation fact, and performs bounded worker shutdown. Window and Headless Adapters consume the same task semantics and may not reconstruct a second decode loop.
_Avoid_: Window-owned codec worker, Headless-only decoder, cancellation inferred after codec return, task success cached after cancellation

**Preview Media Source Resolution**:
The UI-independent App Module that turns one immutable asset record plus complete Viewer color, Alpha, proxy, geometry, and hardware-admission intent into exactly one canonical media key, one explicit color rejection, or one structured unavailable outcome. Proxy generation is returned as an intent; library lookup, dispatch, metrics, and presentation remain Adapter concerns.
_Avoid_: Window-built decode key, Headless-specific source interpretation, silent missing-file `None`, proxy side effects during semantic resolution, output scale encoded into a native-source decode key

**Preview Timeline Execution**:
The UI-independent App Module that consumes immutable Sequence snapshots and typed media outcomes, traverses the canonical Timeline render plan, resolves bounded nesting, evaluates each child on its own canvas under the shared runtime quality, composites nested working output, and returns a ready Viewer plan with mandatory cache identity plus ordered execution facts. Its read-only media-demand collection is the shared input to prefetch, preroll, and input-color evidence.
_Avoid_: Window-owned recursion, Headless-specific evaluator, scheduler-specific nested walker, child raster inherited from the parent, optional cache identity on a ready plan, diagnostics callbacks inside execution

**Preview Production Runtime**:
The UI-independent App composition root that binds the Frame Work Broker, Preview media workers, Frame Store, execution coordinator, Timeline/Viewer evaluation, result pump, evidence, and final application presentation arbitration. Its generic output payload is opaque: Window and Headless Adapters register their own usable GPU output while sharing generation, cache identity, pending state, stale scope, CPU raster, cancellation, and Frame Delivery semantics.
_Avoid_: `WindowPreviewAdapter` as the Headless composition root, Window-owned result pumping or cache policy, a second Headless scheduler, Widget payloads in production state, production decode residency hidden in thread-local state

**Preview Decode Session Context**:
The explicit worker-owned media execution context for packet-source/codec state, DPB, and hardware-surface pools. Playback has one continuous slot; Scrub and GPU-resident exact Still share one Interactive slot; CPU Still has one physically separate slot. Every native output carries a weakly observed lease through App and renderer clones. A slot cannot seek, flush, or change contract while its prior native output remains owned. After the final lease retires, a healthy slot may be reused only when its conservative source revision, isolated/direct packet-source execution family, and codec/output contract still match; incomplete source metadata never authorizes reuse. `IsolatedDemuxTermination` poisons the packet source and retires the complete paired Session without first flushing an unreusable codec context. Residency-family retirement is acknowledged only after every published native-output lease has retired. Idle and worker shutdown clear the complete context.
_Avoid_: hidden production TLS cache, unbounded or alternating native session pools, entering/seek-flushing the codec while its previous native output is still owned, flushing after isolated-demux termination, treating the last access mode as a terminal-state substitute, acknowledging residency retirement while a completion or renderer still owns its output

**Preview Demux Isolation Boundary**:
The media-internal recoverability seam that moves only `AVFormatContext` open, stream discovery, seek, and packet reads into one terminable packaged-helper process per source Session while retaining `AVCodecContext`, DPB, hardware device, and native GPU surfaces in the parent Preview worker. Its versioned bounded protocol validates native paths, launch identity, real FFmpeg library ABI, stream/codec identity, packet ownership, ordered non-zero command IDs, seek completion before codec flush, and pull-based packet responses. Playback, Scrub, and Exact use the same reusable `open/seek/read/close` Interface in production; direct in-process format ownership remains only an explicit unconfigured test/diagnostic Adapter.
_Avoid_: child-process RGBA as the native path, serializing FFmpeg pointers, `AV_PKT_FLAG_TRUSTED` across IPC, unbounded packet buffering, helper path discovery from `PATH`, callback cancellation presented as recoverability, claiming Playback isolation from an exact-Still-only vertical slice

**Preview Decode Execution Progress**:
A lock-free, single-writer evidence snapshot for one Preview media worker: the exact current FFmpeg/media stage, request/progress sequences, FFmpeg interrupt-callback poll/cancel sequences, the request that last observed callback cancellation, and cumulative process-isolated demux command/lifecycle facts. A helper launch owns one non-cloneable evidence lease; ready and cross-request reuse require completed protocol facts, while clean/canceled/failed/forced termination is published only after that child is reaped. It is observable by the UI-independent Preview Production Runtime and Headless acceptance but owns no timeout, cancellation, recovery, or scheduling decision.
_Avoid_: Debug log as liveness proof, helper executable configured as helper execution proof, read-count heuristics as cross-request reuse, process disappearance as a guessed terminal reason, coarse `codec` bucket, callback installed presented as callback polled, callback cancellation presented as call return, watchdog hidden in the media Adapter, progress sequence treated as completed work, UI-owned worker health

**Waveform Analysis Service**:
The UI-independent App Module that resolves asset/source revision, admits bounded background work, owns generation cancellation, decodes through a private bounded audio-source cache, retains revision-keyed envelopes and failures, and publishes bounded terminal evidence. The media Module owns only streaming PCM-to-envelope math; the Timeline receives a shallow nonblocking lookup Adapter.
_Avoid_: Widget-owned FFmpeg state, whole-file PCM, path-only or asset-only cache key, unbounded channel, global thread-local cache

**Window Preview Adapter**:
The shallow `app_ui::preview` Adapter that constructs a renderer-registered external-texture Widget payload, converts `PreviewRasterFrame` to `ViewerFrameImage` without copying pixels, and projects immutable diagnostics into panel models. It owns no worker, generation, scheduling, cache, output-selection, color, or transport policy.
_Avoid_: Redraw-local output priority, Widget types in `app::preview_runtime`, UI transport mutation, stale reuse across sequence/display/geometry identity

**Viewer GPU Preview Runtime**:
The device-scoped owner of native video import, working-linear compositing, spatial processing, display output, calibration, and current external-texture presentation resources for Viewer execution.
_Avoid_: Window-owned GPU grab bag, separate headless rendering semantics

**Viewer GPU Output Health**:
The UI-independent policy that maps one typed presentation-attempt outcome plus renderer stage facts into the canonical Waiting, Blocked, Failed, Rejected, Degraded, or Ready status and cumulative counts. Window telemetry, Headless gates, performance smoke, and the budget CLI consume this single classifier and report schema.
_Avoid_: Window-local Ready rules, duplicated health enums, treating texture registration without presentation/native-boundary proof as Ready

**Viewer GPU Output Residency**:
The UI-independent projection of declared Preview layers or completed renderer execution into typed decode, input-transform, working-residency, zero/low-copy, upload/readback, and native-import evidence. Planned and executed residency are distinct; one immutable platform capability snapshot is shared with hardware-decode admission for the lifetime of a renderer/Window Session.
_Avoid_: Planned zero-copy success, capability probe as execution evidence, per-frame platform re-probe, mixing capability generations inside one frame record

**Audio Playback**:
The realtime path that owns output-device lifecycle, PCM preroll and consumption, render generations, underrun recovery, and consumed-media-position evidence for a Playback Session.
_Avoid_: Audio clock, UI-owned output stream

**Audio Sample Position**:
One signed integer position on an explicitly identified sample-rate timeline, resolved once from rational timeline time using a declared rounding policy.
_Avoid_: Floating-point seconds passed between audio render stages, sample index without rate

**Decoded Audio Source Window**:
One exact interleaved PCM block for a fingerprinted physical stream selection on a prepared Audio Render Contract. Runtime hot windows and media LRU windows may differ in size but preserve the same integer sample coordinates.
_Avoid_: Whole-file PCM as the source Interface, per-sample decoder virtual call, path-only cache identity

**Asset Audio Component Catalog**:
The Asset-owned persisted mapping from stable `AudioSourceComponentId` values to conservative physical-stream signatures and the exact source fingerprint whose probe produced them. Initial default disposition may choose `primary`; relink never retargets an existing identity by index alone.
_Avoid_: Timeline-owned stream index, `0:a:0`, language/title as identity, silently rebinding after source replacement

**Audio Source Selection**:
A short-lived media Adapter value containing one absolute container stream index, its probed native layout, and the source fingerprint that authorized the binding. It is part of decoded-window and persistent-Session identity and is revalidated at open.
_Avoid_: Author state, path-only decode key, channel count presented as layout

**Audio Signal Layout**:
The validated semantic layout and canonical interleaving order of one audio signal. It is either layout-independent Mono, a canonical non-empty set of named speaker positions, or a bounded 1–64 channel Discrete bus with deliberately absent speaker meaning. Standard Stereo, 5.1(side), 5.1(back), and 7.1 are canonical named-speaker values rather than channel-count aliases; channel capacity is always derived from the layout.
_Avoid_: Bare channel count, unordered speaker vector, treating Discrete channels as speakers, `5.1` without side/back meaning, representation mistaken for Adapter execution support
_Avoid_: Bare channel count as signal meaning, device channel index, guessed six-channel ordering

**Timeline Time**:
A normalized exact rational offset interpreted within its owner's declared Authoring Time Domain, independent of frame, sample, or display grids.
_Avoid_: Frame number as universal time, floating-point seconds, fixed subframe ticks

**Sequence Author Revision**:
A persisted nonzero monotonic generation of one stable Sequence identity. Every committed Author Transaction that changes that Sequence, plus Undo or Redo that restores its content, advances it. Project-only transactions, file-save attempts, and playhead-only navigation do not rewrite it; Project-wide execution invalidation uses Author Generation plus the exact resolved Project semantics.
_Avoid_: Project document save revision, frame-demand sequence, content hash presented as a transaction generation

**Authoring Time Domain**:
The coordinate origin and mapping owned by a Sequence, Clip, Audio Component Edit, Audio Processing Scope, Transition, source, or other time-bearing author entity.
_Avoid_: Renderer frame grid, audio block, implicit clip-local flag

**Time Transform**:
A validated exact mapping between two Authoring Time Domains created by placement, trimming, speed mapping, or nesting.
_Avoid_: Rewriting a time base, floating-point seconds bridge, implicit cross-domain comparison

**Evaluation Grid**:
The consumer-specific frame, shutter-sample, audio-sample, or parameter-event instants at which authored semantics are evaluated.
_Avoid_: Persisted authoring time base, UI snap setting

**Parameter Schema**:
One versioned definition-stable contract for a parameter's `ParameterId`, value type, definition default, automation capability, unit, numeric or enum constraints, admitted Hold/Linear/Bezier execution semantics, localization message identity, and cache impact. Editor presets such as Auto Bezier and Ease author Bezier handles and are not separate execution semantics. Processor execution capabilities remain on the Processor/Effect Definition and compiled graph.
_Avoid_: Instance property path as identity, UI-only min/max, duplicated CPU/GPU or color-domain claims

**Parameter Instance Address**:
A current authoring and command-routing address for one parameter instance, which may include an Effect or owner ID and may change without changing its Parameter Schema identity.
_Avoid_: ParameterId derived from display name, suffix matching during execution

**Parameter Resource Reference**:
A recoverable typed parameter value representing unbound intent, a Project Asset, an external file, or a URI, with resource-level invalidation semantics.
_Avoid_: Free-form path text treated as a loaded resource, persisted resolved/available flag

**Display Timecode**:
A presentation contract for formatting timeline positions with a start offset, nominal rate, and drop/non-drop-frame rules.
_Avoid_: Timeline storage coordinate, arithmetic duration, Clock Master

**Audio Component Edit**:
One persistent, placement-local selection and edit of a media audio component or nested Sequence Output, owned by exactly one Timeline Clip. It owns enable/Role, edit-local gain/pan/fades/automation, and a restricted binding into an Audio Processing Scope; Track and Sequence range are always derived from the owning Clip.
_Avoid_: Duplicated Track/range placement, entire audiovisual Clip treated as one audio stream, routing Bus

**Audio Processing Scope**:
One Sequence-owned shareable audio processing definition containing input trim/automation and an ordered Processor Rack. Multiple Audio Component Edits may bind to it with only a Scope ID and exact `scope_in`; it never owns placement, speed, source selection, or output routing.
_Avoid_: Second Clip, arbitrary time transform, implicit copy-on-write

**Compiled Audio Contribution**:
One non-persistent PCM-bearing execution branch derived from an owning Track/Clip plus one Audio Component Edit and its Audio Processing Scope.
_Avoid_: Persisted placement authority, user-routable graph node

**Prepared Audio Schedule**:
One immutable Render-Contract-bound lowering of compiled audio semantics into dense topological node slots, destination-contiguous Route and Contribution ranges, Transition bindings, exact sample spans, validated automation event spans, liveness-assigned scratch slots, admitted processor-private Session storage, and a selected processor kernel backend. It is execution data, never author data, and a Render Session may scan neither author collections nor routing maps after preparation.
_Avoid_: Compiled Audio Program, runtime graph wrapper

**Audio Program**:
The Sequence-owned persistent authoring model for mixer channels, mix buses, typed routes, processors, automation, and exposed outputs.
_Avoid_: Project-global PCM graph, Track as mixer and bus

**Audio Routing Node**:
A persistent user-addressable Track Mixer Channel, Mix Bus, or Program Output with type-specific identity, ports, and lifecycle in one Audio Program routing model.
_Avoid_: Generic property node, universal untyped ID, generated Clip DSP operator

**Audio Route**:
A persistent typed connection between stable signal endpoints in one Audio Program.
_Avoid_: Node-name connection, array-index connection, hidden fallback route

**Audio Processor Instance**:
A persistent built-in or external audio effect instance identified by a stable definition. Each parameter captures the shared Parameter Schema plus one exact-time curve whose default is the unkeyed value, so unavailable external processors preserve editable author intent without a second static-value truth.
_Avoid_: Video EffectNode, plugin file path, registry index, opaque JSON effect

**Audio Processor Rack**:
An ordered collection of Audio Processor Instances at one explicit insertion point of an Audio Processing Scope, Track Mixer Channel, Mix Bus, or Program Output.
_Avoid_: Unordered effect set, format-specific VST chain, hidden master effect

**Generated Audio Processor Occurrence**:
One non-persistent prepared execution instance derived from an authored Audio Processor Instance plus its exact Contribution or Routing Node owner and insertion point. A shared Processing Scope may produce several occurrences; each owns independent mutable Session state.
_Avoid_: Sharing DSP state because author definitions match, flattening a Rack into aggregate gain, processor array index as identity

**Audio Processor Tail**:
The declared meaningful output extent after a Processor's input becomes silent: none, finite non-zero sample frames, or infinite. It excludes hidden algorithmic group delay/lookahead, which is a separate PDC fact.
_Avoid_: Latency alias, guessed reverb timeout, zero-valued finite sentinel

**Audio Public Output Lookahead**:
The bounded internal sample-frame lead required to return one Program Output already aligned to its public Timeline coordinate. It is admission and capacity evidence, never public PCM delay or author-time offset.
_Avoid_: Leading silence exposed to consumers, nested latency propagated after normalization, device latency

**Audio Parameter Event Batch**:
A borrowed, Session-backed block of stable Parameter-ID lanes and exact sample-offset value events for one Generated Audio Processor Occurrence. Static values use one offset-zero event; varying curves are lowered sample-accurately without callback allocation.
_Avoid_: UI-rate parameter polling, per-block heap construction, plugin parameter index as semantic identity

**Audio Transition**:
An explicit Sequence-time-bounded relationship between two Audio Component Edits that applies a declared pair of sample-domain gain curves after their component/scope-local processing.
_Avoid_: Overlap inferred as crossfade, unary fade plugin, Track-wide dissolve

**Audio Role**:
A Sequence-owned single-valued semantic classification for an Audio Component Edit, optionally parented to another local Audio Role; “role” and “subrole” describe hierarchy position rather than different entity types.
_Avoid_: Project-global role object, routing node, display name as identity

**Standard Semantic Key**:
An optional namespaced classification value used for templates, matching, search, and presentation without replacing a Sequence-local Audio Role identity.
_Avoid_: Global Role ID, dynamic routing selector, mutable Project authority

**Sequence Semantic Catalog**:
The Sequence-root collection that owns Audio Roles and other authoring semantics shared by its Timeline, Audio Program, and public output interface.
_Avoid_: Project role registry, Mix Graph node collection

**Strong Entity Reference**:
A typed reference that must resolve inside the same validated authoring aggregate for a snapshot to exist.
_Avoid_: Missing internal node treated as runtime availability

**Recoverable Dependency Reference**:
A stable cross-aggregate or external reference that retains its expected contract while current binding remains derived.
_Avoid_: Missing internal Route endpoint, persisted resolved flag

**Generated Audio Stage**:
A non-persistent IR operation deterministically lowered from Clip, Lane, Transition, Route, or processor authoring semantics.
_Avoid_: User-routable Clip node, anonymous diagnostic stage

**Audio Render Admission**:
A per-execution decision that requested logical audio outputs, exact artifacts, processors, decoders, state, and buffers are prepared.
_Avoid_: Audio-device availability, deadline guarantee

**Monitor Sink Admission**:
A per-listening-path decision that a user- or workspace-scoped monitor graph is bound and prepared to feed one physical device.
_Avoid_: Program Output compatibility, transport permission

**Signal Closure**:
The resolved typed audio dependency subgraph required to produce selected logical outputs under one processing contract.
_Avoid_: Consumer purpose, scheduler window, hash alone

**Audio State Domain**:
The mutable DSP state scope determined by a Signal Closure, continuity epoch, processor origins, nested instance path, time mapping, direction, and processing mode.
_Avoid_: Global plugin state, cache entry, fingerprint-equivalent graph

**Sequence Output Port**:
A stable typed audio output explicitly exposed by one Sequence for nesting, playback, analysis, or delivery.
_Avoid_: Child track reference, output array index, implicit stereo mixdown

**Delivery Mapping**:
A Project or export-job contract that packages selected Sequence outputs into files, containers, channels, and delivery metadata.
_Avoid_: Internal stem selection, physical device routing

**Monitor Path**:
A user-, workspace-, or session-scoped graph that maps logical program outputs to physical listening devices without changing the Audio Program.
_Avoid_: Program master processor, project-owned device ID

**Authoring Session**:
The sole mutable authority for one open Project lifetime: canonical Project Document, Asset Library authority, Sequence navigation, bounded project-wide history, author generation, and manual/autosave baselines.
_Avoid_: AppState fields mirroring Project data, mutable renderer snapshot, active-Sequence-only history

**Author Generation**:
A process-local monotonic identity assigned to every committed Project author state in one Authoring Session. It is neither a Sequence execution revision nor a successful-save revision.
_Avoid_: Document revision, Sequence revision, frame generation

**Author Transaction**:
A candidate transformation of a Sequence or complete Project Document that becomes canonical only after validation, history admission, revision advancement, and atomic installation succeed.
_Avoid_: Mutate then record, best-effort rollback, UI-owned command side effect

**Durable Project Publication**:
The filesystem boundary at which a flushed and validated Project archive or recovery manifest atomically replaces or creates its target. Completion before this boundary is not a successful save.
_Avoid_: ZIP writer close alone, temporary-file existence, enqueue success, rename-old-then-rename-new sequence

**Clip Content**:
The one closed payload that identifies a Clip placement as Media, Adjustment Layer, Nested Sequence, Solid Color, or Basic Title and carries only that variant's external references, interpretation data, or generated-source author state.
_Avoid_: Parallel Clip kind/asset/nested/color/title fields, fake Asset ID for a nested Sequence or generated source

**Clip Visual Author Time**:
The stable exact Clip-local coordinate used by every Clip-owned visual property: Transform, Opacity, visual Effects, Masks, and generated visual content. Moving a Clip or changing its source selection/speed preserves this coordinate; trimming away the in edge, splitting, or creating a right-hand fragment advances the visible `clip_time_in` by the removed placement duration.
_Avoid_: Sequence time used directly for Clip properties, source time used as visual automation time, independently chosen time domains per visual subsystem

**Basic Title**:
A Sequence-local generated Clip content type whose closed definition-backed Property Bag is evaluated in Clip Visual Author Time and rasterized as tightly cropped straight-alpha working-linear picture before the ordinary Clip transform/effect/mask/composite path.
_Avoid_: Asset-library text generator, renderer-only TextLayer, text Effect, implicit system-font fallback

**Basic Title Font Dependency**:
The exact requested system font family plus the resolved face bytes/index fingerprint used by one generation Session. Missing families, inaccessible bytes, or an undeclared fallback face are recoverable execution blockers and never authorize visually different substitution.
_Avoid_: Font display name as output fingerprint, platform default fallback, silent emoji/CJK fallback chain

**Clip Link Group**:
A Sequence-local set identity shared by two or more Clip placements whose ordinary editorial selection and structural edits are synchronized.
_Avoid_: Pair pointer, linked-list chain, singleton group, media ownership relation

**Video Transition**:
A Sequence-owned, typed two-input visual author entity with strong adjacent Clip endpoints, an exact Sequence-time interval, stable instance identity, and validated source-handle demand.
_Avoid_: Clip opacity preset, Track-owned transition, inferred overlap, clamped source request

**Project Migration**:
An ordered, transactional transformation of one persisted archive, document, or SQLite schema version into the next supported version.
_Avoid_: Best-effort deserialization, ignored ALTER error

## Relationships

- A **Playback Session** has exactly one active **Clock Master**.
- A **Playback Session** has exactly one **Transport State** at a time.
- One user Play or Seek intent applies its Sequence identity, semantic revision, evaluation grid, content boundary, position, and resulting Transport State atomically and rotates exactly one playback epoch; an App Adapter cannot expose intermediate reset/seek/play states.
- A **Playback Session** produces zero or more **Frame Demands**.
- Each **Frame Demand** produces at most one terminal **Frame Delivery**.
- The current **Frame Demand** is the sole source of truth for terminal identity and target. Playback position changes refresh that demand atomically; the Engine cannot retain a parallel target field that can diverge under Audio Device Clock updates.
- A **Frame Request Binding** is resolved atomically at completion. If the same semantic frame key is rebound while work is in flight, a reusable result adopts the latest binding; a canceled or incompatible old execution leaves the newer binding pending.
- Every queued or executing frame request belongs to exactly one **Frame Work Broker** lifecycle. Admission and queue capacity cannot disagree, and only an execution lease or explicit synchronous Adapter completion may resolve its Frame Request Binding.
- **Execution Work Intent** is shared language, not shared scheduling ownership. Every consumer Module must define its own bounded admission, cancellation checkpoints, resource budget, publication rule, and domain evidence; Export may never contend in a realtime pool merely to reuse the Interface.
- Every admitted background analysis attempt reaches exactly one **Execution Terminal Evidence** disposition. Rejected admission, dependency failure, cooperative cancellation, and stale publication are distinct and cannot collapse into a missing cache entry.
- `app::preview_access_mode` is the application scheduling Adapter over the **Frame Work Broker**. It owns media keys, access-mode admission, bounded job transport, and worker-lane mapping without depending on Widget or Window modules; `app_ui` consumes it and cannot host a second scheduler.
- `app::preview_scheduler_policy` is the sole application policy for Frame Demand deadline classification, executed decode quality, frame-local hardware recovery signals, bounded playback prefetch depth, scrub-locality/latency adaptation, and the consecutive-late decode-pressure guard. The production Adapter supplies one explicit monotonic observation instant per scrub request; UI code cannot own adaptation thresholds or sample time internally. These local guards may alter decode strategy or suppress speculative/duplicate work but cannot enter Playback Transport `Recovering`, change Clock Master, mutate authored quality, or accept an inexact settled frame. Preview UI Adapters consume these decisions and increment diagnostics but cannot reconstruct hardware fallback or redefine `Ready`, `Degraded`, or `Late`.
- The **Frame Work Broker** records the selected worker lane on the execution lease at dequeue and derives priority, class, lane residency, and cross-lane evidence from that same lifecycle state. Adapters may rename fields for their report schema but cannot maintain parallel activity counters.
- The **Frame Work Broker** derives each execution's cancellation disposition, request age, and execution-lease age from one Monotonic Runtime Clock sample under one lifecycle lock. Adapters may translate that evidence into domain-specific diagnostics, but cannot reconstruct it from separate freshness, competing-work, codec-entry, or timestamp queries.
- The first **Frame Work Broker** close records one immutable closure instant in its **Monotonic Runtime Clock**; every executing lease observes `BrokerClosed` with age from that instant. Repeated close cannot renew the cancellation budget, and an Adapter stop flag has no timing authority.
- The **Frame Work Broker** samples one **Monotonic Runtime Clock** Adapter exactly once for each atomic lifecycle operation that establishes or compares time. A regressing Adapter sample is clamped to the last observation and recorded as fail-closed evidence; it can never move an age backward or masquerade as valid timing proof.
- A **Frame Work Deadline** is lowered exactly once at Broker admission. Rebinding the same in-flight key replaces its deadline with the latest binding, while cancellation reports the earliest applicable deadline, invalidation, or preemption request.
- A worker records completion in the **Frame Work Broker** before crossing its result channel. Later UI polling resolves freshness against the latest binding but cannot change the recorded on-time/missed deadline result.
- A media **Adapter** contributes the Broker-owned execution-lease age at first checkpoint and return exactly once to **Frame Cancellation Evidence**. `app::preview_access_mode` exclusively maps the Broker's atomic disposition, access mode, and bounded speculative budget into Adapter cancellation reasons and Playback work classes; workers and UI reports cannot reconstruct timing from the later codec-function entry instant. The Playback Module alone aggregates causes and evaluates request-to-checkpoint plus class-specific checkpoint-to-return budgets.
- The concrete FFmpeg Adapter returns exactly one **Preview Decode Cancellation Fact** when cancellation becomes effective. `AVIOInterruptCB` records the first active in-process `input_open`, `stream_info`, `seek`, or `packet_read` checkpoint only when that call returns; ordinary cache, codec, frame-materialization, and external-process checks remain cooperative facts. Killing and reaping an isolated demux helper is a distinct `IsolatedDemuxTermination` fact and poisons that packet source; it must never be reported as callback interruption or reusable format state. Reused Sessions reset observation per request. App Runtime evidence may aggregate the fact but may not replace Broker cause/timing evidence with it.
- The **Playback Preview Pump** samples the still-pending Frame Demand identity once per runtime turn. A Preview Adapter reports execution facts against that sample but cannot apply Frame Deliveries or mutate Transport State; Window and Headless consumers receive the same pump outcome.
- The **Playback Preview Pump** applies terminal Frame Deliveries before observing video preroll. A delivery that ends or replaces a demand cannot be followed in the same turn by readiness attributed to that superseded demand.
- If independent bounded completion sources report the same terminal authority in one pump turn, the first accepted fact consumes it and later losing facts are retired before Playback Evidence; an expected internal race is not a rejected terminal observation.
- Successful decode/cache completion is nonterminal readiness. A CPU or GPU **Presentation Adapter** must complete the exact **Frame Presentation Ticket** only after it has produced a usable Viewer output; the Playback Module compares the real completion timestamp with the ticket deadline and emits `Ready`, `Degraded`, or `Late`. Cancellation/failure paths may terminate earlier without presentation.
- A Viewer stale lifecycle state does not terminate a **Frame Demand**; only a deadline/policy decision may emit `StaleAvailable`, while an in-flight worker retains the chance to deliver `Ready`.
- A **Playback Quality Policy** constrains every **Frame Demand** in its Playback Session.
- An active Playback Quality Policy's temporary `Full`/`Half`/`Quarter` scale multiplies the user-authored preview scale at every Preview Adapter boundary; paused/stopped still-frame work returns to the authored scale. It changes output extent only and never changes proxy/original selection or color interpretation.
- Preview scale normalization has one application-layer owner shared by author mutation, Window execution, and Headless execution; UI formatting cannot introduce a parallel clamp or invalid-value fallback.
- Native-video playback admission has one application-layer snapshot for request, device selector, surface-format support, and blocker facts. Window, Headless, and Preview diagnostics project that snapshot and cannot maintain independent admission booleans or downgrade rules.
- A correct CPU frame produced after hardware decode was explicitly requested but did not execute is a presentable `Degraded` **Frame Delivery**. It contributes recovery pressure; observed hardware decode with CPU transfer and native GPU-resident decode remain `Ready` paths.
- Executed decode quality is stored on the decoded frame itself and survives prefetch and Preview Frame Store reuse; it is aggregated across the final composition before creating a **Frame Presentation Ticket**. Job identity is not a substitute for execution quality.
- **Playback Evidence** records state and clock transitions without owning them.
- **Playback Evidence** uses bounded versioned events and aggregates from real Frame Demand, Frame Delivery, Clock Master, seek, and Audio Playback observations; capability probes alone cannot satisfy execution gates.
- **Playback Evidence** detailed-event eviction is normal bounded retention, not observation loss. Metric population count and maximum cover the entire run exactly; percentile estimates use a declared fixed-capacity deterministic reservoir.
- **Process Memory Evidence** is sampled by a platform Adapter over the same real-cadence observation window as professional playback. Acceptance compares fixed settled windows, enforces a versioned absolute Private Commit cap, and samples again after the gate's declared terminal stress has reached quiescence; an unsupported probe fails closed. A video gate may define that stress as a latest-wins seek burst, while an audio gate may define it as completed long-run playback plus worker quiescence.
- **Professional Playback Acceptance** fails closed when observed **Frame Cancellation Evidence** has an unknown cause, lacks request/checkpoint attribution, contains impossible timestamp ordering, observes a request too late, or returns after its work-class budget.
- Every Clock Master observation or handoff that changes the authoritative timeline frame must publish a matching current **Frame Demand** in the same Engine transition; position, demand target, and delivery identity cannot temporarily diverge.
- **Professional Playback Acceptance** accepts HEVC Main10 only when FFmpeg proves the codec profile, dimensions, bit depth/pixel format, supported exact frame rate, primary-video stream duration, and any declared frame count. Container duration cannot substitute for primary-stream coverage; unknown probe values remain unknown and fail the contract.
- A versioned professional acceptance profile owns its cadence, duration, timeout, latency, readiness, visibility, and hardware-execution thresholds. Developer environment variables may tune manual smoke tests but cannot weaken that profile.
- `app::playback_acceptance` consumes only UI-independent execution evidence. A performance or Window Adapter may project its diagnostics into that Interface, but UI report schemas cannot become acceptance policy dependencies.
- A canonical **Reference Corpus** contains only self-owned, deterministically generated, public-domain, or explicitly licensed material. Media with unverified rights may be used only by ignored local diagnostics and can never satisfy release or professional acceptance.
- A generated Reference Corpus entry fixes its recipe identity and semantic probe contract, not an encoder-version-dependent artifact hash. Every **Reference Playback Run** regenerates or verifies an attested artifact, then pins its actual bytes in that run's evidence.
- A baseline-eligible **Reference Playback Run** contains the complete gate set, uses one qualified operator-assigned machine identity, begins and ends at the same clean Git revision, and carries passing asset, machine, and structured execution reports. Hardware serials are neither necessary nor permitted.
- Decode execution provenance lives on the decoded frame and survives playback-ring, global preview cache, prefetch, and Preview Frame Store reuse. Acceptance coverage counts only provenance attached to Viewer candidates that complete through the headless GPU Presentation Adapter; aggregate prefetch diagnostics cannot satisfy it.
- A **Preview Frame Store** admits and evicts decoded payloads by entry count, host bytes, and opaque decoder-resource units. It may retain bounded native decoder leases for playback prefetch, but renderer import tables, bridges, output textures, and presentation resources remain outside it and require their own evidence.
- A stopped **Preview Production Runtime** may release media-only Frame Store residency only when its Broker and execution intent are quiescent and a registered GPU Viewer output is exact for the active generation. Acceptance of that release additionally waits for every decode worker to publish `Idle`, zero active helpers, and launch-to-post-reap terminal accounting; a fixed sleep is never retirement evidence. The GPU output, any Viewer stale pin, and failure memory survive; active Playback, pending still work, or an old-generation output must fail closed. CPU raster presentation retains media until it gains an equivalent generation-proved reuse seam; it must not invoke the GPU-only release rule optimistically.
- Each production Preview worker owns exactly one **Preview Decode Execution Progress** observer for its lifetime. A non-cloneable, thread-safe bootstrap is consumed to construct the non-`Send` FFmpeg context on that worker; only read-only observer clones cross back to App diagnostics. Media call sites publish before entering potentially blocking FFmpeg operations, while the FFmpeg callback publishes every poll and whether it observed the current request canceled. The same observer records isolated-demux launch, validated-ready, acknowledged Seek, Packet/EOF Read, cross-request reuse, active/peak process count, and exactly one post-reap terminal class without creating a second Registry. Poll progress, callback cancellation, FFmpeg call return, helper process reaping, and Broker lease completion are distinct facts. The observer may identify a stalled call or an unpolled/ignored interrupt but can never authorize teardown, substitute for a returned execution lease, or claim that an in-process codec call is cancellable.
- The **Preview Demux Isolation Boundary** is the only recovery authority for a non-returning FFmpeg format call. One helper owns one source revision and serves strictly ordered, single-in-flight seek/read commands; it is never a multi-source pool and is not spawned per frame. The parent samples cancellation at most every 250 µs, disconnects bounded IPC, terminates and reaps exactly that helper, poisons the paired packet source, and reports the active Seek or PacketRead checkpoint before returning. That fact makes the complete packet-source/codec Session ineligible for recovery or reuse; the worker retires it directly rather than flushing hardware codec state that has no valid source to resume. Protocol buffers, packet payloads, side data, codec extradata, keyframe anchors, error text, stderr evidence, and response queue depth are independently capped; the helper may never place OS-native paths on its command line or transfer process-local FFmpeg ownership. EOF leaves the Session seekable, clean close is bounded, command/protocol/FFmpeg failures fail closed, and only a new process with a new nonce may reopen a changed or poisoned source.
- Exact Preview output reuse is bound at least to Sequence revision, Author Generation, playback epoch or stopped frame, output extent, seek intent, display contract generation, and the exact resolved Project engine plus Sequence program color context. A change in any of those facts requires re-evaluation before an old output can become current. Process-global OCIO reload generation is operational evidence, not semantic cache identity.
- `app::preview_media_frame` owns decoded Preview payload residency, lazy CPU working adaptation, frame-local quality/provenance, sampled versus logical geometry, and exact host/decoder reservations. Each frame contains exactly one closed residency payload—working CPU, source-domain CPU/GPU-capable, or native decoder surface—so empty and contradictory combinations are unrepresentable. `app::preview_media_task` constructs it from the concrete decode result; Widget and Window resource types cannot enter it.
- `app::preview_media_source` is the single Preview source-interpretation boundary. It owns source/proxy fingerprinting, color/range/Alpha interpretation, native-surface classification, proxy-generation intent, and canonical decode geometry; Window code only looks up the `AssetRecord`, dispatches the returned intent, and projects typed outcomes into evidence.
- A **Media Decode Target** is one canonical nonnegative source-local **Timeline Time** retained unchanged by the Render Plan, Preview Frame Store key, Broker job, media request, and Export decode cache. Only an explicit media frame-rate override quantizes once onto its declared source Evaluation Grid; nested Sequence consumers project exact child-local time onto the child grid. The FFmpeg Adapter alone lowers the target to stream PTS with checked nearest rounding and the declared stream start PTS. Floating seconds and microsecond keys are not authority.
- `app::preview_timeline_execution` owns **Preview Timeline Execution** for Window and Headless consumers. It is the only Preview traversal and nested-composition implementation; Adapters supply typed media readiness, consume its canonical media-demand collection, and may record returned facts but cannot duplicate recursion, child sizing, working-space conversion, or cache identity.
- `renderer::BasicTitleRasterizer` is the shared Basic Title generation Interface for Preview and Export. Sequence resolution plus the persisted total title-safe margin define layout; exact face resolution/shaping and bounded raster caching remain Session-owned. Preview schedules this work on its bounded title worker, while Export reuses one raster Session for the complete job and nested closure.
- **Preview Output Unavailability** is exhaustive at every production output boundary: absence of an active output target is `NoContent`; unresolved dependencies and invalid display/color contracts are `Blocked`; an admitted decode, composite, color, or packaging execution error is `Failed`. Empty active root and nested Sequences contribute exact transparent Program pixels rather than becoming unavailable. Any terminal unavailability clears current and pinned stale output; only `Pending` may reuse a same-scope prior frame.
- `app::preview_frame_store::PreviewFrameStoreAdapter` is the sole Frame Store policy Adapter inside `PreviewProductionRuntime`, over the playback-owned generic Store. Window and Headless specializations share it; `app_ui` cannot own another cache or residency policy.
- `app::preview_raster_frame` owns the validated RGBA8 extent, encoded color identity, exact byte reservation, and stable resource key for a final CPU Preview raster. Cache and stale-reuse paths retain that application contract; the Window Presentation Adapter performs the only conversion to `ViewerFrameImage`, sharing the pixel allocation rather than copying it.
- The **Waveform Analysis Service** is the sole product owner of Timeline waveform execution. Its key includes asset identity and source revision; project rebinding rotates generation, relink/removal can evict one asset, canceled/stale results cannot populate the success or failure cache, and paint/layout code can only issue a nonblocking lookup.
- The **Thumbnail Execution Service** is the sole product owner of asset-thumbnail work. Its exact key includes source path/fingerprint plus resolved input/range/working/output color contract; generation and pending ownership gate publication, raster bytes and entries are independently bounded, and the Window Adapter performs the only zero-copy conversion to a Widget image.
- The **Proxy Generation Service** is the sole App owner of derived proxy demand. Same-key Import work can be promoted by Playback recovery or explicit user intent without duplication; cache-root `concurrent_jobs` is acquired before a request becomes Running, project rebinding cancels queued and FFmpeg-active attempts, automatic exact failures are retained while an explicit user request may retry, and only current-generation completion publishes success.
- `app::preview_viewer_plan` owns the **Preview Viewer Plan** representation, stable cache identity, frame-local quality/provenance aggregation, and GPU lowering. It is pure and UI-independent; Window and Headless Adapters cannot rebuild these rules.
- `app::preview_cpu_execution` owns **Preview CPU Execution** for both nested working-linear output and final CPU raster output. Its result retains input/output/monitor color facts, composite diagnostics, and execution durations; Window and Headless Adapters may record or assert those facts but cannot execute alternate color/composite rules.
- A **Viewer GPU Preview Runtime** owns GPU execution resources independently of a Window; production Window and headless validation must adapt the same execution lifetime and must not duplicate color or compositing interpretation.
- Window UI refresh has two non-interchangeable domains. Author, project, preferences, and workspace changes may refresh complete panel models; Preview candidate, registration, clearing, and Frame Delivery changes update only the Viewer/transport presentation projection. Neither domain may replace persistent shell-chrome Widget identities during an active Window Session; title, menu availability, and shortcut models update in place so hover, press, focus, pointer capture, and overlays remain UI-owned transient state.
- Window presentation becomes usable after external-texture registration and ordered submission to the same GPU queue used by the subsequent Viewer draw; it does not claim fence completion. Headless validation credits readiness only after the real GPU submission completes. Both complete the same **Frame Presentation Ticket**, and device capability alone is not execution evidence.
- **Audio Playback** may offer an Audio Device Clock Master only after stream health, PCM preroll, and media phase satisfy Playback Policy.
- A healthy active audio stream with temporarily stale callback evidence is **Uncertain**, not lost: the current Audio Device Clock Master may coast for one bounded grace interval without advancing consumed samples. Explicit device/stream failure is **Unavailable** and selects Synthetic Clock Master immediately; grace expiry also hands off continuously to Synthetic.
- **CPAL Output Evidence** proves that a concrete operating-system output stream repeatedly consumed production PCM at a bounded callback cadence. It is not acoustic loopback evidence and cannot prove the waveform reached a physical connector, amplifier, or speaker.
- Every **Audio Sample Position** carries its sample rate. Positions at different rates cannot be compared or subtracted without an explicit resampling Adapter.
- An **AudioDecodedSource** fills exact interleaved **Decoded Audio Source Windows** and may fail the whole block; it cannot publish a partial shifted block. `mondrian-audio` owns only a small aligned hot window, while the media Adapter owns fingerprinting, decode, weighted LRU residency, and failure memory.
- Every Sequence, Audio Render Contract, PCM render request, decoded source cache, and PCM buffer carries one **Audio Signal Layout**. Capacity is derived from that layout. The DSP core can preserve named/custom and Discrete buses, but each media, plugin, device, and export Adapter must negotiate an exact lowering; current FFmpeg execution admits only explicit mono/stereo/5.1(side) matrices and may not infer semantics from a channel count.
- An **Audio Render Session** crosses its source Adapter Seam once per active Generated Audio Contribution block using ordered absolute source-frame coordinates; reverse, repeated, and non-contiguous mappings cannot force a per-sample Interface or move time-mapping authority into media decode.
- A **Prepared Audio Schedule** is the only graph representation interpreted by an **Audio Render Session**. Preparation assigns dense node/contribution/scope slots, contiguous incoming Route ranges, exact active sample spans, Transition bindings, scratch liveness, and the CPU kernel backend; author maps and general Route searches cannot enter block execution.
- CPU scalar reference and runtime-selected SIMD implementations consume the same **Prepared Audio Schedule** and must produce identical PCM for the supported processor set. SIMD is an execution choice, never a second semantic compiler.
- Concurrent misses for the same complete **Decoded Audio Source Window** have one decode leader. Persistent media decode Sessions are bounded by source revision plus output contract; sequential reuse, cold open, and random restart are distinct evidence classes and cannot share one latency claim.
- Persisted timeline positions, ranges, automation keys, and temporal handles use **Timeline Time** in an explicit **Authoring Time Domain**; video frames and audio samples are derived **Evaluation Grids**, not competing author time systems.
- A **Sequence Author Revision** is the conservative invalidation generation for every author entity owned by that Sequence. Project document revision records successful file saves and can never substitute for it in Playback, Preview, audio compilation, or render-cache identity.
- One open Project has exactly one **Authoring Session**. Production UI and execution code receive read-only references or immutable snapshots and cannot borrow the canonical Project Document mutably.
- An **Author Transaction** edits a detached candidate. Failure leaves the canonical document, **Author Generation**, Sequence revisions, dirty state, and Undo/Redo stacks unchanged; success advances them as one commit.
- A persistence request binds one Authoring Session identity, **Author Generation**, Asset Library revision, and request identity. A stale or previous-session completion may not clear dirty state or replace newer in-memory metadata.
- Manual save and autosave share **Durable Project Publication**, but only manual save advances the durable baseline; autosave advances recovery coverage and never makes an unsaved Project appear saved.
- Every Clip has exactly one **Clip Content** variant. Unknown or legacy parallel content fields fail current-schema loading rather than being ignored.
- Every non-null **Clip Link Group** has at least two members. Commands expand a selected member to the complete set, validate all locked Tracks and zero-boundary constraints before mutation, and compact broken/singleton membership before commit.
- A **Video Transition** derives Track membership from its strong Clip endpoints. It persists no parallel Track identity, requires one exact shared cut, and projects its full interval into both source domains without clamping; insufficient source handles fail closed.
- A visual Transition author type, definition, or Property Bag is not execution evidence. Product support requires the shared Preview/Export render projection and verified backend path.
- Undo and Redo restore author content but always advance the **Sequence Author Revision**; restored content can therefore never masquerade as the older live snapshot from which it originated.
- **Timeline Time** equality, ordering, arithmetic, and hashing use checked canonical rational semantics; serialized numerator/denominator field order can never define chronology.
- Timeline Times from different **Authoring Time Domains** cannot be compared or combined until an explicit **Time Transform** maps one domain into the other.
- Video automation and audio automation share the same exact curve and stable parameter-identity foundation; their Evaluation Grids, supported value types, and delivery cadence remain domain-specific.
- Every renderer-stage exchange uses a **Color Frame Contract**. OCIO transforms process RGB only and require straight/opaque coverage; spatial filtering may materialize a typed premultiplied internal frame but must restore the declared public association before the next Module.
- **Display Timecode** formats a Timeline Time but never owns it; changing drop-frame display or start timecode cannot move authored media.
- One Sequence persists exactly one position-display setting and resolves it with its video Evaluation Grid into one validated display contract shared by Viewer and Timeline ruler. Frames ignore the retained timecode origin; drop-frame is valid only for its exact supported rational rates, and neither negative positions nor SMPTE 24-hour label wrapping alter Timeline Time.
- Each Sequence exclusively owns one **Audio Program**; ordinary PCM routes cannot cross Sequence ownership.
- An **Audio Program** presents one typed routing graph of **Audio Routing Nodes** without forcing Track, Bus, and Output to share an untyped identity or lifecycle; a Track Mixer Channel uses its owning audio Track identity.
- An **Audio Route** connects stable typed endpoints and can never target a **Generated Audio Stage**.
- Every independently processable **Audio Processing Scope** and every **Audio Routing Node** may own explicitly placed **Audio Processor Racks** using the same **Audio Processor Instance** author model for built-ins, VST3, CLAP, and future host adapters.
- An **Audio Processor Instance** addresses automation by stable instance and parameter identity; display names, property-path suffixes, plugin file paths, and parameter indexes are never authoritative.
- Audio Processor Instance IDs are unique across every Scope/Track/Bus/Output Rack in one Sequence. Preparation preserves Rack order and creates one **Generated Audio Processor Occurrence** per execution owner; sharing an Audio Processing Scope never shares mutable DSP state between Contributions.
- Each Audio Processor parameter persists one validated **Parameter Schema** snapshot and one exact curve keyed by the same `ParameterId`; built-in compilation additionally requires an exact match to its canonical definition schema, while unavailable external dependencies retain the snapshot and opaque state but fail execution closed unless bypassed.
- Every generated processor receives an allocation-free **Audio Parameter Event Batch** whose lanes retain stable `ParameterId` order and whose sample offsets are block-local. ABI normalization and capability reduction belong only to the concrete processor Adapter and must fail closed when exact delivery is unavailable.
- An intentional audible delay is Processor signal semantics, not PDC-compensable algorithmic latency. A Processor may declare nonzero algorithmic latency only when the Host must align hidden lookahead/group delay; meaningful output after silence is instead an explicit **Audio Processor Tail**. Equal sample-history lengths do not make these meanings interchangeable.
- Audio plan preparation checked-sums every realized occurrence's processor-private Session bytes, aggregate PDC delay-line bytes, and **Audio Public Output Lookahead** against explicit **Audio Render Contract** budgets. Session construction must reproduce the admitted totals; overflow, excess, or Factory contract drift fails before callback execution.
- Component automation uses Audio Component Edit-local rational time, Scope automation uses Audio Processing Scope-local rational time, Track/Bus/Output automation uses Sequence-local rational time, and an **Audio Transition** interval is Sequence-local; compilation maps each domain once to exact sample offsets. Execution subtracts each prepared stage's input-signal delay before evaluating its automation or Processor parameter batch, so PDC cannot shift author time.
- An **Audio Transition** names exactly two Audio Component Edits and does not affect other overlapping material; overlap without a Transition remains ordinary summing.
- Every parallel input to a sum or Transition is delay-compensated from declared processor and nested latency; internal floating-point mixing neither normalizes, soft-clips, nor limits without an explicit authored processor.
- Each **Audio Role** belongs to exactly one **Sequence Semantic Catalog**; its optional parent is a **Strong Entity Reference** in the same catalog, and the resulting hierarchy must be acyclic.
- Each separable Audio Component Edit has zero or one authoritative **Audio Role**; ancestors are implied by hierarchy, while independent tags and routing duplication cannot masquerade as additional Role assignments.
- A Track may materialize a default **Audio Role** when authoring an otherwise unclassified Audio Component Edit, but moving that edit does not silently reclassify it; a mixed Bus has no authoritative single Role.
- A **Standard Semantic Key** may suggest matches between Audio Roles in different Sequences but never establishes a reference or changes existing authoring semantics.
- Audio Component Edits, semantic projections, and output-family selectors use local **Audio Role** identities; a Project may derive indexes and coordinate atomic edits across Sequence snapshots but owns no live role graph referenced by them.
- Every **Strong Entity Reference** resolves before an authoring transaction commits; only a **Recoverable Dependency Reference** can produce a runtime resolution issue.
- Every **Generated Audio Stage** retains a deterministic origin reference for diagnostics, caching, and per-instance DSP state allocation.
- Preview, playback, audition, analysis, and export compile the same author semantics into generated operations; they may schedule differently but cannot reinterpret processor order, automation, transitions, latency, or nesting.
- A **Prepared Audio Schedule** propagates algorithmic latency, **Audio Processor Tail**, stage input-signal delay, and state-entry obligations through every Contribution, port-specific Route, and processor rack. The selected Program Output normalizes its total latency into bounded **Audio Public Output Lookahead**; a normalized nested public output enters its parent with zero algorithmic latency while retaining its independent state-entry obligation.
- A stateful **Audio Render Session** begins unentered, accepts a fresh continuity epoch plus exact first sample, and thereafter admits only exact contiguous blocks. Reusing an epoch, skipping coordinates, or continuing after an execution error is invalid; a fresh entry resets Session history. A stateful Contribution occurrence remains pending until its exact causal execution interval begins, then receives contiguous local callbacks through its declared tail; it is never advanced across pre-Clip silence. Discontinuity causes remain coordinator evidence and are not processor-owned rule keys.
- Each stateful nested Sequence instance owns a private continuity epoch stream. For a nondecreasing parent time map, the nested Runtime enters lazily at the first demanded child sample and evaluates every skipped child sample in order, so speed mapping cannot skip processor history. Generic stateful reverse evaluation is invalid until a processor-specific reverse contract, checkpoint replay, or materialized child output proves block-partition-invariant semantics; stateless nested output remains freely indexable in either direction.
- **Audio Playback** attaches one explicit Enter operation to the first PCM window of each render generation and Continue to every later window. The Timeline PCM Adapter maps that generation to the Runtime continuity epoch and rejects duplicate entry, missing entry, generation mismatch, or a non-contiguous sample coordinate; it never infers entry from a changed start sample.
- A PCM renderer declares either independent windows or generation-owned state. Only independent-window failure may become exact-duration silence; failure or PCM-contract violation after generation-owned state has advanced invalidates the whole generation, clears its output, hands Clock Master to Synthetic, and re-enters a fresh generation at Playback's authoritative position. Fresh-generation retries are bounded; exhaustion enters explicit RenderBlocked state until a deliberate reprime boundary.
- CPU scalar, CPU SIMD, isolated plugin, and optional GPU execution are prepared processor backends for the same compiled semantics, not alternate Audio Programs. A GPU backend must declare and account for batch/transfer latency, PDC, state ordering, cancellation, bounded in-flight storage, and device loss; the audio callback never waits for GPU work.
- Equal **Signal Closures** may share immutable plans, but mutable processors share an **Audio State Domain** only when all continuity and evaluation identities also match; fingerprints alone never authorize state sharing.
- An **Audio Program** exposes one or more stable **Sequence Output Ports** and never binds physical listening devices.
- A **Parameter Schema** has one stable identity shared by all instances; each instance has its own **Parameter Instance Address** and author value.
- Effect execution selects parameters by Parameter Schema identity, never by address suffix; UI and commands may route through the current Parameter Instance Address.
- Color/Alpha domain, CPU/GPU support, determinism, temporal extent, and ROI have one owner on the Processor/Effect Definition and compiled graph; Parameter Schema cache impact only determines what must be re-resolved.
- A nested Sequence is one instanced composite audio source in its parent; it consumes selected **Sequence Output Ports** and owns independent mutable DSP execution state.
- A parent binds nested PCM through the child's stable public output identities and records any semantic assignment separately against parent-local **Audio Roles**; it never references child-internal Audio Role identities.
- A **Delivery Mapping** packages Sequence outputs but cannot address private child tracks, buses, or routes.
- A **Monitor Path** consumes logical Sequence outputs and cannot alter program or exported samples.
- **Audio Render Admission** and **Monitor Sink Admission** are independent; a missing Monitor Sink never invalidates an Audio Program or stops Synthetic Clock transport.
- During `Priming`, **Audio Playback** may render and queue PCM but must keep device consumption inactive. Only `Playing` or `Recovering` grants consumption permission; a late/missing video frame cannot revoke it or rotate the audio render generation.
- **Audio Playback** records isolated underruns without changing Clock Master; sustained missing-sample evidence enters recovery through a continuous Synthetic handoff and fresh preroll.
- Each persisted archive, document, and SQLite library has an independent version and **Project Migration** chain.
- A **Project Migration** operates on an in-memory value or runtime copy; opening never rewrites the source archive.

## Example dialogue

> **Dev:** “The Viewer missed frame 240. Should it set playback buffering?”
> **Domain expert:** “No. It reports a late **Frame Delivery**. The **Playback Session** decides whether its **Transport State** keeps playing, primes, or recovers according to the active **Clock Master** and quality policy.”

> **Dev:** “Can Reel 1 route directly into a Project-wide dialogue bus?”
> **Domain expert:** “No. Reel 1 exposes a **Sequence Output Port**; a parent Sequence combines it inside its own **Audio Program**, while the Project only applies a **Delivery Mapping**.”

## Flagged ambiguities

- “Buffering” previously meant startup priming, per-frame decode waiting, and sustained recovery. These are distinct Transport States or Frame Delivery outcomes.
- “Audio clock” previously advanced from `Instant` even when it was not proven to represent consumed device samples. Device sample position and synthetic monotonic time are distinct Clock Masters.
