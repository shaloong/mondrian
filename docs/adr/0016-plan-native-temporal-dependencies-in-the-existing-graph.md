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
second instance to query dependencies. A later same-owner plan must pin exact
author time, parameters, geometry/sampling and dependency identity through its
consumption or cancellation. Shared pinning/admission interfaces remain to be
implemented and reviewed against the actual bank; this document does not claim
that existing optional residency already provides them.

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

Next implement native dependency negotiation and the shared Custom planning/value
connection, then verify Preview/Export with real temporal plugins. Qualification
must cover discontinuous ranges, negative/fractional time under the clip sampling
contract, changed parameters, upstream Effects, cancellation, exact image leases,
bounded storage, cache invalidation and repeated same-owner use. Keep the current
honest capability guards until those paths are connected and verified.
