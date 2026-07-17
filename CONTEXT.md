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
The Playback Module that atomically owns frame-work admission, queued transport, in-flight execution leases, generation invalidation, preemption, deadline dequeue, cancellation, and completion binding while treating media payloads and clock values as opaque Adapter data.
_Avoid_: Independent scheduler and worker queue, key-only worker completion, UI-owned priority rollback

**Playback Quality Policy**:
The allowed temporary preview resolution and user-selected proxy/original policy for a Playback Session.
_Avoid_: Quality flag

**Playback Evidence**:
Structured events and aggregates proving clock, scheduling, delivery, degradation, synchronization, and recovery behavior.
_Avoid_: Debug log

**Professional Playback Acceptance**:
A versioned fail-closed contract that binds decoder-proven media identity to frame-local decode provenance, the exact Viewer candidate for a Frame Demand, and completed GPU presentation evidence.
_Avoid_: Filename-based fixture label, capability-only hardware claim

**Preview Frame Store**:
The bounded runtime store for decoded CPU preview payloads, final Viewer rasters, failure memory, the explicitly pinned current/stale Viewer frame, and an oversize current-media exception that remains visible in evidence.
_Avoid_: Entry-count-only cache, unbounded last frame, dropped oversize current delivery

**Viewer GPU Preview Runtime**:
The device-scoped owner of native video import, working-linear compositing, spatial processing, display output, calibration, and current external-texture presentation resources for Viewer execution.
_Avoid_: Window-owned GPU grab bag, separate headless rendering semantics

**Audio Playback**:
The realtime path that owns output-device lifecycle, PCM preroll and consumption, render generations, underrun recovery, and consumed-media-position evidence for a Playback Session.
_Avoid_: Audio clock, UI-owned output stream

**Audio Sample Position**:
One signed integer position on an explicitly identified sample-rate timeline, resolved once from rational timeline time using a declared rounding policy.
_Avoid_: Floating-point seconds passed between audio render stages, sample index without rate

**Timeline Time**:
A normalized exact rational offset interpreted within its owner's declared Authoring Time Domain, independent of frame, sample, or display grids.
_Avoid_: Frame number as universal time, floating-point seconds, fixed subframe ticks

**Authoring Time Domain**:
The coordinate origin and mapping owned by a Sequence, Audio Contribution, Transition, source, or other time-bearing author entity.
_Avoid_: Renderer frame grid, audio block, implicit clip-local flag

**Time Transform**:
A validated exact mapping between two Authoring Time Domains created by placement, trimming, speed mapping, or nesting.
_Avoid_: Rewriting a time base, floating-point seconds bridge, implicit cross-domain comparison

**Evaluation Grid**:
The consumer-specific frame, shutter-sample, audio-sample, or parameter-event instants at which authored semantics are evaluated.
_Avoid_: Persisted authoring time base, UI snap setting

**Display Timecode**:
A presentation contract for formatting timeline positions with a start offset, nominal rate, and drop/non-drop-frame rules.
_Avoid_: Timeline storage coordinate, arithmetic duration, Clock Master

**Audio Contribution**:
One independently processable PCM-bearing component placed by a Timeline Clip or nested output instance before it enters a Track Mixer Channel.
_Avoid_: Entire audiovisual Clip treated as one audio stream, routing Bus

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
A persistent built-in or external audio effect instance identified by a stable definition and stable parameters, with authored state independent of its loaded runtime.
_Avoid_: Video EffectNode, plugin file path, registry index, opaque JSON effect

**Audio Processor Rack**:
An ordered collection of Audio Processor Instances at one explicit insertion point of an Audio Contribution, Track Mixer Channel, Mix Bus, or Program Output.
_Avoid_: Unordered effect set, format-specific VST chain, hidden master effect

**Audio Transition**:
An explicit time-bounded relationship between two Audio Contributions that applies a declared pair of sample-domain gain curves after their clip-local processing.
_Avoid_: Overlap inferred as crossfade, unary fade plugin, Track-wide dissolve

