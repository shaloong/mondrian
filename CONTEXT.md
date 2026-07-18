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

**Execution Cancellation Token**:
Monotonic, cloneable generation authority shared by schedulers, runtimes, and concrete media Adapters. Cancellation never resets a token; a new generation receives a new token. Canceled execution may not populate success caches or terminal failure memory.
_Avoid_: Resettable flags, Adapter-owned generation truth, caching canceled results

**Playback Quality Policy**:
The allowed temporary preview resolution and user-selected proxy/original policy for a Playback Session.
_Avoid_: Quality flag

**Playback Evidence**:
Structured events and aggregates proving clock, scheduling, delivery, degradation, synchronization, and recovery behavior.
_Avoid_: Debug log

**Process Memory Evidence**:
Bounded fixed-cadence observations from a native current-process probe. Private committed bytes are the ownership/plateau metric; resident or working-set bytes are diagnostic because the OS may reclaim them independently.
_Avoid_: Frame-cache byte totals presented as whole-process memory, one start/end sample, OS peak since process launch as gate-local growth

**Professional Playback Acceptance**:
A versioned fail-closed contract that binds decoder-proven media identity to frame-local decode provenance, the exact Viewer candidate for a Frame Demand, and completed GPU presentation evidence.
_Avoid_: Filename-based fixture label, capability-only hardware claim

**Preview Frame Store**:
The bounded runtime store for decoded preview payloads, including CPU pixels or opaque native decoder-resource leases, final Viewer rasters, failure memory, the explicitly pinned current/stale Viewer frame, and an oversize current-media exception that remains visible in evidence. CPU bytes, entry count, and decoder-resource units are independent budgets.
_Avoid_: Entry-count-only cache, treating zero-host-byte native surfaces as free, a residency budget smaller than the configured prefetch window, unbounded last frame, dropped oversize current delivery

**Playback Preview Pump**:
The UI-independent App Module coordinator that samples one pending Frame Demand, applies bounded preview-work completions and expirations, submits their exact terminal Frame Deliveries, then observes current-epoch video preroll in that order.
_Avoid_: Window-owned result policy, Headless-only orchestration, separately sampled demand identities for completion and expiration

**Preview Execution Coordinator**:
The UI-independent App Module that atomically binds complete Viewer intent to one media generation, pending state, executed presentation quality, monotonic candidate identity, and the exact currently registered output. Window and Headless presentation are Adapters over its opaque GPU execution contract; neither may reconstruct candidate/cache lifecycle.
_Avoid_: Window-owned candidate counter, separate Headless output identity, UI-owned generation/pending booleans, cache hit inferred from texture presence alone

**Preview Presentation Module**:
The private Window Adapter Module that selects an exact registered GPU output, exact Viewer raster cache entry, same-scope stale content, deferred playback composite, or explicit CPU output boundary for one resolved Viewer plan. It owns packaging and pinning but no generation, scheduling, cache-residency, or transport authority.
_Avoid_: Redraw-local output priority, UI transport mutation, a second candidate lifecycle, stale reuse across sequence/display/geometry identity

**Viewer GPU Preview Runtime**:
The device-scoped owner of native video import, working-linear compositing, spatial processing, display output, calibration, and current external-texture presentation resources for Viewer execution.
_Avoid_: Window-owned GPU grab bag, separate headless rendering semantics

**Audio Playback**:
The realtime path that owns output-device lifecycle, PCM preroll and consumption, render generations, underrun recovery, and consumed-media-position evidence for a Playback Session.
_Avoid_: Audio clock, UI-owned output stream

**Audio Sample Position**:
One signed integer position on an explicitly identified sample-rate timeline, resolved once from rational timeline time using a declared rounding policy.
_Avoid_: Floating-point seconds passed between audio render stages, sample index without rate

**Decoded Audio Source Window**:
One exact interleaved PCM block for a fingerprinted media component on a prepared Audio Render Contract. Runtime hot windows and media LRU windows may differ in size but preserve the same integer sample coordinates.
_Avoid_: Whole-file PCM as the source Interface, per-sample decoder virtual call, path-only cache identity

**Timeline Time**:
A normalized exact rational offset interpreted within its owner's declared Authoring Time Domain, independent of frame, sample, or display grids.
_Avoid_: Frame number as universal time, floating-point seconds, fixed subframe ticks

