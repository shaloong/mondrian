# Plan native temporal dependencies in the existing graph

Status: native temporal Filter Source implemented with finite Windows
qualification. Current evidence and limits are recorded below. This decision
extends the existing graph and invocation ownership, not the project time model.

## Demand and execution authority

`EffectTemporalInputExtent` describes statically expanded IR demand.
`CURRENT_FRAME` does not grant late image access. A sealed processor factory
separately declares `requires_input_service`; that immutable declaration enters
complete/subtree identity and is checked for provider drift. Dynamic input uses
the original program, bank, working grant and `EffectProcessorControl`. Do not
invent an unbounded static window, another temporal interpreter or another
decoder/session/cache to satisfy it.

Historical input is the upstream graph at the requested author time, including
animated parameters, original Grade ordering and Clip masks. It is not a raw
media frame bypassing Effects. `PreparedEffectInput` carries a program-owned
proof: legacy stack slot or before/after Grade assignment plus Grade node. A
reused Grade, dense compiled ordinal or occurrence ID alone cannot select input.
`input_for_processor` also verifies that the concrete Custom node directly
consumes the Definition entering value. Synthetic mask/composition stages,
internal multi-node values and NoImage producers do not grant that capability.
Only ancestors of that input execute; the waiting Definition, unrelated branches,
downstream metadata builders and their DSP remain excluded.

Selection provenance is not GPU execution identity. GPU keys retain graph
semantics/contracts while excluding this selection metadata. Snapshot proofs
remain nontransferable even when equivalent GPU plans share. A factory must not
retain its own prepared program or source capability: that creates a strong
program/factory/program cycle. The normal factory borrows input per invocation.

## Time, sampling and image domain

OFX uses frames. Native double time is interpreted exactly with the original
rational author rate; an unrepresentable `TimelineTime` fails explicitly. The
original Clip/source mapper applies retime and the physical interpretation grid
once, including the half-open Source Sample Target boundary. Never substitute a
guessed grid or silently round a fractional native request.

Historical seeds use the originating Render Plan's exact Sequence evaluation
grid, including its doubled field grid. The retimed source grid is not seed
authority. Invert seconds-per-frame before frame-rate conversion; the existing
off-grid hash rule remains unchanged. Field-grid tests do not qualify native
interlaced image delivery or widen older full-temporal entry admission.

Extent, origin, PAR and sampling belong to the producer value, independently of
the Sequence/project canvas. Finite Media preparation first freezes the existing
time-expanded value program and complete mapped source wave, including signed
history/lookahead. Preview schedules that wave immediately. Only ROI/coverage
binding waits for actual producer extent/sampling; binding must not reevaluate
author state, replace time/seed or run DSP from an unbound closure. Do not use a
provisional canvas or serialize discovery behind the current frame. Proven
source-free programs keep an explicit owner output target.

Preview validates Ready samples; Export uses the decoder's exact aspect-fit
policy and resolved picture interpretation, then checks each decoded sample.
Unit Media retains the original CPU raster, including odd sizes. Varying source
domains refuse explicitly. Source picture mapping survives temporal pixel
replacement into the existing projected transform/cache identity. This introduces
no resampler. Title, nested and stack history retain their stated limitations.

## One Control and resource ledger

The scalar/adaptive engine exclusively borrows the existing processor bank,
retention limits and working grant. Session wrappers own generation changes and
complete output-cache publication; invocation input resolution does neither.
`with_input_service` scopes the actual direct-input stage and restores caller
bindings on success, error and unwind. Prepare/admit the complete simultaneous
source backing first, then restore the mutable service loan before upstream DSP
so nested processors can use it without aliasing. NoImage inherits no Source.

Deduct the waiting parent's active storage and all held native backing before
nested work; compose its peak in the same ledger. Already charged caller feedback
is the baseline: only newly added nested feedback consumes the remaining grant.
After nested owners are added, retention must reapply the original count/byte
ceilings. The bank retains one idle configuration per occurrence; raster/project,
format or recipe changes retire obsolete workers. Optional retention uses actual
recipe bytes and may be refused without invalidating a legal active render.

One original Control covers masks, copies, scalar kernels and Custom processors,
including heterogeneous CPU prefixes. Deadlines never renew; child deadlines may
only shorten them. Cancellation and stop reasons remain sticky. Preserve caller
observation, GUI/feedback occurrence and previously reserved receipts. Only new
successful prefix receipts enter completion; failure/unwind drops partial facts
without dropping caller facts or publishing completion. Static frontier grants
do not reserve arbitrary dynamic Source demand: admit it from the same remaining
grant. The legacy standalone entry remains unobserved.

## Native invocation and borrowed images

