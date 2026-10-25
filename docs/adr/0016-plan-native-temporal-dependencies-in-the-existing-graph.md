# Plan native temporal dependencies in the existing graph

Status: implementation in progress. The native pre-DSP boundary is explicit;
OpenFX temporal access remains unsupported until the complete consumer path passes.

The current host declares `CURRENT_FRAME`, rejects temporal descriptors and only
returns the current image. Effects already has time-expanded value programs and
`PreparedTemporalFrameSet`; Preview and Export resolve their source demands before
pixel execution. A Custom processor currently receives only one current input.
Changing a capability flag cannot connect these boundaries.

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
not grant this input. A consumer inside a multi-node Definition must additionally
verify its selected Custom node directly consumes that Definition entering value.

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

Input capabilities must be borrowed by the invocation. A strong capability retained
inside a factory would make a program/factory/program ownership cycle. The native
calibration uses weak test bootstrap only; that is not a production binding scheme.
The production resolver still needs the original immutable source/Clip admission.
The bank retains one configuration per occurrence: changed raster/project geometry,
format or recipe revision retires the obsolete idle worker. The native calibration
therefore checks exact configuration retirement, rather than assuming a variant
cache. Optional native retention must use the recipe's declared bytes, not a guessed
constant; a legal active render may intentionally run without optional retention.

Current Windows qualification: Effects 376 passed/0 ignored, Renderer 673
passed/39 ignored, Export 300 passed/18 ignored and App 3122 passed/212 ignored.
The App validation build includes OpenFX host 113 passed/67 ignored and Effect
adapter 11 passed/0 ignored. Five actual supervised resident gates passed with
the current ordinary worker; every gate settled with empty stderr. The unchanged
C++ HostSupport/Host retains its earlier 13-group qualification, including official
Basic and published Misc Invert 2.0; those groups were not rerun for this Rust slice.
The worker is test-profile opt0/debug0 with default features and no libtest harness;
The four libraries were built together: App validation opt0/debug0, Effects
opt1/debug0 without extra features, Renderer/Export validation opt1/debug0.
Export validation adds two tests to the earlier default298; these counts are not
a new default whole-workspace census.
This is affected-module regression, not a new whole-workspace or release qualification.
The native late-request gate repeats four full-RGBA renders on one native owner,
with eight readers coalescing two exact dependencies while Output/current leases
remain live. Its original owner executes the existing bounded time-expanded graph
with animated exposure and a real finite 1/48-second temporal blend. It selects
the entering value from a full original program whose waiting/downstream metadata
would fail at the requested time, rather than a caller-trimmed stack. Native output
matches the original graph values bit for bit. A separate mathematical DSP oracle
allows the documented Float32 pivot/luma rounding. Graph-budget, dependency,
cancellation and copy-refusal cases all settle without publishing output. Shared
Effects tests cover 12 failure/owner-stop combinations and fresh-owner recovery.

The additional native bank gate executes the actual Filter inside an Effects
processor invocation. Its Source resolver borrows that invocation's resources and
Control, consumes the full original program's input capability, and never creates
an execution Session. Four renders keep one native parent and eight exact Source
services. One upstream occurrence switches current/historical size and format;
the original bank correctly creates and retires twelve configurations, with one
current configuration retained alongside the parent. Exact creation/drop counts
would fail if upstream used a separate bank. Complete RGBA, original terminal
feedback/generation, no output cache, unique pixel transfer and strict native
shutdown pass. The grant derives from the native recipe's declared storage.
Five Effects regression groups also cover same-owner progress/feedback, inherited
retention ceilings, one-byte-short refusal, panic attribution, cancellation,
sticky deadline, pixel-chunk polling and no duplicate charge of prior feedback.

SourceDeliveryProbe deliberately does not declare temporal access: this is finite
transport/graph calibration, not conforming third-party temporal compatibility.
The normal product factory has no upstream resolver binding yet. The full Clip
retime/source admission, dependency-action scheduling, legal action/thread image
coverage, Preview/Export, real temporal plugins and cache invalidation remain open.
Temporal capability flags stay false. No RT, performance, complete heap, GPU or
other-platform qualification follows from these gates.

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

Next bind this compiler-selected entering-value capability and the internal service
to the production Custom processor's shared resource bank and exact Clip
retime/source admission.
Connect the dependency action to that same execution; verify all legal
action/thread image requests before enabling capabilities. Distinguish declared media absence from an
unprepared dependency; missing planned input cannot silently become a black frame.
Then verify Preview/Export with real temporal plugins. Qualification
must cover discontinuous ranges, negative/fractional time under the clip sampling
contract, changed parameters, upstream Effects, cancellation, exact image leases,
bounded storage, cache invalidation and repeated same-owner use. Keep the current
honest capability guards until those paths are connected and verified.
