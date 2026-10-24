# Render native OpenFX domains before viewport publication

Status: finite Windows x64 CPU slice qualified; broader spatial scheduling remains partial.

The existing Effects CPU value is a fixed, zero-origin viewport. An OFX effect's
canonical RoD, its enclosing pixel RoD and that viewport are different facts.
The previous host never queried effect RoD and reported the viewport as every
Output image's support. That cannot represent finite or translated generators.

Keep the existing graph and frame ownership. Query current-time RoD after
parameter changes and ClipPreferences. Initialize the action's output with the
SDK context default; ReplyDefault uses that default regardless of residual writes.
Convert canonical coordinates with the actual image PAR and unit render scale,
enclosing fractional boundaries. Output clip RoD remains canonical; Output image
bounds and RoD are the complete pixel domain. Input remains the supplied upstream
viewport. This does not add upstream spatial or temporal scheduling.

Tiles remain unsupported: supply and render the complete finite domain, then
place its intersection in the requested viewport, with transparent black outside.
An Opaque native image becomes StraightFloat when placement introduces transparent
pixels; its covered alpha is canonicalized to one. Other representations and RGB
values are preserved. Empty support produces transparent output without starting
a sequence requiring a nonexistent Render action.

The matching full-viewport path uses existing storage. Other domains use one
action-local allocation, bounded by 8192 per axis and 128 MiB payload. Retain no
scratch/cache across frames. Reject malformed, unbounded, oversized or unsupported
non-multiresolution domains before Render. Native wire 14 distinguishes the
current callback/report contract from older workers. Allocator administration and
plugin-owned memory remain outside logical payload accounting.

This is viewport publication, not a variable-domain Effects value or full RoI
scheduler. Pixels outside the viewport are not propagated downstream. Full-frame
native work can exceed requested viewport work and needs later measurement. GPU,
temporal fetch, tiled execution and multiresolution render scales remain separate.

## Authorize actual transient storage through the existing bank

The initial fixed 128 MiB reservation broke the original Invert Graph at its
unchanged 128 MiB working budget, even though that viewport requires no scratch.
The reservation was removed rather than increasing the test budget.

`EffectProcessorResources` is an invocation's borrowed view of the existing bank.
The provider requests its complete simultaneous transient requirement before
allocation. After static active storage and feedback are charged, the bank evicts
optional sibling residents when that can make the request fit and charges the
granted peak. Intrinsically over-budget requests preserve valid residents. The
borrow ends on every outcome and panic. No second budget owner, executor, author
state or retained scratch is added.

One bounded callback request uses the existing worker pump. The original parent
execution thread authorizes it through the borrowed bank; owner, invocation and
byte count must match exactly. Cancellation wakes the callback and the original
deadline continues running. Allocation and BeginSequenceRender/Render require
authorization. Post-preflight refusal is a distinct disposition: parameter/RoD
metadata actions may have run, while DSP and output publication have not. It
does not claim HostResourceRefused's no-native-action guarantee. Actual feedback
returns to the same bank, which decides retention; valid plugins are not
quarantined as processing faults merely because resource admission failed.

## Qualification

Ordinary Product465b552e (dev opt1/debug2/default features) passes twelve actual
native gates, including the original unchanged128MiB Invert Graph, finite-domain
Direct/Graph, published Constant Size and CheckerBoard, prior Generator/contextual/
Invert paths and two public author/history/save/reopen flows. Exact319/320-byte
authorization proves refusal before DSP, unchanged mapped output and same-owner
reuse. Final Graph test parent e53b7ae1 proves retained feedback/owner, no definition
quarantine, no Begin/Render/End on denial and a successful subsequent frame.
All have exit0, empty stderr and clean original180s execution/2s cleanup supervision.

Effects360, App host99/provider10, ordinary Renderer672 and current native image-
ownership20 checks pass. Host62/Renderer39 ignored tests remain unverified. Native
conversion checks include the exact128MiB limit and failure-preserving errors.
These finite Windows CPU results do not qualify whole-heap accounting, realtime,
comparative performance, other platforms or the spatial/temporal/GPU/Timeline
capabilities excluded above. GL and intermittent F06 exit failures remain open;
the protected integration branch is not advanced by this slice.

## Existing Timeline Effect placements (finite qualification)

A Generator used as an Effect is not a new Clip-content kind. Its output target
is the existing execution canvas: source raster before Clip Transform, generated
Solid/Title raster, nested child output, or Adjustment accumulator. The compiled
graph still owns exact author time, project geometry and dependency reachability.
Reachable external Source branches remain required for partial blending/mattes.
No new media identity, parameter authority or source scheduler is introduced.

For image-processing entry points, a zero-input node selects its staging alpha
from its retained output declaration, independently of external image alpha.
Selection is stable across unrelated incoming-alpha changes; actual provider
output is still negotiated and validated. The explicit source-free `generate`
entry keeps the caller's exact format requirement and refuses unsupported
requests before provider work. No existing input pixels are reinterpreted.
The CustomSource semantic tag advances so old source-containing cache identities
cannot reuse results from the former staging rule; ordinary Filter tags remain.

Timeline uses the same Effects session, feedback route, memory admission, color
conversion and downstream alpha/Transform/blend/Transition operations. The blanket
CustomSource refusal can therefore be removed without bypassing other admission.
This does not add a dedicated Generator Clip or Catalog insertion, upstream RoI,
temporal/GPU support, or permission to skip real external dependencies. Ordinary
canvas materialization is initially retained; no allocation/performance claim.
Core361, Renderer673 (39 remain ignored) and App Preview38 ordinary checks pass.
Renderer covers root/nested admission, Solid/Media/Adjustment/Transition and
reachable Source blending; its qualified code precedes only the source cache-tag
update, which current Core and native App checks include. Current ordinary
Product/App df20a931 passes the real Constant1.0 root Solid flow: public add/color,
same Preview owner, Undo/Redo, working-frame/semantic-trace parity with actual
Export, SaveAs/Close/fresh AppState Open and a fresh native owner. Five phases each
render0/6/3/6; associated RGBA, actual PremultipliedFloat and sampling are exact,
separately from the StraightFloat preparation request. Original180s/2s supervision
passes with exit0/empty stderr/clean process trees. This is Full16x16/24fps Windows
CPU working-frame qualification, not encoded export, physical GUI/device,
animation/Loop/resize/nested-native, other platforms, RT or performance evidence.
GL/F06 remain open and the protected integration branch remains held.