**Audio Role**:
A Sequence-owned single-valued semantic classification for a separable audio contribution, optionally parented to another local Audio Role; “role” and “subrole” describe hierarchy position rather than different entity types.
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
- A **Frame Request Binding** is resolved atomically at completion. If the same semantic frame key is rebound while work is in flight, a reusable result adopts the latest binding; a canceled or incompatible old execution leaves the newer binding pending.
- Every queued or executing frame request belongs to exactly one **Frame Work Broker** lifecycle. Admission and queue capacity cannot disagree, and only an execution lease or explicit synchronous Adapter completion may resolve its Frame Request Binding.
- Successful decode/cache completion is nonterminal readiness. A CPU or GPU **Presentation Adapter** must complete the exact **Frame Presentation Ticket** only after it has produced a usable Viewer output; the Playback Module compares the real completion timestamp with the ticket deadline and emits `Ready`, `Degraded`, or `Late`. Cancellation/failure paths may terminate earlier without presentation.
- A Viewer stale lifecycle state does not terminate a **Frame Demand**; only a deadline/policy decision may emit `StaleAvailable`, while an in-flight worker retains the chance to deliver `Ready`.
- A **Playback Quality Policy** constrains every **Frame Demand** in its Playback Session.
- An active Playback Quality Policy's temporary `Full`/`Half`/`Quarter` scale multiplies the user-authored preview scale at every Preview Adapter boundary; paused/stopped still-frame work returns to the authored scale. It changes output extent only and never changes proxy/original selection or color interpretation.
- A correct CPU frame produced after hardware decode was explicitly requested but did not execute is a presentable `Degraded` **Frame Delivery**. It contributes recovery pressure; observed hardware decode with CPU transfer and native GPU-resident decode remain `Ready` paths.
- Executed decode quality is stored on the decoded frame itself and survives prefetch and Preview Frame Store reuse; it is aggregated across the final composition before creating a **Frame Presentation Ticket**. Job identity is not a substitute for execution quality.
- **Playback Evidence** records state and clock transitions without owning them.
- **Playback Evidence** uses bounded versioned events and aggregates from real Frame Demand, Frame Delivery, Clock Master, seek, and Audio Playback observations; capability probes alone cannot satisfy execution gates.
- **Professional Playback Acceptance** accepts HEVC Main10 only when FFmpeg proves the codec profile, dimensions, bit depth/pixel format, and 25/30-family frame rate. Unknown probe values remain unknown and fail the contract.
- Decode execution provenance lives on the decoded frame and survives playback-ring, global preview cache, prefetch, and Preview Frame Store reuse. Acceptance coverage counts only provenance attached to Viewer candidates that complete through the headless GPU Presentation Adapter; aggregate prefetch diagnostics cannot satisfy it.
- A **Preview Frame Store** admits and evicts CPU frames by both payload bytes and entry count; renderer-owned GPU resources remain outside this store and require their own budget evidence.
- A **Viewer GPU Preview Runtime** owns GPU execution resources independently of a Window; production Window and headless validation must adapt the same execution lifetime and must not duplicate color or compositing interpretation.
- Window presentation becomes usable after external-texture registration and ordered submission to the same GPU queue used by the subsequent Viewer draw; it does not claim fence completion. Headless validation credits readiness only after the real GPU submission completes. Both complete the same **Frame Presentation Ticket**, and device capability alone is not execution evidence.
- **Audio Playback** may offer an Audio Device Clock Master only after stream health, PCM preroll, and media phase satisfy Playback Policy.
- Every **Audio Sample Position** carries its sample rate. Positions at different rates cannot be compared or subtracted without an explicit resampling Adapter.
- Persisted timeline positions, ranges, automation keys, and temporal handles use **Timeline Time** in an explicit **Authoring Time Domain**; video frames and audio samples are derived **Evaluation Grids**, not competing author time systems.
- **Timeline Time** equality, ordering, arithmetic, and hashing use checked canonical rational semantics; serialized numerator/denominator field order can never define chronology.
- Timeline Times from different **Authoring Time Domains** cannot be compared or combined until an explicit **Time Transform** maps one domain into the other.
- Video automation and audio automation share the same exact curve and stable parameter-identity foundation; their Evaluation Grids, supported value types, and delivery cadence remain domain-specific.
- **Display Timecode** formats a Timeline Time but never owns it; changing drop-frame display or start timecode cannot move authored media.
- Each Sequence exclusively owns one **Audio Program**; ordinary PCM routes cannot cross Sequence ownership.
- An **Audio Program** presents one typed routing graph of **Audio Routing Nodes** without forcing Track, Bus, and Output to share an untyped identity or lifecycle; a Track Mixer Channel uses its owning audio Track identity.
- An **Audio Route** connects stable typed endpoints and can never target a **Generated Audio Stage**.
- Every independently processable **Audio Contribution** and every **Audio Routing Node** may own explicitly placed **Audio Processor Racks** using the same **Audio Processor Instance** author model for built-ins, VST3, CLAP, and future host adapters.
- An **Audio Processor Instance** addresses automation by stable instance and parameter identity; display names, property-path suffixes, plugin file paths, and parameter indexes are never authoritative.
- Clip-local automation uses contribution-local rational time, Track/Bus/Output automation uses Sequence-local rational time, and **Audio Transition** automation uses transition-local rational time; compilation maps each domain once to exact sample offsets.
- An **Audio Transition** names exactly two contributions and does not affect other overlapping material; overlap without a Transition remains ordinary summing.
- Every parallel input to a sum or Transition is delay-compensated from declared processor and nested latency; internal floating-point mixing neither normalizes, soft-clips, nor limits without an explicit authored processor.
- Each **Audio Role** belongs to exactly one **Sequence Semantic Catalog**; its optional parent is a **Strong Entity Reference** in the same catalog, and the resulting hierarchy must be acyclic.
- Each separable audio contribution has zero or one authoritative **Audio Role**; ancestors are implied by hierarchy, while independent tags and routing duplication cannot masquerade as additional Role assignments.
- A Track may materialize a default **Audio Role** when authoring an otherwise unclassified contribution, but moving that contribution does not silently reclassify it; a mixed Bus has no authoritative single Role.
- A **Standard Semantic Key** may suggest matches between Audio Roles in different Sequences but never establishes a reference or changes existing authoring semantics.
- Timeline contributions, semantic projections, and output-family selectors use local **Audio Role** identities; a Project may derive indexes and coordinate atomic edits across Sequence snapshots but owns no live role graph referenced by them.
- Every **Strong Entity Reference** resolves before an authoring transaction commits; only a **Recoverable Dependency Reference** can produce a runtime resolution issue.
- Every **Generated Audio Stage** retains a deterministic origin reference for diagnostics, caching, and per-instance DSP state allocation.
- Preview, playback, audition, analysis, and export compile the same author semantics into generated operations; they may schedule differently but cannot reinterpret processor order, automation, transitions, latency, or nesting.
- Equal **Signal Closures** may share immutable plans, but mutable processors share an **Audio State Domain** only when all continuity and evaluation identities also match; fingerprints alone never authorize state sharing.
- An **Audio Program** exposes one or more stable **Sequence Output Ports** and never binds physical listening devices.
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