**Authoring Time Domain**:
The coordinate origin and mapping owned by a Sequence, Audio Component Edit, Audio Processing Scope, Transition, source, or other time-bearing author entity.
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
One immutable Render-Contract-bound lowering of compiled audio semantics into dense topological node slots, destination-contiguous Route and Contribution ranges, Transition bindings, exact sample spans, validated automation event spans, liveness-assigned scratch slots, and a selected processor kernel backend. It is execution data, never author data, and a Render Session may scan neither author collections nor routing maps after preparation.
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
- `app::preview_access_mode` is the application scheduling Adapter over the **Frame Work Broker**. It owns media keys, access-mode admission, bounded job transport, and worker-lane mapping without depending on Widget or Window modules; `app_ui` consumes it and cannot host a second scheduler.
- `app::preview_scheduler_policy` is the sole application policy for Frame Demand deadline classification, executed decode quality, frame-local hardware recovery signals, bounded playback prefetch depth, scrub-locality/latency adaptation, and the consecutive-late decode-pressure guard. The production Adapter supplies one explicit monotonic observation instant per scrub request; UI code cannot own adaptation thresholds or sample time internally. These local guards may alter decode strategy or suppress speculative/duplicate work but cannot enter Playback Transport `Recovering`, change Clock Master, mutate authored quality, or accept an inexact settled frame. Preview UI Adapters consume these decisions and increment diagnostics but cannot reconstruct hardware fallback or redefine `Ready`, `Degraded`, or `Late`.
- The **Frame Work Broker** records the selected worker lane on the execution lease at dequeue and derives priority, class, lane residency, and cross-lane evidence from that same lifecycle state. Adapters may rename fields for their report schema but cannot maintain parallel activity counters.
- The **Frame Work Broker** derives each execution's cancellation disposition, request age, and execution-lease age from one Monotonic Runtime Clock sample under one lifecycle lock. Adapters may translate that evidence into domain-specific diagnostics, but cannot reconstruct it from separate freshness, competing-work, codec-entry, or timestamp queries.
- The first **Frame Work Broker** close records one immutable closure instant in its **Monotonic Runtime Clock**; every executing lease observes `BrokerClosed` with age from that instant. Repeated close cannot renew the cancellation budget, and an Adapter stop flag has no timing authority.
- The **Frame Work Broker** samples one **Monotonic Runtime Clock** Adapter exactly once for each atomic lifecycle operation that establishes or compares time. A regressing Adapter sample is clamped to the last observation and recorded as fail-closed evidence; it can never move an age backward or masquerade as valid timing proof.
- A **Frame Work Deadline** is lowered exactly once at Broker admission. Rebinding the same in-flight key replaces its deadline with the latest binding, while cancellation reports the earliest applicable deadline, invalidation, or preemption request.
- A worker records completion in the **Frame Work Broker** before crossing its result channel. Later UI polling resolves freshness against the latest binding but cannot change the recorded on-time/missed deadline result.
- A media **Adapter** contributes the Broker-owned execution-lease age at first checkpoint and return exactly once to **Frame Cancellation Evidence**. `app::preview_access_mode` exclusively maps the Broker's atomic disposition, access mode, and bounded speculative budget into Adapter cancellation reasons and Playback work classes; workers and UI reports cannot reconstruct timing from the later codec-function entry instant. The Playback Module alone aggregates causes and evaluates request-to-checkpoint plus class-specific checkpoint-to-return budgets.
- The **Playback Preview Pump** samples the still-pending Frame Demand identity once per runtime turn. A Preview Adapter reports execution facts against that sample but cannot apply Frame Deliveries or mutate Transport State; Window and Headless consumers receive the same pump outcome.
- The **Playback Preview Pump** applies terminal Frame Deliveries before observing video preroll. A delivery that ends or replaces a demand cannot be followed in the same turn by readiness attributed to that superseded demand.
- If independent bounded completion sources report the same terminal authority in one pump turn, the first accepted fact consumes it and later losing facts are retired before Playback Evidence; an expected internal race is not a rejected terminal observation.
- Successful decode/cache completion is nonterminal readiness. A CPU or GPU **Presentation Adapter** must complete the exact **Frame Presentation Ticket** only after it has produced a usable Viewer output; the Playback Module compares the real completion timestamp with the ticket deadline and emits `Ready`, `Degraded`, or `Late`. Cancellation/failure paths may terminate earlier without presentation.
- A Viewer stale lifecycle state does not terminate a **Frame Demand**; only a deadline/policy decision may emit `StaleAvailable`, while an in-flight worker retains the chance to deliver `Ready`.
- A **Playback Quality Policy** constrains every **Frame Demand** in its Playback Session.
- An active Playback Quality Policy's temporary `Full`/`Half`/`Quarter` scale multiplies the user-authored preview scale at every Preview Adapter boundary; paused/stopped still-frame work returns to the authored scale. It changes output extent only and never changes proxy/original selection or color interpretation.
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
- Decode execution provenance lives on the decoded frame and survives playback-ring, global preview cache, prefetch, and Preview Frame Store reuse. Acceptance coverage counts only provenance attached to Viewer candidates that complete through the headless GPU Presentation Adapter; aggregate prefetch diagnostics cannot satisfy it.
- A **Preview Frame Store** admits and evicts decoded payloads by entry count, host bytes, and opaque decoder-resource units. It may retain bounded native decoder leases for playback prefetch, but renderer import tables, bridges, output textures, and presentation resources remain outside it and require their own evidence.
- A **Viewer GPU Preview Runtime** owns GPU execution resources independently of a Window; production Window and headless validation must adapt the same execution lifetime and must not duplicate color or compositing interpretation.
- Window presentation becomes usable after external-texture registration and ordered submission to the same GPU queue used by the subsequent Viewer draw; it does not claim fence completion. Headless validation credits readiness only after the real GPU submission completes. Both complete the same **Frame Presentation Ticket**, and device capability alone is not execution evidence.
- **Audio Playback** may offer an Audio Device Clock Master only after stream health, PCM preroll, and media phase satisfy Playback Policy.
- A healthy active audio stream with temporarily stale callback evidence is **Uncertain**, not lost: the current Audio Device Clock Master may coast for one bounded grace interval without advancing consumed samples. Explicit device/stream failure is **Unavailable** and selects Synthetic Clock Master immediately; grace expiry also hands off continuously to Synthetic.
- **CPAL Output Evidence** proves that a concrete operating-system output stream repeatedly consumed production PCM at a bounded callback cadence. It is not acoustic loopback evidence and cannot prove the waveform reached a physical connector, amplifier, or speaker.
- Every **Audio Sample Position** carries its sample rate. Positions at different rates cannot be compared or subtracted without an explicit resampling Adapter.
- An **AudioDecodedSource** fills exact interleaved **Decoded Audio Source Windows** and may fail the whole block; it cannot publish a partial shifted block. `mondrian-audio` owns only a small aligned hot window, while the media Adapter owns fingerprinting, decode, weighted LRU residency, and failure memory.
- An **Audio Render Session** crosses its source Adapter Seam once per active Generated Audio Contribution block using ordered absolute source-frame coordinates; reverse, repeated, and non-contiguous mappings cannot force a per-sample Interface or move time-mapping authority into media decode.
- A **Prepared Audio Schedule** is the only graph representation interpreted by an **Audio Render Session**. Preparation assigns dense node/contribution/scope slots, contiguous incoming Route ranges, exact active sample spans, Transition bindings, scratch liveness, and the CPU kernel backend; author maps and general Route searches cannot enter block execution.
- CPU scalar reference and runtime-selected SIMD implementations consume the same **Prepared Audio Schedule** and must produce identical PCM for the supported processor set. SIMD is an execution choice, never a second semantic compiler.
- Concurrent misses for the same complete **Decoded Audio Source Window** have one decode leader. Persistent media decode Sessions are bounded by source revision plus output contract; sequential reuse, cold open, and random restart are distinct evidence classes and cannot share one latency claim.
- Persisted timeline positions, ranges, automation keys, and temporal handles use **Timeline Time** in an explicit **Authoring Time Domain**; video frames and audio samples are derived **Evaluation Grids**, not competing author time systems.
- **Timeline Time** equality, ordering, arithmetic, and hashing use checked canonical rational semantics; serialized numerator/denominator field order can never define chronology.
- Timeline Times from different **Authoring Time Domains** cannot be compared or combined until an explicit **Time Transform** maps one domain into the other.
- Video automation and audio automation share the same exact curve and stable parameter-identity foundation; their Evaluation Grids, supported value types, and delivery cadence remain domain-specific.
- **Display Timecode** formats a Timeline Time but never owns it; changing drop-frame display or start timecode cannot move authored media.
- Each Sequence exclusively owns one **Audio Program**; ordinary PCM routes cannot cross Sequence ownership.
- An **Audio Program** presents one typed routing graph of **Audio Routing Nodes** without forcing Track, Bus, and Output to share an untyped identity or lifecycle; a Track Mixer Channel uses its owning audio Track identity.
- An **Audio Route** connects stable typed endpoints and can never target a **Generated Audio Stage**.
- Every independently processable **Audio Processing Scope** and every **Audio Routing Node** may own explicitly placed **Audio Processor Racks** using the same **Audio Processor Instance** author model for built-ins, VST3, CLAP, and future host adapters.
- An **Audio Processor Instance** addresses automation by stable instance and parameter identity; display names, property-path suffixes, plugin file paths, and parameter indexes are never authoritative.
- Each Audio Processor parameter persists one validated **Parameter Schema** snapshot and one exact curve keyed by the same `ParameterId`; built-in compilation additionally requires an exact match to its canonical definition schema, while unavailable external dependencies retain the snapshot and opaque state but fail execution closed unless bypassed.
- Component automation uses Audio Component Edit-local rational time, Scope automation uses Audio Processing Scope-local rational time, Track/Bus/Output automation uses Sequence-local rational time, and an **Audio Transition** interval is Sequence-local; compilation maps each domain once to exact sample offsets.
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
