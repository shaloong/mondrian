# Plan native temporal dependencies in the existing graph

Status: implementation in progress. The native pre-DSP boundary is explicit;
OpenFX temporal access remains unsupported until the complete consumer path passes.

The product still declares `CURRENT_FRAME` and rejects temporal descriptors.
Effects has time-expanded value programs and `PreparedTemporalFrameSet`; the
normal OFX factory can now consume an invocation-borrowed input service. Main
Preview/Export source scheduling is not connected to that service yet. Changing
a capability flag cannot connect these boundaries.

OFX [GetFramesNeeded](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectActions.html#c.kOfxImageEffectActionGetFramesNeeded)
returns per-input pairs of frame-range endpoints, including discontinuous ranges.
The host initializes every input to the current frame. ReplyDefault uses that
default, rather than residual writes. Malformed/nonfinite/reversed or excessive
requests must fail before DSP, without replacing them with a range hull or
silently substituting the current frame.

The dependency path will use the existing immutable author/program snapshot,
exact Clip mapping, graph evaluation and source scheduler. Historical input means
the upstream graph evaluated at that time, including its animated parameters;
it does not mean a raw media frame bypassing upstream Effects. Each dependency
contributes to the same cancellation, memory and cache identity. No second
decoder, scheduler, parameter store or temporal cache is introduced.

OFX time is measured in frames, not seconds. Dependency conversion must use the
same owner frame context and exact Clip retime map as ordinary execution. Native
fractional requests follow the declared clip sampling contract; do not silently
round them or create a second interpretation of Sequence time.

The native pre-DSP prefix must retain the original instance, frame borrow,
parameter callbacks and resource bank. `UserEdited` change actions may legally
read the current Source image. Thus a universally pixel-free metadata preflight
is not the contract: prepare that prerequisite through the original graph before
expanding native historical/future demand. Do not supply dummy pixels or create a
second instance to query dependencies. Exact author time, parameters,
geometry/sampling and dependency identity must survive through consumption or
cancellation.

Keep the staged dependency exchange inside the original active processor call.
The existing child pump can carry dependency messages while the native call keeps
its FrameBorrow and parameter callback borrows alive; do not introduce a
cross-call plan handle or a second instance registry first. This is the chosen
transport direction; the internal IPC17 service below now implements the exchange. The current bank already
holds the active instance outside evictable optional entries. Upstream work must
exclude the waiting parent's active peak, held rasters and transient grant from
its remaining budget, then compose the nested peak in the same ledger. Each
dependency wave must use the original immutable graph/source admission; an SDK
callback cannot open a new decoder. Missing prerequisites stop the invocation,
without replaying partially executed DSP or substituting the current frame.

Same-bank staged probes exposed and fixed two scope defects: nested work cleared
the parent's observation binding, and final retention did not reapply the outer
count/byte ceilings after nested owners were added. The original bank now restores
the previous binding on normal/error/unwind paths and reapplies ceilings at the
actual retention boundary. Two regression tests cover success/reuse, refusal,
panic, cancellation, five optional grants and actual progress receipts; the
complete Windows Effects library passed 376 tests with zero ignored. These are
in-process fixtures with explicit held-byte/peak accounting, not a native staged
resolver, full heap accounting or real temporal-plugin qualification.

The first native boundary produces an invocation-local output-domain value after
parameter changes, ClipPreferences and RoD admission. It owns no image or plugin.
The existing Render path consumes it for empty output, Identity, resource
authorization and normal processing. Invalid-input checks, action order, legal
current-image access, failure precedence and diagnostic phases remain unchanged.
This is preparation for dependency negotiation, not temporal execution support.

The native image adapter now reads immutable, caller-owned Source frame views.
Views carry exact native time, pixel origin/extent, Float32 RGBA and the same
clip alpha/PAR contract. Binding validates the complete set before replacing it,
with at most 256 additional views and the existing 128 MiB per-image limit.
Backing belongs to the caller's original ledger; no raster copy/cache is added.
Bind only between foreign actions with no image handles leased. Each wrapper
owns its property handle, not the pixels. FrameBorrow retires source image addresses;
rebinding changes historical image identities even at the same time/array slot.
Validation polls the original abort control in 64 KiB chunks and before commit;
callers preserve the original stop reason before classifying a refused bind.
Unknown times do not borrow the current image or fabricate its RoD. The RoD
suite now distinguishes query failure from allocation failure on a valid clip,
rather than misreporting BadHandle, and clears rejected/nonfinite bounds.

Already-resolved sources and late Source requests now travel through resident
IPC17 inside the same native call. Source resolution is explicitly admitted per
invocation. Foreign callbacks send bounded POD identities through the original
pump; only the original mutable RenderOwner may evaluate an upstream graph,
observe cancellation and grant memory. There is no decoder or graph closure on
plugin threads. Owner/serial/service/time replies must match the one outstanding
request. Concurrent readers coalesce exact-time misses in one invocation-owned
arena. Existing mappings remain immutable while images are leased; new mappings
append without rebinding their addresses or identities. The arena and backing
outlive native actions and error teardown, with no cross-call temporal cache.

At most 256 additional images and 128 MiB aggregate payload are accepted. Files
are read-only in the child, never pixels in JSON or process addresses. Exact time,
geometry, alpha/PAR, representation, disjoint byte ranges, actual file length and
finite samples are checked before publication. Known descriptor/map/path storage
is charged when resolution is enabled. Each grant composes held inputs, the native
output domain and the returned graph frame while copying it. The upstream owner
must also deduct its waiting parent's already-admitted active backing before
using its remaining original grant. This is logical controlled storage, not SDK
heap/RSS accounting. Pre-submit refusal releases reserved file slots; invocation
errors retain backing through retirement and never publish staged output.

A typed dependency failure keeps its upstream provenance, retires the waiting
native owner without replay, and does not quarantine that parent's definition.
Declared media absence and unprepared input remain distinct. An upstream graph
inherits the same absolute invocation deadline; derived cancellation scopes can
shorten it but never renew it. Explicit cancellation still reaches the entire
execution generation. Consuming a uniquely owned graph tile transfers its pixels;
shared tiles refuse transfer rather than silently copying unadmitted backing.

`PreparedEffectInput` selects one Definition entering value from the original
immutable program. Compiler provenance includes the legacy stack slot, or the
before/after Grade assignment and Grade node; a reused Grade or a dense compiled
stage index is insufficient. A program-owned proof rejects bindings from another
snapshot. Synthetic composition/mask stages and emitted zero-input producers do
not grant this input. The concrete node selector
`PreparedEffectProgram::input_for_processor` verifies the actual Custom node
directly consumes that entering value; an internal multi-node Definition value
is not silently substituted.

At the requested exact author time, only that input's ancestors are evaluated.
The waiting Definition, unrelated Grade branches and downstream metadata builders
are excluded, as well as their DSP. Grade ordering and Clip mask placement stay
with the existing compiler. Ordinary and temporal upstream values use the same
bounded frame executor; the older temporal-only entry points retain their guard.
The capability owns no execution session, processor bank, cache or decoder.

Position provenance is not GPU lowering identity. GPU plan keys retain graph
semantics and stage execution contracts while stripping this selection metadata;
equal plans still share, changed parameter values remain isolated, and snapshot
capabilities remain nontransferable. One proof allocation is shared per prepared
program. Controlled metadata estimates include binding storage and the Grade's
zero evaluation; these remain conservative logical charges, not allocator/RSS.

The scalar/adaptive frame engine now borrows the existing processor bank,
working grant and retention limits instead of requiring the entire Session.
That exclusive borrow is necessary inside an active native processor: constructing
another Session to satisfy it would create a second instance/budget authority.
Session wrappers keep generation changes and complete output-cache publication;
invocation input resolution performs neither cache access nor generation rebinding.
The original Control crosses nested processor calls and bounded pixel checkpoints,
so progress, GUI/feedback provenance and sticky stop reasons remain with the owner.
The legacy standalone entry remains unobserved; borrowing an owner Control does
not implicitly enable another product observation route.
Parent active storage and still-held native backing are deducted before upstream
work; its peak is then composed in the same ledger. Already charged parent feedback
is the nested baseline, so only new nested feedback reduces the remaining grant.

Input capabilities are borrowed by the invocation. A strong capability retained
inside a factory would make a program/factory/program ownership cycle. Weak
bootstrap remains only in legacy calibration fixtures; the normal factory gate
borrows the original program directly, with no bootstrap or factory capture.
`PreparedVisualProgram::prepare_clip_processor_input` now prepares that input
from the original Clip/Transition-endpoint snapshot. Complete-frame temporal
work and processor input use one source mapper: media retime plus interpretation
grid, nested Sequence color semantics, or solid color. It emits the existing
demand batch for the original Preview/Export scheduler and bank; it owns no
Session, decoder or cache. Basic Title and Adjustment history remain refused.
Input extent, PAR and sampling belong to the actual source value, not the
Sequence/project canvas. The invocation must supply and preserve that contract.

`EffectProcessorControl::with_input_service` borrows source preparation for one
execution. The executor scopes the actual direct-input stage and restores it on
success, error and unwind; NoImage processors receive no inherited input. Source
preparation first admits the complete simultaneous source backing and returns
frozen frames only. The mutable service loan is restored before upstream DSP,
so nested processors may use the same service without aliasing it. Resources
execute that input through the original bank, working grant and Control. Input
failure retires the waiting owner without blaming its definition. Subrequest
deadlines can only shorten the original deadline; stop reasons remain sticky.

`PreparedVisualProgram::clip_processor_inputs` borrows the original placement,
Frame Execution Request and source resolver. It shares the existing position
validator, including the doubled field grid, and checks the placement instant.
The original attempt's grid supplies historical seeds. This does not change the
older full temporal path's field-grid policy. The Clip service currently admits
SceneLinear Float32 StraightCoverage only; other formats fail explicitly.

The normal OFX processor owns one RenderOwner for observation, memory and Source
requests. Native double frame time is interpreted exactly using the original
rational author rate; values not representable by `TimelineTime` fail rather than
rounding onto a guessed grid. Returned source origin/extent can differ from the
current image. Unique tile pixels transfer without an unadmitted Rust clone;
the existing charged native file transfer still applies.

Actual Coverage input exposed an association mismatch: current images admitted
Coverage as native Float, but late Source mapping rejected its Rust label. Both
now use one lossless straight/premultiplied/opaque association lowering. The
original numeric alpha domain still validates mapped pixels through
`ColorFrameAlpha::admits_alpha`: Coverage stays in [0,1], Float retains finite
signed/out-of-range alpha, and Opaque requires one. No pixel conversion or clamp
is introduced. Native requests must also stay inside the declared author range;
negative-time calibration supplies an explicit range containing those times.

Binding a dynamic input service disables scalar input-only output caching.
Outer consumers must likewise forbid reuse without complete dependency identity;
the installed OFX definition already propagates Uncacheable to Preview/nesting.

Historical frame seeds use the Sequence grid: seconds-per-frame must be
inverted before frame-rate conversion. The former reciprocal error produced
hashed seeds at on-grid historical instants. The existing off-grid hash rule
is unchanged. A regression executes the original temporal path with current
frame72 and historical frame69 and verifies their exact mixed pixel.
The bank retains one configuration per occurrence: changed raster/project geometry,
format or recipe revision retires the obsolete idle worker. The native calibration
therefore checks exact configuration retirement, rather than assuming a variant
cache. Optional native retention must use the recipe's declared bytes, not a guessed
constant; a legal active render may intentionally run without optional retention.

Current Windows affected-module qualification: Effects 380 passed/0 ignored,
Renderer 680 passed/39 ignored, Export 300 passed/18 ignored and App 3123
passed/213 ignored. All complete gates settled with empty stderr under their
original budgets. The four libraries were built together: App validation
opt0/debug0, Effects opt1/debug0 without extra features, Renderer/Export
validation opt1/debug0. Builds and full gates ran outside the Windows sandbox;
earlier sandbox rmeta/publication failures are not source or assertion workarounds.

The current-source default worker was rebuilt at test-profile opt0/debug0,
without libtest or validation features. Its 12 existing diagnostics/test-helper
warnings remain; this is not a zero-warning or release qualification.
Six actual native gates passed with that worker. The normal installed factory
gate performs four full-RGBA renders on one native owner, eight coalesced exact
Source services with current/Output leases held, and twelve upstream
configuration creations/retirements in the original bank. Source origin/extent
vary; terminal feedback, generation, unique pixel transfer and Float alpha6
remain intact. It borrows the original program through Control, with no weak
bootstrap. The other gates retain graph/dependency/budget/cancellation/refusal
and native lifecycle coverage. This does not prove universal100ms shutdown.

Three new Effects groups cover nested use, service/stage/deadline restoration,
changed historical content without stale output reuse, panic/budget/identity
refusal, recovery and sticky cancellation. Two Renderer groups cover the
original attempt/field grid, source admission and foreign/unsupported input
refusal. Native time conversion has its own exact-clock test. The source-map
regression exercises five associations and 35 numeric-alpha conditions,
including unclamped finite Float values and rejection of invalid Coverage.

SourceDeliveryProbe deliberately does not declare temporal access. These are
owned finite integration calibrations, not conforming third-party temporal
compatibility. Main Preview/Export source admission, mixed CPU/GPU service
entry, native format/alpha beyond the bounded coverage executor, dependency
scheduling, legal action/thread image coverage and complete field-grid parity
remain open. Temporal flags stay false. No new whole-workspace, release, RT,
performance, complete heap, GPU or other-platform qualification follows.

The HostSupport dependency action now atomically replaces its complete input map
on OK/ReplyDefault; an error leaves the caller's map unchanged. It preserves
disjoint, fractional and explicit empty OK ranges, ignores ReplyDefault residuals
and refuses invalid time, malformed/nonfinite/reversed pairs. A product limit of
256 returned range pairs across inputs bounds negotiation metadata. Per-input
property writes are bounded before resize/copy; a refused write remains visible
even after reset. This does not bound the number of frames inside a range, SDK
heap/RSS, or raster payload: the shared planner must admit those separately.
The five protocol groups (27 queries) use an independently compiled owned OFX
fixture and actual HostSupport/Host. Action logs confirm one Create/Destroy per
group, no Render, and no dispatch for invalid time/non-temporal declarations.
These are action qualifications, not third-party temporal-renderer evidence.

Next bind the borrowed Clip service at the actual Preview/Export consumer,
resolving its existing demand batches through the original scheduler/decoder
and shared bank. The mixed backend currently constructs a local Control without
the service; its node scope hook alone is not an integrated consumer. Do not
retain a capability in the factory or reintroduce another Session.
Connect the dependency action to that same execution; verify all legal
action/thread image requests before enabling capabilities. Distinguish declared media absence from an
unprepared dependency; missing planned input cannot silently become a black frame.
Then verify Preview/Export with real temporal plugins. Qualification
must cover discontinuous ranges, negative/fractional time under the clip sampling
contract, changed parameters, upstream Effects, cancellation, exact image leases,
bounded storage, cache invalidation and repeated same-owner use. Keep the current
honest capability guards until those paths are connected and verified.