The same native instance, current image borrow, parameters and resource bank own
the complete call. UserEdited notifications may legally read current Source;
there is no universally pixel-free metadata preflight. Prepare that prerequisite
through the original graph, without dummy images or a second planning instance.

After parameter/clip preferences and before RoD/DSP, a temporal instance queries
[GetFramesNeeded](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectActions.html#c.kOfxImageEffectActionGetFramesNeeded).
HostSupport initializes each input to `[time,time]`, preserves discontinuous
ranges and explicit empty input sets, uses defaults on ReplyDefault despite
residual writes, and atomically rejects malformed/nonfinite/reversed/excessive
requests. Range metadata and temporal Clip/service admission are checked before
DSP. Actual pixels use the invocation-borrowed source resolver on demand; do not
enumerate continuous ranges into invented sample points or collapse range hulls.

Filter Source temporal access requires the effect and Clip declarations. Output
images remain current-time. Required sequential processing and temporal
Output/Generator processing are unsupported. Current Source Identity retains its
exact copy optimization. A legal temporal Source Identity uses normal Render and
the original input service, preserving plugin behavior without another executor.

Every instance action narrows the borrowed image permission. Recursive
RoD/preferences/FramesNeeded actions cannot inherit UserEdited image access;
suspended caller handles survive until release at the caller's own action
boundary. Atomic permission reads cover joined plugin workers within a serialized
invocation. Overlapping independent actions/render threads are not qualified.

IPC17 keeps dependency exchange inside that active native call. Foreign threads
send bounded POD requests through the original child pump. Only its original
mutable RenderOwner evaluates upstream graphs, observes stop and grants memory;
no decoder, parent FnMut or graph closure executes on a plugin thread. Match
owner/serial/service/time to the single outstanding request. Exact-time misses
coalesce in one invocation arena; new immutable mappings append without rebinding
leased addresses. Arena backing outlives actions and error teardown. FrameBorrow
retires all source/provider addresses; image identifiers change on rebinding even
when time and storage slots repeat.

At most 256 additional images and 128 MiB aggregate payload are admitted. Frame
views validate time, bounds, stride, disjoint ranges, file length, matching
alpha/PAR and finite pixels; publication validation polls original cancellation
in 64 KiB chunks. Files are read-only in the child; JSON contains neither pixels
nor process pointers. Known map/path/descriptor storage is charged. Coverage
alpha remains in [0,1], Float alpha may be finite signed/out-of-range, and Opaque
requires one; lossless native association lowering does not clamp pixels. Native
requests stay in the declared author range. Unique graph tiles transfer pixels;
shared tiles refuse an unadmitted clone. Pre-submit refusal releases file slots;
errors retain backing through retirement and never publish staged output.

## Preview, Export and cache ownership

`PreparedVisualProgram` borrows the original placement, Frame Execution Request
and source resolver. Media retime/interpretation, nested Sequence color semantics
and Solid input use the existing mapper. Historical Title/Adjustment and some
nested/stack/transition variants refuse explicitly. The Clip service's current
format is SceneLinear Float32 StraightCoverage; other formats require their own
admission, independently of the native association lowering above.

Frame Store admission attaches the complete originating media key to its decoded
allocation. Retarget only the source sample: file/stream fingerprint, proxy, RAW,
field, alpha and input color remain frozen. Never reopen current asset settings
to satisfy an older program. Both serial CPU domains use their existing result
channels, media scheduler and Frame Store, with one wait per domain and at most
256 distinct source keys per batch. Live processors wait under original Control
without DSP replay. Retirement/shutdown cancels domains before dropping replies
and partial leases, so decoder joins cannot wait on frames retained by this queue.

The original bounded EvaluationWorkingSet owns Sources and Executing phases.
Canonical program/placement/request and partial exact Ready leases survive
admission retries and producer wakes in its existing four physical slots;
speculative preparations evict first. Degraded media is queried again for quality
upgrade. Active grant replacement retires partial preparation; optional cache
trims do not replace that authority. Title pixels charge active CPU grants;
media leases remain charged by Frame Store. Resume only before DSP; unexpected
Pending after execution begins fails closed instead of replaying processors.

Source-ready temporal/nested CPU closures execute on the existing visual
Broker/worker and compositor bank; UI materialization must not block on late
media. Worker/results hold weak admission interest. Retirement cancels the exact
Broker binding without a payload cycle or parent-generation cancellation, blocks
publication and prunes failed observations. Failure consumption removes its
per-attempt failure key. Raw keys use the frozen program,
evaluation and raster, never reconstructed processed output. Only current work
can publish Viewer feedback. Original blocked, failure, stop and deadline
dispositions survive the handoff. Root per-layer GPU lowering remains intact.

Export uses its existing job Session and original execution request through CPU
and heterogeneous-prefix lowering. GPU continuation observes original job
cancellation; lowering cannot change time, intent, settings or observation owner.
Planned/late media share immutable decode-request lowering and read the existing
cache without storing extra history. Working conversion uses the decoded
allocation's already charged OnceLock and a temporary uncached color Session,
without reborrowing the active compositor's color Session or adding a bank/cache.
Preflight charges retained coverage plus conservative decode/alpha/color peak
(four Float32 RGBA buffers per source pixel); SDK/decoder heaps and RSS are outside
that logical reservation.

Lending dynamic input disables both lookup and publication of input-only output
caches. Installed OFX propagates Uncacheable through Preview, nested and persistent
consumers. Complete dependency identity is required before future cross-call
native caching. Missing media, unprepared input and RetryAdmission stay distinct;
typed upstream failure retires the waiting owner without quarantining its
definition or substituting the current image.

## Qualification and remaining work

Windows MSVC14.44 passes 16 native control groups, including the actual published
Natron-2.3.16-beta3 TimeBlur1.0 through the product C ABI. Its 3x2 Float32/PAR1
control checks complete independent RGBA literals, same owner, changed inputs,
native times2/3/2/2 and divisions4/8/4/1. This covers fractional history/future,
the plugin's multi-pass image-memory accumulator and temporal Identity. The real
parameter is `shutterOffset`, with capital O.

The published TimeBlur passes three finite NLE controls with real lossless FFV1
16x16/24fps media, public Timeline placement/curve edits, native feedback and
2/4/2 seeks. The uniform eight-frame control isolates upstream exposure; the
varying twelve-frame control changes Y/U/V per frame. Independent source-only
exports on a96fps reference grid express the exact quarter-frame author times;
the actual plugin chain stays24fps. Average those samples with independently
evaluated exposure=(requested frame)/4 stops. Full CPU Preview and Export
working images match exactly in both controls.

The varying fixture checks its actual stream time base and PTS. In millisecond
Matroska, frame4 starts at .167s, after4/24s:16/96 retains the preceding image,
while17/96 crosses that presentation boundary. Do not infer sample equality from
nominal CFR frame numbers or change Source Sample Target/decoder policy to fit
a test oracle. This is not encoded Export or display/GPU qualification.

The public-state control installs the real Filter catalog through the configured
ordinary Product, persists its selection in a temporary preferences file, edits
shutter through the public parameter transaction, and verifies Undo/Redo pixels.
It saves/closes the Project, joins the old Runtime, drops AppState, and reopens
the archive in a new AppState. The Project-owned AssetLibrary comes from that
archive, with no test-library replacement. Startup restore publishes a fresh
definition; new native bank owners reproduce the exact authored Clip and
temporal Preview/Export pixels. This is a new AppState/Runtime/native child in
the same parent test process, not a desktop GUI process-restart qualification.
Only TimeBlur is functionally qualified by the bundle installation control.

A Ready frame evaluation owns the immutable plan; ordinary current-frame Custom
DSP still belongs to the existing CPU composition task. Its original source
client resolves late media. The synchronous no-input composition helper cannot
qualify that path. Consume feedback from `PreviewWorkPoll`, which transfers it
out of the internal pending slot. Preserve these boundaries in future controls.

The last full App regression at4e51ef86 passes3145/214 ignored. Current uniform,
varying and public-state TimeBlur controls each run in an isolated process and pass; the
unchanged ordinary regression is retained rather than rerun for an ignored-test
change. Original1800s ordinary/180s native budgets,
exit0, empty stderr and settled descendants are retained. This batch changes
tests/documentation only. Unchanged Effects390, Renderer689/39 ignored,
Export307/20 ignored, 16 native groups and seven resident/action controls retain
their prior qualification. The last full combined ordinary baseline remains4531 passed,
with273 conditional tests ignored then; new native-only controls are explicitly invoked separately from that baseline. App uses validation opt0/debug0;
Effects/Renderer/Export use opt1/debug0. The ordinary development product remains
opt0/debug0/features[] with unchanged runtime source; it was not rebuilt for a
test/documentation-only change.

Earlier finite real-FFmpeg Preview/Export source and formal PNG/GPU-prefix
qualification retain their original conditions; they are not new TimeBlur
product-flow or GPU-interop evidence. Native General/Transition/Paint/Retimer,
historical input variants above, variable domains, other formats/fielding,
tiles/upstream RoI/non-unit scale, overlapping action/thread requests, more real
Temporal plugins, broader TimeBlur placements/retiming, loop/cancellation/encoded
export, GUI, comparative performance
and other platforms remain open. F06/GL remain open; this does not establish
universal100ms shutdown, whole-heap safety or realtime behavior.
