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

## Explicit machine-local Catalog context (candidate)

The Graphics menu selects Filter or Generator effects explicitly. Catalog work
prepares only that context; a Filter failure never triggers a Generator retry.
Existing Filter keys stay unchanged and Generator keys remain disjoint. The
Generator display suffix distinguishes two contexts of one class in the library.
The original worker, bounded result channel, notice budget and live App-owner
publication are retained; canceled/unpublished work cannot enter preferences.

One machine-local selection records path plus context. Preference v4 reads v3
Filter paths, preserves unrelated preferences and writes canonical typed entries.
Deduplication includes context; unknown contexts/fields refuse rather than default.
Projects, author time, parameters and native wire14 remain unchanged. Ordinary
controls and native install/restore/Timeline qualification are pending.

Catalog cold-process reuse is a candidate optimization. The real 123-class Misc
install/restore exceeded its original shared120s gate. Current ordinary Product
still costs about1.24s per cold class; the measured parent boundary spends1201ms
waiting, versus23ms spawning, 3/4ms staging/cleanup and1ms reading. Actual native
archive calls take about80ms; an instrumented Product call takes109ms. These are
diagnostic conditions, not reference-host or realtime performance qualification.

Reuse only the metadata process for one bundle/context, never the native Host.
Every class still runs the original complete Host/cache/module lifetime. One
atomically published, size-bounded class packet waits for acknowledgement; only
its exact schema/context/index/identifier can advance the original30s class
deadline. Keep binary verification before and after each class. Native faults,
fatal or incomplete observations retire the process; later classes resume in a
fresh process without replaying completed actions. A crash/hang rejects the
in-flight class through the original child settlement guard. A failed final
process settlement refuses the preparation. Definitions remain unregistered
until the original live App owner publishes the complete result. No Render,
resource-bank or author-state contract changes. Candidate e3989a35 passes138
ordinary checks (78 remain ignored), four overlapping protocol controls and four
clean native gates: three-class reuse with exact independent lifetimes, middle
process crash followed by a fresh process for the remaining class, cancellation
discarding the prefix and reaping the blocked class, and explicit context refusal.
The original real-bundle gate still fails; this is not Catalog qualification.

### Exact plugin-version identity: required contract correction

The real Misc inventory contains123 headers/119 identifiers: LensDistortion has
major2/3/4 and ShufflePlugin has major2/3/1 in export order. Discovery correctly
retains `(identifier, major, minor)` and sorts it; the legacy native selection
and parent description lookup choose the first matching identifier. For Shuffle,
the sorted parent metadata can therefore name major1 while native export lookup
chooses major2. The new identifier-only manifest rejects this legal inventory.
Do not remove that control, skip these classes or silently select one version.

Carry the existing descriptor's full identity through Catalog packets, native
Describe, the immutable processor factory and resident Prepare/SDK selection.
Validate the exact tuple before native actions and bind completion to it. Preserve
unique-class Effect keys and the existing versioned key formula; retain all legal
versions rather than creating another identity authority. Legacy identifier-only
entry points may operate only when unambiguous; explicit selection must remain
available for versioned classes. The native selector must never choose by order.

This is an OpenFX adapter contract change, not a Timeline/Project migration.
Describe schema4 and resident wire14 need explicit versioned replacements;
stale helpers must refuse, never reinterpret a missing version. No project file,
parameter authority, Audio DAG or Render Graph rewrite is required. Validate an
owned out-of-order same-identifier/multiple-major fixture through metadata and
actual pixels, then rerun the original real Misc install/restore/Timeline gate,
the new crash/cancel controls and affected single-version native regressions.
The implementation candidate now uses the existing full descriptor in Catalog
schema2, Describe schema5 (`describe-v2`) and resident wire15 (`session-v13`).
The native selector matches identifier/major/minor before Load, rejects missing
or duplicate exact headers, and returns that selected plugin directly. Factories
retain the same descriptor; identifier-only APIs refuse ambiguity and explicit
descriptor APIs cover all versions. Notices are version-bound too. Effect keys
keep their existing formula; only the OFX execution-contract identity advances
to prevent old selection semantics from retaining stale compiled/cache results.
Project fields are preserved; fidelity of projects authored under the former
wrong-version behavior is not silently inferred or repaired.

Owned export order2/3/1 now has a Catalog→Graph pixel regression: the uncorrected
candidate fails1.54s at manifest admission, with clean process settlement. The
diagnostic-only timing tests were removed after measurements were compacted.
The corrected implementation, stale-protocol controls, actual per-version pixels
and all real-bundle acceptance remain pending qualification.
