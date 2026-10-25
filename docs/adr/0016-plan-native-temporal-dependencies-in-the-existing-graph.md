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
transport direction, not an implemented dependency RPC. The current bank already
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
complete Windows Effects library passed 363 tests with zero ignored. These are
in-process fixtures with explicit held-byte/peak accounting, not a native staged
resolver, full heap accounting or real temporal-plugin qualification.

The first native boundary produces an invocation-local output-domain value after
parameter changes, ClipPreferences and RoD admission. It owns no image or plugin.
The existing Render path consumes it for empty output, Identity, resource
authorization and normal processing. Invalid-input checks, action order, legal
current-image access, failure precedence and diagnostic phases remain unchanged.
This is preparation for dependency negotiation, not temporal execution support.

The initial boundary passed five supervised Windows native Release gates linked
to the actual product C ABI: SessionProbe Filter, GeneratorProbe, published Misc
Invert 2.0 (Natron 2.3.16-beta3), the existing 20 image-memory ownership cases and
image-domain conversion. Checks include complete RGBA, legal parameter-action
image access, same-session retry after memory refusal, empty output and actual
Generator action counts/lifecycle. These tests do not qualify a rebuilt desktop,
Rust IPC, Timeline, Preview/Export, temporal access, performance or other platforms.

The HostSupport dependency action now atomically replaces its complete input map
on OK/ReplyDefault; an error leaves the caller's map unchanged. It preserves
disjoint, fractional and explicit empty OK ranges, ignores ReplyDefault residuals
and refuses invalid time, malformed/nonfinite/reversed pairs. A product limit of
256 returned range pairs across inputs bounds negotiation metadata. Per-input
property writes are bounded before resize/copy; a refused write remains visible
even after reset. This does not bound the number of frames inside a range, SDK
heap/RSS, or raster payload: the shared planner must admit those separately.
Five protocol groups (27 queries) passed against an independently compiled owned
OFX fixture and the actual HostSupport/Host. Actual action logs confirm one
Create/Destroy per group, no Render, and no foreign call for invalid time or a
non-temporal descriptor. Six affected native regression gates also passed,
including published Misc Invert. These are dependency-action qualifications,
not evidence that a third-party temporal renderer or the full product works.

Next connect native dependency messages, shared Custom staged graph resolution
and leased historical images. Preserve upstream fault provenance without
quarantining the waiting parent; verify all legal action/thread image requests
before enabling capabilities. Then verify Preview/Export with real temporal plugins. Qualification
must cover discontinuous ranges, negative/fractional time under the clip sampling
contract, changed parameters, upstream Effects, cancellation, exact image leases,
bounded storage, cache invalidation and repeated same-owner use. Keep the current
honest capability guards until those paths are connected and verified.
