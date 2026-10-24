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

## Explicit machine-local Catalog context and exact version identity

Qualified finite Windows x64 CPU slice, 2026-10-24. The Graphics menu selects
Filter or Generator explicitly; failure never triggers another context. Existing
Filter keys stay unchanged and Generator keys stay disjoint. Machine-local
preference v4 records path plus context, reads legacy v3 Filter paths, preserves
unrelated preferences and refuses unknown contexts/fields. Projects, author time
and parameter authority stay unchanged. The original worker, result channel,
notice budget and live App-owner publication remain; cancellation cannot publish.

The real Misc inventory has123 headers/119 identifiers. LensDistortion exports
major2/3/4 and ShufflePlugin major2/3/1. Previously discovery sorted complete
identities but parent/native lookup selected the first matching identifier; the
sorted parent could describe major1 while executing major2. Identifier uniqueness
in the first batching candidate also wrongly rejected this legal inventory.

Carry the existing complete descriptor through Catalog schema2, Describe schema5
(`describe-v2`), immutable factory and resident wire15 (`session-v13`). Explicit
selection retains all legal versions; identifier-only entry points refuse
ambiguity. Native selection matches identifier/major/minor before Load. The SDK's
second export query validates API, complete identity and entry pointers before
SetHost. Constructor failure balances its module reference; foreign exceptions
become host-owned errors while their defining module remains alive. Notices are
version-bound. Effect keys retain their existing formula; the OFX execution tag
advances to reject cached results from the former selection semantics. No Project
migration or automatic repair of formerly wrong-version output is inferred.

The original123-class install/restore exceeded its shared120s gate. Cold native
processes cost about1.24s per class in that diagnostic workload. Reuse only the
metadata process for one bundle/context, never the native Host: every class still
runs a complete Host/cache/module lifetime. One bounded atomic packet waits for
acknowledgement; exact schema/context/index/descriptor validation alone advances
the original30s class deadline. Binary checks remain before/after each class.
Faults retire and reap the current process before later classes resume in a fresh
one, without replaying completed actions. Failed final settlement or cancellation
discards unpublished definitions. Render and resource-bank ownership are unchanged.

Ordinary Product cf3325ab (dev opt1/debug2/default features) and App test parent
5ab5cdac (opt0/debug0/default features; test-only locator change) pass9 exact native
gates: four out-of-order versions2.1/3.0/1.0/2.0 with Catalog metadata and actual
Graph RGBA at0/6/3/6; changed export refused before a trapping SetHost; foreign
exception destruction plus fresh later worker; healthy reuse, middle-class crash,
cancel/discard/reap, explicit context refusal and existing-bank refusal/reuse.
The ninth gate is the real Misc Catalog install/preference restore/public Timeline
roundtrip, passing42.62s. Its preceding test-label failure (ConstantOFX versus
Constant) was corrected using independently pinned persistent class identity,
without direct registration, reduced assertions or increased deadlines.

All9 gates have exit0, empty stderr and clean child settlement under original
180s/2s supervision; shared120s install/restore and resident100ms remain unchanged.
The affected ordinary OpenFX group passes141, with79 ignored. Ordinary preference,
menu, localization, codec and picker controls retain their separately stated
qualification. Original failures remain failures, superseded by these scoped runs.
No reference-host performance, statistical stability, whole-heap, realtime,
physical GUI, encoded export or other-platform qualification follows. Dedicated
Generator Clip, other contexts, RoI/temporal/tiles/GPU and broader plugin coverage
remain separate. GL/F06 stay open; protected integration1109d99f remains held.
