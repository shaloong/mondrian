# Native plugin compatibility and qualification

## Objective and standards baseline

The product objective is complete, negotiated VST3, CLAP and OpenFX hosting,
with canonical authoring transactions, correct preview/export output, usable
native and product controls, and measured execution cost. Loading an ABI and
passing owned fixtures do not qualify every conforming plugin. Optional standard
features must have an explicit implemented or unavailable negotiation result.

Checked on 2026-10-08:

- [VST SDK 3.8.1](https://steinbergmedia.github.io/vst3_dev_portal/pages/Versions/Version%2B3.8.1.html),
  published 2026-08-11. The new transport-control, MIDI 2 articulation and exchange
  interfaces require explicit implementation and qualification.
- [CLAP 1.2.10](https://github.com/free-audio/clap/tree/1.2.10), also reviewed on 2026-10-09.
  Stable extensions and draft extensions are separate obligations.
  The pinned 1.x binding's ABI compatibility does not establish feature parity.
- [OpenFX 1.5.1 release](https://github.com/AcademySoftwareFoundation/openfx/releases/tag/OFX_Release_1.5.1).
  Main-branch clarifications and extensions must be identified separately from
  the published release. The current product advertises host API 1.4; selected
  newer properties do not establish complete 1.5.1 hosting.

The detailed [audio standard matrix](native-audio-standard-features.md) and
[OpenFX standard matrix](native-openfx-standard-features.md) identify actual
product consumers and pending closure for each feature family.

Current desktop audio checkpoint (2026-10-29): the factory/module ownership
slice passed complete SDK library and App product CI on Windows, Linux and
macOS before integration ([PR31](https://github.com/shaloong/mondrian/pull/31)).
Factory contexts survive concurrent discovery/instances; editor retirement does
not retire factory callbacks. The following Product slice uses the existing
Main/DSP schedule and one bounded initial parameter handoff on every desktop.
Its Linux Audio library passes 851 tests (65 ignored).

Unmodified MIT-licensed SDK3.8.0 ADelay passes the
[public Runtime/Graph case](../../crates/mondrian-audio/tests/native_vst3_portable_runtime.rs)
on Ubuntu24.04.5, Rust1.97.1 and private patched FFmpeg8.1.2: explicit48k/Stereo,
offline/max64, delays0 and1/64, three entries/seeks per delay and64/17/47
partitions. All3411 frames match the independent ring-buffer oracle; native
opaque state, project serialization and original child settlement are checked.
The original Ring Out CLAP Runtime/Graph case also passes after sharing its
unchanged input/census helper. Both runs have exit0, empty stderr and settled
process groups. Native GUI, product timer/FD concurrency, broader instruments,
physical devices, realtime/performance, full heap/leaks and macOS native-plugin
qualification remain open. The macOS actual-main CFRunLoop timer/retirement
probe passes CI214; this is separate from the native editor parent probe below.

Scoped X11 and Cocoa parent primitives are also implemented in the SDK, separate
from Product GUI adapters. On Ubuntu, an authenticated private Xvfb gate passes
three create/resize/deferred-close/retirement cycles with empty test stderr and
settled process ownership; the complete SDK library passes 439 tests (16 ignored).
The Cocoa actual-main parent probe awaits CI. These gates load no foreign view:
Product GUI still uses the Windows adapter, and cross-platform plugin attachment,
native input, full heap/resource and physical-display qualification remain open.

## Implementation boundary

| Feature family | Current implemented boundary | Remaining work |
| --- | --- | --- |
| Native discovery and isolation | Selected class/descriptor, fingerprinted native binary, supervised discovery and persistent processing child, bounded control traffic and shared buffers | Standard scan locations/workflow, permitted vendor corpus, platform and native editor lifecycle |
| VST3 SDK interfaces | One canonical runtime binding package generated from verified SDK 3.8.1 sources; independent layout/signature and actual cross-language calls for all nine methods of the four new interfaces | Optional host-service implementation, MIDI 2 event routing, native MSVC/macOS/Linux qualification; upstream generator excludes SDK testing interfaces |
| Audio layout | Native Mono/Stereo Main and mixed auxiliary inputs; named types retain exact native type/arrangement and width checks. CLAP additionally maps arbitrary NULL/empty fixed Main/output lanes to the requested host Mono/Stereo contract, and one/two-lane arbitrary Aux to host Mono/Stereo by explicit ordinal identity. Native speaker declarations are not invented. Both formats admit up to 31 ordered same-layout auxiliary outputs through the existing DAG. CLAP retains absent-main and auxiliary-only inputs; VST3 absent input requires explicit Some Note and no auxiliary inputs. | Wider arbitrary/custom types, surround/ambisonic, full topology/configuration recovery, broader third-party auxiliary sound and product output-selection workflows remain unqualified or refused. |
| Processor-output graph | Track/Bus processor identity plus Main/Auxiliary key uses the existing routing DAG, producer PDC and Session storage; raw producer tap precedes later rack processors and Mute, retaining Track Solo semantics; schema 33 preserves version-32 author values. CLAP native output IDs and VST3 direction-local output indices bind to this same graph | Product output selection, other rack/owner families, broader native adapters and realtime/performance evidence |
| Audio parameters | Canonical author curves and sample offsets; CLAP plain values, VST normalized values; insertion/rebinding/history/archive state; one App custody slot for single-owner gestures or terminal frontiers; finite single-CLAP and two-owner VST3 headless Session/Media/Audio handoffs | The first headless consumer requires terminal, direct-root, same-object preserved execution. Broader multi-owner frontiers, Scope/nested execution, native display conversion, units/programs/presets, output events, broader gestures and large state/resources remain unqualified |
| Audio render mode and threads | Explicit VST realtime/offline selection; CLAP requested-mode probe and execution, hard-realtime/declined-mode refusal; instance-local CLAP symbolic thread checks | Dynamic restart/rescan/latency/tail handling, sleep/request-process scheduling and extension event loops |
| Audio transport and events | Canonical samples and the sole Sequence musical reference supply seconds/beats/constant Tempo. Optional activity crosses the existing Runtime/PDC/cache/IPC21 path into native Playing; App forwards activity and Export supplies Playing. Entered epochs refuse changes before callbacks/PCM. | The current default-feature normal release worker (opt3/debug0/no debug assertions) passes18 full Windows native gates: original canonical transport and new meter Runtime controls; original/new App Media software-preroll; original/new public Export; four unchanged Note/author-CAS controls; and two real SurgeXT1.3.4 AppMain regressions, each24windows/6144frames. Original assertions,180s gate/190s outer/2s cleanup limits, stderr0 and settled Jobs pass. App parent is opt0/validation; Export parentopt1. Complete ordinary App3101/207ignored and Export298/18ignored pass with zero filtering; App uses the existing1800s whole-suite window. These finite software-output/offline conditions do not qualify physical Play/device, independent third-party musical response, dynamic maps/loops, realtime/performance or other platforms. |
| Constant meter and bar position | Partially Supported: optional positive-u16 meter in the sole Sequence reference; bar0 at quarter-note0, exact negative floor, no default4/4. Project schema35 preserves unknown legacy meter; IPC21 carries the complete reference. CLAP checks i32 bar/i64 ticks; VST3 computes full-width rational bar start and advances native chunks. | Current normal release passes both owned DLL DSP/PCM gates at44.1/48k x None/Stopped/Playing,7/8,two epochs,Scope5s/source3:2/PDC6, independent complete stereo PCM/author/owner/exit checks. CLAP preserves its object; VST3 seek rebuilds in the same child. New App gates use public seek63@24fps,S126000 and two actualB4 software-preroll windows crossing bar126002/PDC3. New Export gates use WorkArea65..66@25fps at44.1/48k,8jobs/12MOV-PCM24 or WAV-stem artifacts; all22104 stereo frames match exactly. Original None-reference App/Export controls also pass, giving16jobs/24files/44208frames across all four Export gates. Both fixture ordinary suites pass9 each; original short boundary controls remain, with finite samples-8..3rate and1..16384-frame callbacks. Core517/Project60/Audio873/SDK409 ordinary evidence retains its stated scope; latest Audio has193 ignores. Separately, real SurgeXT1.3.4 CLAP/VST3 authored7/8 Runtime gates pass120BPM/48k/B256/16384frames with bar1 at6000 inside Note127..8193, under the prior normal test-profile worker (opt0/debug0); this is a finite workflow, not an independent meter-response algorithm oracle. The public `ui.sequence/set_musical_time_reference` transaction and localized Musical time dialog now edit/clear the same exact reference. Full-Core decimal/fraction round trips, explicit owner, invalid/no-op atomicity, Undo/Redo/archive, cancellation/IME and existing Source/native-hold controls pass. Current App3115PASS/209ignored;6 owned App PCM gates include2 dialog-to-author-to-native paths, using a fresh App parent and existing normal release child. Widget/Shell localized paint controls pass; native window presentation/interaction, dynamic tempo/signature maps, physical RT/performance and other platforms remain unqualified. Drop/OS exit does not prove universal100ms graceful shutdown. |
| Bounded Note execution | Public direct-root eager CLAP Track pre/post scores and VST3 scores with an explicit musical reference use the original admission receipt, DAG, format-specific worker contract and exclusive Hosted ledger. Finite positive nonzero cold entries pass under the separate conditions below; completion and admitted channels remain explicit | App Playback beyond the finite owned preroll case, Export, windowed/precompiled/nested families, continuous DSP-history seek, automatic loop/reverse, broader instrument shapes/event dialects and physical realtime; third-party qualification is separate below |
| Native editors | Product hosting, isolated editor protocol and observation/author handoff exist; first signal and same-view refresh have dedicated qualification gates | Vendor/platform matrix, resize/DPI/focus/keyboard, remaining GUI/timers/FDs and destructive later-entry continuity |
| OpenFX CPU image execution | Filter Source/Output, full-frame RGBA Float32, with limited temporal Source access below, progressive; complete typed Straight/PM Float and Opaque signal, exact Normal Over, representation-aware cache and actual nested/opaque output; resident native instances. Source-free Generator Direct/Effects graph uses NoImage and one Output mapping; optional inputs remain disconnected, with finite Windows owned pixel/refusal qualification below. SupportsMultiResolution=1 refers to differing full native image bounds/origins ([ADR0015](../adr/0015-render-native-ofx-domains-before-viewport-publication.md)), independently of non-unit renderScale. Retained default release baseline passes19 original full gates:8 fixed-canvas,8 published Invert Direct and3 capability/PAR/readonly regressions. Existing schema5 records24 successful original graceful closures,23.16..83.67ms; the reused release integration parent has unchanged invoked OFX inputs. This finite recheck does not resolve historical intermittent F06 or qualify realtime/performance | Generator context remains limited, not fully verified; Finite existing-Clip Timeline and explicit machine-local Catalog qualification are listed below; dedicated Generator Clip remains unavailable; contextual Normalised defaults and public headless author/save-reopen have the finite Windows CPU qualification below. General/Transition/Paint/Retimer, other required inputs/formats/fields, mixed PAR/depth, tiles/scale/complete temporal and ordered scheduling remain separate; nonlinear/GPU/OCIO consumers retain explicit admission |
| OpenFX temporal Filter Source | Partially Supported: same-instance GetFramesNeeded before RoD/DSP, validated bounded ranges, effect/Clip declarations and original source service/bank/Control. Current Identity copies; temporal Identity renders normally. Windows MSVC14.44 published Natron-2.3.16-beta3 TimeBlur1.0 passes six C ABI calls:3x2 Float32/PAR1, independent full RGBA, fractional sources, accumulator, changed inputs, temporal Identity and same owner. First/last endpoints require transparent absence with alpha1/2 and3/4 and exact Source masks. Known temporal absence returns empty Source RoD without pixel fetch/time clamping; in-range failure, ErrMemory and invalid-time refusal remain. The actual image suite passes22 controls after the new boundary control independently fails on the old host; its original21 remain passing. Five real NLE controls pass with the fresh normal opt0/debug0/CGU1 default-feature Product: real FFV1 16x16/24fps uniform/varying media, public placement/exposure, actual PTS selection, independent96fps source reference and exact CPU Preview/Export working images; public shutter Undo/Redo, catalog startup restore and Project archive reopen with fresh owners; actual Source-wait public-seek cancellation/recovery; strict Headless/wgpu timed advancement, pause/resume, natural Ended/frame11 and final picture. All retain original budgets, empty stderr and settled ownership. Two public trim/move/retime/history controls pass30 complete256-pixel Preview/Export comparisons against unretimed192fps source images and literal tables. The3/2 forward/-3/2 reverse/hold case separates Sequence/Clip/media time; the unit-rate/two-frame-shutter case warmsAtOrBefore250ms, then requires the preceding208ms picture for a reverse additional Source at exactly250ms and retains Before through hold/Undo/Redo. Clip animation advances and author state is preserved. Both pass on the current validation parent and unchanged5fb4d923 Product; runtime/SDK source is unchanged. Source replies survive UI feedback-view turnover; original deadlines/typed stop preserve exact attempt/demand without persistent plugin failure. Source7, terminal identity1 and ordinary App3147/218ignored pass at98f97cc5; Rust parent paths are unchanged for this native-only fix. See [ADR0016](../adr/0016-plan-native-temporal-dependencies-in-the-existing-graph.md), [native pixels](../../tests/openfx_reference/temporal_third_party.cpp) and [NLE controls](../../crates/mondrian-app/src/app/preview_runtime/temporal_plugin_tests.rs). | Finite Windows CPU/placement qualification. The final native6 root is fixed: an after-range Source RoD miss propagated HostSupport Property(Failed) through Output RoD to the generic C ABI error. All diagnostic probes are removed. An earlier interval safety failure is not independently explained; finite passes do not establish long-run/RT reliability. Other PTS/time-base boundaries, broader rates/placements/retiming/input variants, encoded Export, GUI/desktop restart, comparative performance, full heap/leak/100ms shutdown and other platforms remain unqualified. Automatic loop has no Engine contract. Temporal Generator/Output, ordered processing and General/Transition/Retimer contexts remain unsupported. Native OFX GPU interop and GPU pixels/performance are separate. No new executor/cache/protocol version or plugin-name policy. |
| OpenFX capabilities and bootstrap | Actual Float32/CPU advertisement, optional early bootstrap, CPU-disabled refusal and canonical PAR/progressive coordinates. Windows owned Clip Int/Double and Effect Double query controls pass Direct/retained-Graph assertions at 24/96/39 checks per frame. Shared negative GetN admission passes four raw types and Clip String at 18 Image/priority plus 12 String checks per frame. Fixed String setters now preserve declared dimensions for prefix and zero-length writes; real Param Label/Icon and variable Main PageChild pass 24+8 checks per frame, two frames per resident in both paths, with restored values and original pixels/Source/author/lifecycle. CPU/PAR/String original regressions pass. Current97595db8 adds shared effective-dimension admission before scalar/array reads: independent actual Suite fixed/variable/empty/hooked/chained controls change64 failed assertions to0; the CMake target also passes. The strengthened CatalogProbe reproduces old actual Render failure, then passes four-version metadata/Graph pixels; real Misc install/restore/Timeline passes37.80s. All9 current native controls and141 ordinary OpenFX checks pass (79 ignored), with original180s/2s, clean owners and empty stderr. | The new finite read-bound checks do not close the five older full property gates with original shared100ms shutdown failure. Graph cleanup does not establish natural100ms exit. Full Property V1, GUI icon/page refresh, general OOM/Notify atomicity, temporal/GPU execution, third-party, comparative performance and other platforms remain unqualified. |
| OpenFX author controls | Numeric tuples/colour/bool/choice/StrChoice/UTF-8 text/resources; canonical curves/shared transactions; contextual XY/XYAbsolute defaults and explicit default-only Sequence geometry rebind | Other spatial/time types, hierarchy/buttons/custom/parametric/interacts, dynamic descriptors, plugin-origin writes/private data |
| OpenFX product callbacks | Product AuxSuite classifies transient handles; typed current-scope feedback and bounded Describe-only/Catalog notices reach their existing owners; controlled question, progress/cancellation and Log tracing exist | Product navigation and scenario/platform qualification; unsatisfied gotoTime fails explicitly |
| Product interaction | Shared Rack control identity preserves keyboard ownership through canonical refresh; focused text owns clipboard and bounded local Undo/Redo; single-line visual BiDi/EGC navigation and associated caret geometry; exact model ingress with one-line source projection | Remaining control adoption, accessible numeric names, multiline visual navigation and physical/native-editor acceptance |
| Performance | Persistent children, bounded mappings, preallocated native audio buffers and execution budgets | Measured native DSP plus IPC percentiles/deadlines, video frame budgets, warm/cold/memory/overload and vendor/platform matrices |

Note preparation retains checked, fallible vector reserves, actual-capacity
acceptance, in-place ID sorts and the existing Project/Resolve/Bind/Retained
payload readers. String cloning remains infallible. Direct unwindowed selected
scores reserve B0 + compiler payload + maximum lowering-phase payload before
materialization; this logical reservation does not bound physical allocation
peaks or replace the final native/IPC/Hosted and author-handoff grants. A private
ledger helper carries the same non-cloneable receipt through actual Plan
preparation and completes the original Session accounting once. It preflights
every target before media, child or native-factory work. Local Track pre/post
targets in a direct root admit CLAP; VST3 additionally requires an explicit
musical reference. Standalone, windowed, precompiled and nested Note paths retain refusals.
Mixed-format execution has no native qualification.

Public direct-root eager Runtime Note processing requires an installed owner,
an explicit cold entry and positive contiguous forward blocks. The sole ledger
records the public cutoff S. A paired note with On < S < Off is retriggered at S;
earlier events and a proven ended note with Off == S are not replayed. Original
On == S events merge in the existing deterministic order, and later Off events
retain their original identity and velocity. PDC priming does not move S to S-D.
Paired immutable IR is validated once at construction, with indexed windows for
processing; cold-batch and actual render-window capacity checks remain distinct.
A render-capacity refusal after successful Enter preserves that new epoch; it
does not restore the previous producer.
Empty requests and invalid epochs still refuse; preflight failures preserve the
existing owner and clock, while post-native failures retain terminal semantics.
The discovered CLAP resolver uses the same restored
instance's schema-5 processing offer (embedded processing schema 3), then the existing registration checks,
worker specification, shared-block sizing, event merge and copied End ownership.
Plugin parameter outputs still pass through canonical author compile,
Source authorization/install, continuing immutable-score rebind and Ack.

Windows owned PCM-input Note controls qualify authored two-port scores at 44.1/48 kHz, literal per-sample output across partitions, release, parameter handoffs and invalid-entry refusal. New S5 controls pass VST3 first/retained entry with D0/D3, complete PCM/PPQ/native audits and monotonic retained IDs; CLAP D0 passes three same-object epochs with complete literal PCM and actual callback counts 4/0/4 through the original Poll/author/install/Ack. Signed-cutoff/underflow admission has ordinary coverage, without a new native negative-origin qualification. Fixed-seed real Surge VST3 separately passes first/retained nonzero cold retrigger and sustain. These finite offline paths do not qualify continuous DSP-history seek, App Playback/Export, arbitrary third-party instruments, realtime performance or other platforms.

App PCM preparation carries optional fixed-origin metadata through Media's
existing continuity model. Current Note Runtime metadata returns `None` while
`requires_state_entry` stays true, selecting `GenerationState` with explicit cold
entry. This removes the obsolete fixed-zero declaration; installed owner,
Source/author closure, resource/PDC and output-quiescence admission still apply.
Other renderers' fixed-origin contracts retain their generic preflight checks.
Pause, end-of-program and denied recovery suspend PCM without admitting Enter,
retaining installed Source/control ownership and callback retirement obligations.
A late Preserved Ack cannot resume suspended PCM. The finite first-nonzero
software-preroll cases below qualify VST3 and CLAP App paths; Export and idle warmup retain
separate, unqualified Note Source-installation preconditions.

For the ordinary None-prefix handoff, cancellation and claim validation precede
the actual acknowledgement. A confirmed acknowledgement completes successfully
even if its callback cancels the operation; it is not reported as retryable after
the receipt has been retired. Three ordinary controls cover these boundaries.

Host-model controls cover the corresponding Audio, Media and App handoffs; this adds no third-party or realtime qualification.

### Authored content extent and frame ranges, 2026-10-16

`Sequence::total_duration` now takes the maximum end of every Clip and authored
Note, including unordered lanes and legal overlapping Clip placements. Mute,
bypass, plugin availability and tail do not redefine authored duration. App and
Preview share the checked last-occupied-frame projection, `ceil(end)-1`, clamped
to zero. Export retains its exclusive-end ceiling and explicit Out/WorkArea
rules. No project field, cache, playback clock or second duration authority is
introduced. Nested Export controls qualify selected visual ranges and finite
Transition handles; they do not claim rendered transparent pixels or audio DSP.

The table describes implementation boundaries. Qualification below states the tested conditions; detailed consumer obligations remain in the linked audio and OpenFX feature matrices.

## Current qualification summary (2026-10-28)

Behavioral cases explicitly choose their relevant inputs; only default-value
tests assert project defaults. Resolve frame/sample targets from the authored
timebase with the canonical exact conversion, while keeping PCM oracles
independent of produced output. Quantized clock observations must declare their
representation uncertainty; do not alter production guards to accept false
zero-uncertainty test data.

CLAP input reconfiguration has finite **Verified** installed-width qualification:
Windows x64, owned modes19..22, Offline/48k/Stereo/block4/latency3, stable native
IDs/count/order, Main/output/parameter/Note contracts unchanged. Mono/Stereo detector
width changes only on legal deactivation, on the same child and original Root grant.
The product's existing pending-notice entry stages native planes and both shared
mappings, activates and captures actual state/latency/tail, then rebuilds the
Factory and canonical routes/PDC over the unchanged Program. The joined owner
commits matching native planes, typed metadata and mappings; Source CAS,
retirement and explicit Fresh Enter produce complete independent new-geometry
PCM (gain1 to gain2.5), including partitions. The original object/author remain
unchanged and the child exits0. Single reverse and three consecutive alternating
migrations also pass complete PCM/state/owner checks. Equal final shape/state
has equal normalized fees across repeated rounds, without accumulated capture,
codec, map or seed fees. Stable-key permanent metadata retains its original
storage, with widths updated in place. A real unrelated Source authorization
reaches the late WrongRegistration refusal after native commit: original Source,
cursors and HOLD remain, retry is refused and seed/overlap fees stay retained.
All overlap fees remain held until canonical retirement. Pre-cancel/wrong
tickets/duplicates and unarmed old-layout seeds are refused. Pending preflight preserves caller PCM; an admitted Unentered Runtime
refusal initializes it to zero, matching the existing CLAP/VST3 contract.
Actual App/Media software output also passes both width directions, including
public seeks0/1/2 before output quiescence: the latest seek target, original
Source/native object, unchanged author, retirement and complete new PCM remain
correct. Eight exact App native gates include the original CLAP latency/seek/
repeat and VST3 controls. An actual same-ticket Accepted seed-commit reply is
read and then suppressed by the existing test seam: the original Source,
cursors, HOLD and overlap fees remain, retry is refused and the owned child
is terminated and reaped. This qualifies controlled reply suppression only.
IPC25 normal Product, sixteen exact Audio native gates and real Surge XT1.3.4
CLAP state/Notes/Main Runtime regression pass with empty stderr/exit0/settled
owners. Audio ordinary899/206ignored and Media ordinary695/18ignored pass with
zero filtered and empty stderr. Full App regression remains **unqualified**:
3146passed/1failed/224ignored; the title cache-pressure progress test exceeded
its existing10-second deadline. Its isolated run, affected Preview cohort
(351passed/12ignored) and five repeated real font/raster lifecycles pass with
empty stderr and settled exit0. Timeout diagnostics now report the last typed
pending dependency and acquisition count, without changing the deadline.
The full-suite failure remains open pending root-cause evidence; no production
fix is claimed.

Primary and stem Export have finite **Verified** queue-to-artifact reprepare coverage:
Windows x64, Offline/48k/Stereo, actual immutable queue, one-second varying
Float32 WAV, PCM24 MOV and blocks9600 (maximum16384). Owned CLAP19/20 verify
both input-width directions; CLAP21 verifies four consecutive installations;
CLAP7 and VST3 modes5-to6 verify latency3-to7. Each successful primary artifact
and both stems decode to all48000 independent exact sample-frame references,
including every Fresh boundary. The two real native stem instances have
different authored gains0.5/0.25, detecting cross-instance state reuse.
The consumer owns the real Source and uses the original job token and shared
Runtime lifecycle. Stems reserve cold occupancy, then divide the remaining
occurrence/fixed/logical job grant into disjoint hard slices without increasing
defaults. Three quota controls cover exact fit, each insufficient dimension,
conservation, empty input and overflow. A real cold-fit grant with2MiB spare
completes the first PCM block but refuses native capture overlap before
allocation/publication. CLAP8's new request during capture also fails without
publication; both primary and stem failures leave no output staging or PCM
files in their dedicated process temp scope.
Thirteen full-queue gates plus four original CLAP/VST3 main/stem
transport/meter gates pass with the matching normal Product, empty stderr,
exit0 and supervised process-tree settlement. Export ordinary313/33ignored
passes with zero filtered and empty stderr. Untimed restart application is at
the actual block boundary, not sample-accurate topology automation. These are
owned controls, not wider third-party, long-run, heap or performance evidence.
The grant proof covers the existing Runtime ledger, not complete physical,
SDK/plugin or outer-consumer heaps.

Export now forwards original Audio child-owner settlement into the Queue's
existing state and lock. Cold factory work and stem measurements are observed;
the thirteen new queue gates require nonempty actual-owner registrations and
fully balanced terminal obligations. Live schema3/shutdown schema5 reject missing
JSON fields. Missing forwarding, counter faults and unconfirmed exit/reap remain
sticky failures. App's affected endurance cohort passes94/2ignored, including
owned-resource projection/overflow and current canonical receipt replay; Audio
ordinary899/206ignored passes. The independent consumer script passes three
baselines and rejects246 rehashed mutations under direct bundled PowerShell.
All Rust gates have empty stderr and settled supervised Jobs. This is finite
Windows/offline evidence; settlement does not prove graceful acknowledgement,
OFX child closure, full App/Workspace regression or F06 resolution.

To reproduce the thirteen queue cases, supply the matching normal Product
and both owned sidechain fixtures through their existing CLAP/VST3 helper and
fixture environment variables. Pre-create a private directory and set
`MONDRIAN_EXPORT_TEST_TEMP_ROOT` and the process temp location to it (`TMP` and
`TEMP` on Windows; `TMPDIR` on Unix) before starting the test process. Run
`cargo test -p mondrian-export --features validation --lib queue::tests::native_reprepare:: -- --ignored --test-threads=1`.
Only the Windows conditions above have been qualified.

Port additions/removals/reordering, full audio-port configuration/rescan,
physical reply-loss and long-run stress, repeated App width migration and
wider third-party dynamic topology, RT/performance and other
platforms remain unqualified or unsupported. No author edit, second executor or
wire schema is introduced.

Shared auxiliary-input layouts, 2026-10-27: finite Windows owned controls verify
Mono/Stereo Main with simultaneous Mono/Stereo and unconnected silent inputs,
two-route summing, PDC, target-time gain automation, partitioning and seek in
both modes/backends, in-process and through actual generic child processing.
All 899 ordinary Audio controls pass (206 conditional ignored, zero filtered,
empty stderr). Current IPC25 retains independent region/overflow, exact shared-byte fee and
old/future-version rejection controls. The two original CLAP/VST3 native
sidechain gates also pass with the matching rebuilt normal Product and original
ownership budgets. CLAP additionally passes owned direct ABI/storage controls
with Main present/absent; VST3 passes cold/retained semantic metadata and widest
port resource controls. Both pass real DLL plus canonical DAG gates: Stereo Main
with Mono/Stereo inputs and Mono Main with Stereo/Mono inputs, 44.1/48 kHz,
Realtime/Offline setup, native state, latency3, full literal PCM, partitions and
seek. All gates exit0 with empty stderr and settled original ownership budgets.
VST3 reuses SDK per-bus planes and validates input extents before native calls.
Broader layouts, third-party coverage beyond the finite ZamCompX2 VST3 case
below, realtime/performance and other platforms remain unqualified. See the
[shared contract](../adr/0003-compile-audio-authoring-for-execution.md) and
[owned PCM controls](../../crates/mondrian-audio/src/tests/mixed_auxiliary_inputs.rs) and
[native CLAP controls](../../crates/mondrian-audio/src/processor_isolation/tests/native_clap_mixed_inputs.rs) and
[native VST3 controls](../../crates/mondrian-audio/src/processor_isolation/tests/native_vst3_mixed_inputs.rs).

ZamCompX2 4.5.0 official Windows64 VST3 now has finite qualification at48k,
Stereo Main, Mono sidechain, block64, Offline. Actual discovery/probe, native
default-state capture/fresh restore,14 parameters (4 read-only), and canonical
DAG processing pass. With the same asymmetric low Main signal, silent detector
and disabled-sidechain cases return exact dry PCM; enabled high detector
gives positive gain below0.5 over the final512 frames, retaining the stereo
relationship. Complete returned PCM
matches across block partitions and seek. The
[native gate](../../crates/mondrian-audio/src/processor_isolation/tests/native_vst3_mixed_vendor.rs)
passes with exit0, empty stderr and settled original180s/190s/2s ownership limits.
This is not a complete compressor algorithm oracle, nondefault state round-trip,
GUI, realtime/performance, public Playback/Export or other-platform grant.

That probe exposed dependent COM references released after their parent objects.
The SDK now releases processor/view/connection interfaces before controller and
component anchors, then unloads the module while HostApplication still lives;
the failed-load guard preserves the same dependency order. An actual final
Release control fails before the fix (parent refcount4 instead of5), then passes;
the failed-load control and full no-default-features SDK411PASS/13ignored also
pass. The original Probe's three native warnings become empty stderr without
filtering. Audio885PASS/197ignored and all three affected owned native regression
gates pass with matching Product. This does not establish a universal shutdown
deadline or qualify unrelated lifecycle failures.

CLAP arbitrary-lane mapping has finite Windows owned qualification, 2026-10-28:
NULL and empty declarations on Main, auxiliary and output ports preserve their
actual configuration bytes and complete literal PCM through the existing DAG.
Both Mono/Stereo shapes pass at44.1/48k and both setup modes, including latency3,
partitions and three state epochs/seek. These are eight new configurations plus
the original eight declared-layout configurations in one expanded native gate.
The full16 native regressions, Audio901/206ignored and fixture9/0 pass with the
matching normal Product, empty stderr and original settled ownership budgets.
Zero/3/u32-max widths, custom types, identity/role faults and declared-width
conflicts retain strict refusal before activation. This is a host-selected
ordinal mapping, not channel-count inference of native speaker positions or
full configuration/rescan, Float64, realtime/performance or platform support.

ZamCompX2 4.5 official Windows64 CLAP has **Verified finite Runtime and
public Export sound**; overall support remains **Partial**. The original
[three-scenario gate](../../crates/mondrian-audio/tests/native_clap_zam_runtime.rs)
passes at48k Offline/Stereo/block64 with silent/enabled, loud/disabled and
loud/enabled detector controls. Each case renders three complete1024-frame runs
at starts0/0/97, partitions64/64,17/47,47/17: dry PCM, active attenuation/stereo
relationship, partition/seek invariance, two actual restart installations and
unchanged author state all pass. The lab-only initial author echo transaction
has been removed: the actual cold native owner completes verified pure rescans,
and the test requires no initial author proposal plus all writable current facts.

The [public RenderQueue regression](../../crates/mondrian-export/tests/native_clap_zam_startup.rs)
first failed Preparing with NativeAuthorPending(PendingCallbacks). After the
cold completion fix, the unchanged original gate passes through the normal
Product and original consumer: one complete48000-frame stereo PCM24 MOV, with
an authored Makeup12dB override. Every decoded sample matches input times
10^(12/20) within the predeclared two-PCM24-LSB DSP/quantization bound.
Both real gates have stderr0, exit0 and settled original Job budgets; Export
also verifies complete Queue shutdown and nonempty native-child registrations.

Nondefault state now has separate finite qualification. The existing inactive
SDK diagnostic flushes Makeup0→12dB, reads back exactly12 and calls the plugin's
state.save; its137 bytes are published after instance/library Drop. The
[App Project regression](../../crates/mondrian-app/src/app/playback/tests/cold_native_joint_controls/zam_state_roundtrip.rs)
passes public SaveAs/Close/new-App Open with both native-owned None and a
conflicting Host Some6dB over that same opaque12dB. The actual getter returns12
before host DSP; all11 native facts and nine writable author IDs remain distinct.
Each fresh, installed App Timeline/Delivery source renders all32 test frames;
every Float32 sample matches the independent gain formula within two Float32
epsilons, with unchanged author data and actual old/new Source retirement.
Realtime setup48k/stereo/block4 with a software output grants no realtime safety
or automatic transport/device/Preview scheduling qualification.

The public Export regression also passes both nondefault cases, each with all
48000 stereo PCM24 frames and the original two-LSB bound; the default-state
Host12dB baseline still passes. Capture, App and Export gates have stderr0,
exit0 and settled original Job budgets. Only tests changed for this extension;
the normal Product from69f05fc1 is reused with identical production sources.
GUI, complex or
larger states, arbitrary DSP-history snapshots and broader platforms remain open.

The [real App transport case](../../crates/mondrian-app/src/app/audio_rendering/tests/playing_host_fader_continuity/zam_transport.rs)
now passes four Windows48k/stereo/Realtime-setup cases: explicit24 and
30000/1001 fps crossed with native-owned12dB and Host Some6dB. Public Play,
Pause/Resume and two authored seek targets drive the original App/Media queues;
all current FIFO samples and actual software consumption match an independent,
position-dependent Float32 gain oracle within two Float32 epsilons. Initial
AudioDevice handoff, zero substitution/recovery/underrun, unchanged author
data/history and complete original2s cleanup pass; stderr0, exit0 and settled Job.
The test uses128-frame processing and4096-frame watermark inputs, supports a
partial final callback, derives positions from each project timebase and uses
the public paused position as Resume input. It does not pin a default FPS or a
wall-clock-dependent Pause frame. Integer-nanosecond timestamps round cumulative
sample boundaries once and declare their one-nanosecond quantization bound via
the existing adapter contract. Production sources/guards remain unchanged.
The deterministic software callback and existing headless video-ready seam do
not qualify a physical DAC, Viewer/GPU presentation, long runs or realtime cost.

Retained definition revalidation queries get_info only, without querying or
clamping transient meter values. Full cold/value and author projection checks
remain strict. Audio903PASS/205ignored includes an actual SDK seven-case cold
notification fixture: matching rescans complete, while value mismatch,
query-time notification, Dirty/Clear, unknown flags, pending flush and active
subsequent loads retain their work; restart intent and source sequence remain.
Cold-fix regressions pass: sixteen Audio native gates; Export313/33ignored
plus17 native gates; App audio13/27ignored, eight native reprepare gates, and
Queue-related94/2ignored. All have stderr0 and settled ownership. This does not
close the historical full-App title-pressure or F06 failures. Other rates, long
runs, reference-host performance and macOS/Linux remain unqualified.
GPL-2.0-or-later binaries/licenses remain external local test materials; the
release ZIP was discarded.

OpenFX owner project geometry now has finite Windows CPU qualification:
SessionProbe checks project10x7/PAR2 independently of image3x5, Direct image
PAR1→1.2→1, all15 literal pixels, same-project Graph owner reuse and project12x9
replacement. A separate raw App libtest checks rebinding refusal and original-owner
recovery; it does not execute Product IPC. Both new gates and five affected
Generator/Basic/capability regressions pass exact1/stderr0 with original180s/190s/2s
ownership budgets; worker gates use the normal dev Product. Fixed zero offset
only. CPU working values and cache now preserve independent PAR and X/Y sampling:
Effects352, affected Renderer207, cache19, affected App694 and Export4 controls pass;
Export includes two actual FFmpeg controls. The App selection and existing
exclusions are unchanged. Unit requests preserve the original Full CPU source
through transitions and lookahead while Any retains its proxy/quality policy;
the new odd-raster effect/mask and Export decode controls pass. Shared Any/Unit
requirements and pre-provider admission also pass strict checks. Native v11
sampling and a finite owned media-to-native Preview bridge now pass. Original
PPM3x5 with Clip PAR4/3 reaches Unit native processing before 2x2/3x2 CPU spatial
composition; complete pixels/sampling, same owner, author state and cleanup pass.
Eight related controls pass with the new normal dev Product. Six unchanged OFX
component controls use the pinned fa86 client, separately from the current worker;
this is not a current whole-driver rebuild or Media/cache qualification.
Unit temporal source materialization remains explicitly unsupported; actual
non-Unit native inputs refuse before provider work without quarantine. Full
Timeline editing/Export and other-plugin qualification remain separate. Spatial defaults,
Rectangle, full contexts, GPU, realtime/performance, old exit-root closure and
other platforms remain unqualified.

| Area | Result and conditions | Limit |
| --- | --- | --- |
| Canonical native terminal candidates | EditorState194 ordinary controls pass, retaining the prior187 and adding seven terminal-group controls. The existing Session privately prepares all members, rechecks delayed tickets, publishes once, retains no-change acknowledgement barriers and finishes one History group. A public save completion between preparation and commit preserves its current document revision, timestamp, save path and baseline through native publication and Undo/Redo. | Verified ordinary Session behavior only. Member inputs do not authenticate native proposals. The separate finite Audio installation below does not establish the Session/Media/App bridge or cross-queue retirement. Old single-owner nonterminal APIs retain their behavior. |
| Concurrent native-author admission | Two actual Windows owned VST3 mode13 instances process one complete four-frame buffer. Single-owner preparation refuses an unpolled peer's callbacks, then each peer's retained delivery; both original proposals, objects, Source claim and cursors survive. Audio774 ordinary controls and the new native control pass. | Verified negative admission only. No joint canonical update, Source installation, Ack or second PCM is exercised. The two test HANDLE observations share one post-Drop two-second deadline; this is not whole-shutdown or 100ms qualification. |
| Terminal native frontier installation | Windows owned VST3 mode13, two direct-root effects, Offline/48k/stereo/block4: the original peer-refusal control and a new joint control pass with the current normal Product. Complete retained members prepare one Plan, install once and acknowledge each actual owner. After the first Ack, cancellation preserves the second prefix; retry acknowledges only that member. Installation preserves cursors; SourceClosed and the next complete four-frame buffer pass on the same objects and epoch. Audio780 ordinary controls pass (774+6); App188 pass (184+4), including the shared GUI callback projection. | Verified finite Audio path only: terminal, same-object, actual Preserved execution. No Session-to-Media publication, headless App, Scope/nested/state-entry batch, third-party, realtime, comparative-performance or other-platform qualification. The logical Plan fee is not a whole-heap reservation. Native exit observations share a post-Drop two-second deadline; whole-Drop/100ms remains unqualified. |
| Native author queue and retirement | Media172 default and173 validation ordinary controls pass, preserving161/162 and adding11 terminal-continuation controls. The original queue parks the same prepared work across canonical publication; typed admission and phase keys reject stale attempts. Partial acknowledgement retains the installed claim and remaining members, including Suspend across its own installation. Stop, stale completion and replacement retire the original owner on the existing worker; panic poisons the lane. | Verified real Media queues with a portable provider. Exact thread, cancellation and fault-injection evidence does not establish those same injected cases inside a native SDK. The original pending frontier remains bounded; this is not a whole-heap, realtime, performance or other-platform qualification. |
| App native-author custody | The complete affected App manifest passes206 ordinary controls (196+10). One Single/Terminal slot retains the original gesture, prefix and snapshot independently of presentation. Terminal native preparation precedes one canonical commit/barrier; only real acknowledgement permits group finish. Current Media-slot identity guards strong renderer upgrades during source replacement or worker loss. A fresh normal Product passes GUI author, NoGUI ProjectClose, state/Project round-trip, retained playing fader and VST3 startup restart. | Verified ordinary controls and finite Windows owned-plugin regression. Three new controls initially held their own renderer aliases during shutdown; releasing those aliases after functional checks preserves every original exit assertion and budget. Ordinary provider controls alone give no additional native, realtime or other-platform qualification. |
| Headless native-author delivery | Windows Ring Out restores the actual185-byte nondefault seed and passes the original App headless gate with the fresh normal Product. All nine writable lanes remain None; an actual Unentered MainQuery reports GainOut=-6. One real terminal proposal traverses actual preparation, a single canonical publication barrier (including legitimate NoChange), installation, acknowledgement and one group finish. The same native objects remain Unentered with zero queued PCM; the new current claim is authenticated, the independent complete-Root SourceClosed check passes, and consuming cleanup releases all resources. The original failure is resolved within these conditions. | Verified single-plugin startup author handoff only. Captured state is checked by length, not byte equality with the seed. No PCM, GUI, Project round-trip, multi-owner App frontier, realtime, comparative-performance, whole-shutdown100ms or other-platform qualification. Original30-second work, two-second consuming cleanup and180-second/Job2s supervision remain. |
| App joint native-author frontier | Windows, two independently created owned VST3 mode13 effects, direct-root/Realtime/48k/stereo: separate one-batch and successive two-batch exact gates pass with the normal Product. Inactive software Preroll produces four frames, then retains those queued frames while appending four more. Each batch uses two actual new prefixes with advancing revisions and native process-call counts; the second starts from the first batch's actual post-ACK targets and claim. Each passes one Prepared event, one canonical update containing both actual opaque states, Author Generation+1 and Undo entries+1, installation, each real acknowledgement and one group finish, with fresh claim/plan/bindings. The same Source, native objects/PIDs, sessions, epoch and Media generation survive both batches. Independent complete-Root SourceClosed, old-target refusal and consuming cleanup pass. All206 ordinary controls also pass. | Verified finite one- and two-batch, two-owner App handoffs; two batches total two Generation/Undo increments, acknowledgements and finishes per batch. Software Preroll verifies authority and frame counts, without PCM sample comparison or device-clock/playback consumption; this is not Unentered startup. No forced partial-ACK failure, continuous stress, Scope/nested, Project reload, physical output/GUI, third-party, realtime safety, comparative-performance, whole-shutdown100ms or other-platform qualification. One total30-second work window, final two-second consuming cleanup and180-second/Job2s supervision remain. |
| Retained CLAP Runtime and Delivery restart installation | Windows owned modes7/13, direct-root Output with routed sidechain, Offline/48k/stereo/block4: original Source authorization installs the same child's seed and an empty Unentered Session. Explicit FreshEnter produces full PDC7 PCM; two restarts reach PDC3→7→11 under the same bounded Root grant, with actual old-owner retirement before resource normalization. CAS refusal and controlled authenticated-reply suppression preserve the hold and forbid retry. Latest Audio770 and App181 ordinary controls pass. The original eleven native controls retain their fixed qualification; install/repeat and Delivery pass again after the shared VST3 admission change. Delivery installation clears the old submitted window; Unentered refusal preserves external sentinel99, then Fresh windows at20/24 produce complete PDC7 PCM. | Verified within this fixed Windows subset: fixed ports/parameters, no Notes. Reply suppression is not physical TCP loss. Preparation grants no installation permission. Ordinary admitted Runtime refusal initializes its output, while Delivery preserves the unpublished external buffer. Audio-only controls do not prove Media/device quiescence; the separate App qualification follows below. No arbitrary DSP history, complete-heap, realtime/performance, third-party or other-platform qualification. |
| App/Media retained CLAP restart | Default Media158, validation Media159, App184 ordinary controls and eight affected native controls pass. Owned modes7/13 complete one and two successful restarts through the original producer, retirement and settlement queues after output-quiescence acknowledgement, on the same Source/child/load. Fresh Enter follows Unentered/no-PCM and produces complete canonical PDC3→7→11 PCM; activation waits for current-demand video Ready. The first restored run consumes the FIFO before the second restart. A separate mode7 control publicly seeks frames0/1/2 after Required and before quiescence acknowledgement; Fresh produces complete PDC7 PCM at the latest target3840. Seek rotates only PCM/output intent, preserves the original work/claim/settlement and discards held PCM without reviving late completions. It never acquires the Runtime lock; installation and Fresh entry retain their original authentication. Current claims/leases, author/history, same-object observations and consuming cleanup pass. | Verified Windows48k/stereo/block4, one instance, fixed ports/parameters and no Notes, with bounded software output. The second Fresh of the repeated-restart control is active with complete PCM but its FIFO is not consumed by that test. Native seeks during RPC/retirement, multi-instance or failed/retried recovery, physical output or wall-clock/device scheduling, arbitrary DSP history, whole-shutdown100ms, complete-heap, realtime/performance, third-party and other platforms remain unqualified. |
| VST3 latency-only configuration Factory | Audio770 ordinary controls pass. The owned Windows stereo/48k/block4 seed gate runs Realtime and Offline modes: the original Factory consumes a real mode5→6 component/controller candidate for exact flags8, returns latency7/tail7 and preserves all nine non-state metadata fields. Service and Commit keep the actual object; each Fresh Enter at100/-101 increments its serial in the same child, rejects the old Bind and accepts the new Bind. Original refusals and complete 7-frame native-delay PCM pass. | This adapter gate does not prove canonical PDC; the Runtime gate below does. Fixed metadata/no Notes. Known-host byte-buffer bounds exclude native metadata temporary allocations, stream COM wrappers and SDK/plugin heaps. Processing-mode selection does not establish realtime safety. No third-party, comparative-performance or other-platform qualification. |
| VST3 latency-only Runtime installation | Verified Windows owned mode5→6, direct-root Output with one routed sidechain, Offline/48k/stereo/block4. A real flags8 callback, possibly during PDC3 priming, enters the original retained capture/prepare/Source-CAS/Hosted-transfer path. The new PDC7 Session is Unentered with no old or new-address lease; actual old-owner retirement precedes resource normalization. Explicit Fresh Enter at20 reloads the component in the same child with a larger authenticated object serial and a new binding/claim/epoch. Complete public windows20/24 equal independent routed PCM, with public cursor28 and execution cursor35. Author state is unchanged; cancellation, wrong ticket/destination, post-install Source supersession and consuming cleanup pass. | Fixed ports/parameters, no Notes. This is Audio-only startup/restart qualification, without Media/output quiescence or stable-Playing recovery. Exact/one-byte-short/overflow capture budgets are ordinary pure-admission controls, not native RPC refusal tests; native retirement checks owner Drop and resource categories, with exact fee equations covered separately by shared ordinary controls. Native metadata, COM and SDK/plugin heaps remain outside the byte-buffer bound. No arbitrary DSP history, complete-heap, realtime/performance, third-party or other-platform qualification. |
| App/Media VST3 priming restart | Windows owned mode5→6, stereo/48k/block4: the original producer/retirement/settlement path installs on the same Source, revokes old Ready and leaves the new Session Unentered. Original Fresh Enter produces two complete canonical PDC7 windows with the same PID but larger component serial and a new claim/lease; activation waits for current-demand video Ready. Natural Media diagnostics delivers the current Gain=.5 presentation; author/history remain unchanged and consuming cleanup passes. App181, two fixture COM controls and the affected Audio Runtime/seed native regressions pass. The fixture now accepts normative zero-sample parameter flushes without advancing DSP; temporary diagnostics are removed. | Verified single startup/priming recovery with fixed ports/parameters and no Notes. Software clock advances through actual FIFO consumption; this does not qualify physical output, stable-Playing or repeated/multi-instance recovery, arbitrary DSP history, full heap, realtime/performance, third-party or other platforms. |
| Retained VST3 parameter and audio-bus projection | The stopped Windows configuration service compares borrowed facts without collecting another owned inventory. Raw parameter counts above 1024 refuse before Info queries. One fixed audio-bus frame preserves native metadata, cached ProcessData/activation ACKs, then fresh arrangement queries; every actual direction count is independently bounded to 32. Complete shape validation precedes inactivity and exact main/auxiliary key/layout comparison. The frame's actual Layout enters the original main-inline charge once. SDK404, Audio770 and App181 ordinary controls and the two VST3, three CLAP and VST3 App native controls pass with the current normal Product. COM success and legal-mismatch scans observe zero Rust host allocator operations in the tested SDK queries/callbacks. | Parameters retain the legacy successful-Info projection: failed Info rows are skipped; missing controllers and negative parameter counts remain empty. Strict author census is unchanged. Audio-bus counts outside the new bound, including negative fresh arrangement counts, explicitly refuse; within bounds metadata/layout/arrangement order remains, and every owned or borrowed arrangement getter now requires kResultOk before publishing its mask. Failure includes direction/index/status and stops without a partial view. SDK405 passes the failure-status matrix, frame reuse and existing zero-host-allocation controls; the previously qualified service conditions remain unchanged. Allocator observations do not cover the whole retained comparison/service, COM/SDK/plugin heaps or realtime safety. The separately qualified known-buffer family follows below. Third-party and other-platform qualification remain open. |
| VST3 cold/Fresh processing-buffer bounds | Windows Worker preparation carries immutable bus/channel/frame bounds and the known-buffer peak through strict IPC19 and the original Session reservation. Each actual allocation-driving shape is checked before allocation; exact capacity and old/new overlap use U=2P+2A+Q+W+2L+M. Deferred loading restores state before first preparation. A real owned DLL changes default six-channel audio to Stereo before its first Offline/48k/4-frame setup, with separate Event16 and literal PCM. The same control refuses default Realtime under identical restored conditions. SDK404, Audio770 and App181 ordinary controls plus 27 native exact controls pass: ten SDK, sixteen Audio/Product including real MDA, and one App startup/restart. The original Runtime latency failure passes after cold/Fresh validate exact activated latency/tail before Ready or Fresh installation. | Verified only for this fixed Windows subset and the known buffer family; SDK generic Eager/no-bounds behavior is retained. Discovery and manual Unprobed validation are outside canonical Root pre-admission. Worker and probe builders now seed the original request mode before preparation; Fresh reuses that Host. The SDK first-mode positive/refusal is actual-DLL qualification; Product first-setup mode is not independently captured. Other metadata, complete Arc/COM/plugin/OS heaps, physical commitment, whole-shutdown100ms, realtime/performance, remaining ignored native gates and other platforms remain unqualified. See [ADR0012](../adr/0012-restore-native-audio-state-before-preparation.md). |
| Shared processor outputs | Shared-slice baseline: 5,343 ordinary tests across eight libraries passed, including 16 new controls; 269 ignored tests excluded. Three strict groups and the normal Product build passed. | Track/Bus shared DAG qualification; native adapters have their separate conditions below. Product output selection, realtime/performance and other platforms remain open. |
| Media completion owner retirement | Four exact baseline failures reproduce caller/producer-thread destruction and publication after stop. All four pass after the fix and remain covered in the current 108 ordinary Playback controls (default plus validation features), including foreign Drop panic, 250 renderer plus 250 PCM capacity and the original 256/257 foreign-owner boundary. Only original retained Work enters the existing retirement worker; original180s/2s and compiler-idle boundary checks remain. | Windows ordinary qualification; one ignored actual MMCSS case is excluded. Native-child observation is separate below. No whole-shutdown100ms, cold-construction custody, realtime, comparative-performance or other-platform qualification. |
| Native child settlement observation | The prior Windows baseline passes Core509, Audio719, Playback105 and App74 ordinary controls, plus two actual OS-owner controls. Current/nested/held Sessions and Timeline renderer aliases retain counted scopes; only the original ProcessOwner reports its child's final result. Its matching normal Product/owned-VST3 Runtime-to-Media controls pass: real forwarding/producer registration, Retiring gives incomplete/unresolved, Settled gives complete/resolved, zero PCM and original180s/2s Job settlement. Queued App typed-refusal controls also cover retirement and external-alias unresolved receipts. | Verified within this finite admitted-instance boundary; the earlier false-healthy projection is fixed. Test-only HANDLE cleanup does not upgrade original-owner evidence. No residual Child custody, legal native App handoff, PCM, whole-shutdown100ms, third-party or other-platform qualification; [thread/heap limits](native-audio-standard-features.md#shared-playback-shutdown) still apply. |
| Cold native construction observation | Audio725, Playback108 and App76 ordinary controls pass (909, ten new); normal Product and the additional integration type check pass. The original App builder borrows its Media tracker domain through probes, validation and retained creation, with Session counting before populate and original-owner registration immediately after spawn. Four actual owned VST3/CLAP probe gates pass, including rejected registration preserving successful metadata without false settlement. Three actual App owned-VST3 gates pass: original Timeline/Runtime/Media preparation retires or leaves an external alias unresolved; actual binding Ready delivery followed by one original editor action returns NoEditor without changing author state or Source. All verify claim removal at shutdown. App76 and both prior gates were rerun after shared test setup extraction; unchanged180s/2s/Job/idle-boundary checks apply. | Finite Windows owned probe and zero-PCM/no-device App closure/readiness/refusal qualification. Idle can service diagnostics/native controls. This does not prove each App cold probe's registration, GUI attachment or legal native handoff. Independent discovery/create_instance, input inspection, Idle Warmup and precompiled Export construction remain unobserved. No residual custody, whole-shutdown100ms, realtime, full heap, comparative-performance or other-platform qualification. Existing App warnings remain; no strict GO. |
| VST3 current parameter presentation and owned editor authoring | Preserve now returns source/control facts without optional presentation queries; full-owner Drain/DrainAuthor retain selected plain/text facts; normalized author values remain separate. The original query-counter regression and current Audio846 ordinary controls pass. Four actual owned controls pass: full-owner Drain, live Preserve followed by post-borrow Drain, repeated same-object pending-restart preflight without consuming its cohort, and the normal Product App grouped-author workflow. The owned DLL retains its eleven-parameter census, three selected lanes and typed stale-Ack refusal. The refreshed App gate starts with three native-owned None lanes; the real GUI changes gain/balance to explicit .75/.25 while Mode remains None. The App retains ten callbacks, actual92-byte state, one Undo entry, unchanged PCM generation and PID/serial, same Source with a new registration, revoked old permission, exact Ack and Close. Separate explicit single-ID Worker COM controls preserve the native object, Source/cohort and None author values; the instrumented initial-values fixture counts exactly one metadata/value getter and refuses wrong binding or absent ID before getters. A separate owned Editor case passes three actual automatic MainQuery Facts through the original handler/cache, exact Open-time Adopt refusal with no side effects, and Closed Host-origin Adopt: one Some(.5), two None, history+1 and unchanged92B opaque/schema/UI. Stale capability refusal, public SaveAs/ProjectClose, new-App author-state reload and Source retirement also pass. Its visible row correctly offers no manual query and the query serial stays unchanged. A separate first-Idle-Open case now passes unequal Host static Some(.25)/opaque(.5), natural MainQuery .25/.5/0 and unchanged author/history, Source/claim, native object and Unentered state. A separate saved-Host-overlay gate passes public SaveAs/Close/new-App reopen, a new Source/Session/claim, post-Opened natural MainQuery .25/.5/0 and unchanged author/opaque/schema/history. Empty-key Host Some gain.25 overrides old opaque.5 while two lanes remain None; after Close, that same fresh Timeline Source executes Enter0/Continue4 with all eight literal frames exactly ±.0625. The genuinely observed old target refuses, and both Apps complete consuming cleanup. SDK411 and Audio846 ordinary controls, Audio Strict, and all six required native gates pass, including the original Surge processing and CLAP editor regressions. | Finite Windows owned fixture; eight software Timeline PCM frames are qualified only by the separate saved-Host-overlay case, without physical output or full App playback qualification. Static projection covers only empty-key Some; keyed curves, playhead/seek policy and native process-call counts remain unqualified. Canonical Unentered evidence does not count plugin-internal processing. The fresh-overlay gate preserves its original30s work/2s control and180s/190s outer/2s cleanup with empty stderr and clean owners; it adds no DSP-history recovery, whole-shutdown100ms, realtime, GPU, third-party or other-platform qualification. Full/live adapter objects execute in Audio libtest; preflight and App use the rebuilt non-harness dev Product (opt1/debuginfo2). The App uses one real Win32 key message. Original180s/2s Job, empty stderr and compiler-idle boundaries remain. No physical-input UX, third-party GUI, full-inventory getter-count, realtime, whole-heap, comparative-performance or other-platform qualification. App's25 existing test-build warnings remain; no App strict GO. The tag15 normal Product/App single-ID query is now verified with real Surge XT1.3.4 under the separate finite instrument case below; App273 and default Media179 ordinary controls pass. The Close-Adopt/persistence case itself still verifies canonical author-state reload only; it adds no native proposal/Ack or fresh execution. The separate saved-Host-overlay qualification does not extend that case or close the earlier Surge XT Effects None-reference failure. The separate real Surge XT1.3.4 Query-to-Closed-Adopt and public author-state save/reload case also passes under the finite instrument conditions below. The changed-path real Surge control also passes once; the earlier timeout remains open below. |
| CLAP presentation, owned editor authoring and Project state | The owned sample-gain fixture retains its event/state/Realtime and Offline PCM qualification. App GUI authoring starts with native-owned None and passes a real Win32 G, Begin/Value/End, explicit Some(.75) plus coherent four-byte state, one Undo entry, exact Ack and Close. Public Save before Ack refuses without a file, persistence request or DocumentHistory change. After Ack, public Save As/ProjectClose release the old owner; a new App publicly reopens the same Project with a new Authoring Session. Before host signal entry, actual getters return .75 rather than factory1.0 while the fresh Plan/Session remains Unentered; stale targets refuse. Original Timeline Enter/Continue then produce two four-frame WAV buffers, every sample exactly ±.1875. Separately, first Idle Open exposes an existing empty-key Host Some(.25) over opaque1.0 without author/history/authored opaque-state changes; after Close, the same Timeline Source produces eight exact ±.0625 frames. The independent saved-Host-overlay control additionally passes public SaveAs/Close/new-App Open with empty-key Some(.25) and unchanged four-byte authored opaque1.0. Actual new Source/session/claim and old-owner retirement establish Fresh ownership. After complete initial presentation, the sole GUI Open yields natural MainQuery .25/25.00 % in either actual delivery order with Opened; the real old target refuses while the new window stays alive. GUI Close precedes the same Fresh Source Enter0/Continue4: all eight stereo frames equal ±.0625, with author/schema/history/authored opaque bytes/query serial unchanged and both Apps consuming cleanup. Latest affected App280 and the new exact native control pass; prior Audio853/Audio Strict and five selected CLAP/VST3/Surge qualifications remain. | Verified finite Windows x64 owned-fixture static-first-Open and Project/parameter-state workflows. PCM uses the real Timeline renderer trait; full App playback, Media output and physical callbacks remain unqualified. The Fresh query does not assert a post-Opened cache or a larger revision. Canonical Unentered evidence does not count init-internal native processing. Original180s Job/2s cleanup, test30s work/2s Idle, empty stderr and clean ownership remain. Surge XT Effects1.3.4 covers same-object GUI and six initialization proposals/install/Acks, with no edit stimulus or third-party static Some qualification. Arbitrary private DSP history, physical-input UX, realtime/comparative-performance, full heap, whole-shutdown100ms and other platforms remain open. Existing App build warnings remain; no App Strict GO. |
| Surge XT Effects 1.3.4 CLAP App editor | Windows x64 normal Product passes actual Ready/Open, borrowed parent/native child verification, six initialization proposals through the original author/install/exact Ack path, same Source/native incarnation, Close and explicit Audio shutdown. One proposal arrives during pending Open and the same retained proposal is naturally redelivered after actual Opened. The generic App consumer now defers that exact case without author mutation or acknowledgement; ordinary target RED becomes GREEN. App82 and the owned CLAP plus four original VST3 App gates pass. | Finite Idle lifecycle case, no device or PCM dispatch, deliberate parameter edit, physical-input UX, resize/DPI, Undo execution/save/reopen, realtime/performance or other-platform qualification. InstalledAwaitingAcknowledgement was not observed. Original180s/2s, stderr0 and clean Job checks remain. The first failed test's panic-unwind outlive remains unresolved; normal Close/shutdown does not qualify automatic Drop or project-close custody. |
| Project audio close admission | Media158 (default), Media159 (validation) and App184 ordinary controls pass, with eight affected native regressions using the current normal Product. SourceClosed remains serialized on the original worker before Source retirement and persistence pause. Explicit close may request this proof after a terminal restart refusal once prior output obligations settle; only actual Closed and the exact current ticket/claim permit clear. Query refusal, stale tickets, author obligations and execution/retirement faults retain the owner and HOLD. Later output acknowledgements still finalize their original proof, while terminal Refused admits no further provider or service attempt. Public close of held and paused failed Timeline sources, strict fader/Undo refusal, claim revocation and off-thread release pass. The owned CLAP NoGUI ProjectClose and the seven restart/seek/fader/VST3 controls pass. | Partially supported, Windows. Refused-source closure is verified with registered portable and real Timeline/NotApplicable queue models; native DLL rejection recovery remains unqualified. Existing native NoGUI close is regression coverage. Reusable Playback, alias refusals, cancelled Open, binding latches and public-Play reinstallation guards retain ordinary qualification. SourceClosed observes the current prefix without sealing later callbacks; external aliases retain their original unresolved accounting. Playing native close, third-party ProjectClose, panic custody, whole-shutdown100ms, realtime/performance and other platforms remain unqualified; unrelated native exit failures remain open. |
| Idle CLAP history restoration | Owned CLAP G/install/Ack/Close followed by public Undo/Redo passes: Undo restores native-owned None and actual native1.0; Redo restores explicit Some(.75), with exact four-byte state, old Source/claim/presentation invalidation and fresh GUI value/text at100%/75%. Opposite nonempty history refuses while each actual view is open, preserving author, Engine, current Source/claim and window; original Close and complete consuming cleanup pass. App165 ordinary controls include changed/unchanged Idle audible bindings, Project-close history refusal and ProjectSnapshot postcommit native-hold custody. The six original native App gates pass with the rebuilt Product. | Partially supported: Windows owned fixture, Idle, no device or PCM; no third-party history, playing, save/reload, acoustic, realtime/performance or other-platform qualification. The ProjectSnapshot ordinary control qualifies Source custody and explicit refusal after publication; gestures retain the original postcommit guard without new actual qualification. Automatic audio resynchronization remains unqualified. Retained state-entry cache preservation is source-reviewed only; the passing original Media same-source control does not establish App nonempty-cache behavior. |
| Busy Sequence audio author admission | Nongesture Sequence edits use the original prepared Session ticket and compare complete current/candidate audible closures before publication when native Source replacement is busy. The changed Product output-fader control now refuses with unchanged author/history, Engine, Media, Source/output and hold facts; same-value no-op and unchanged-audio metadata pass. All App165 and EditorState187 ordinary controls pass, plus the existing nested-audio closure control. The rebuilt normal Product passes the seven original owned native App gates, including CLAP history and NoGUI ProjectClose. | Partially supported: ordinary Media host-model/fake-output admission qualification; the native gates are regression coverage, not an actual held-DLL fader test. No natural stored-binding-None case was exercised. Gestures and ProjectSnapshot retain their original postcommit boundary. The separate playing PCM control below does not qualify generic DSP/tail continuity. No third-party, realtime/performance or other-platform qualification. |
| Superseded Host fader reconstruction | The former configuration-only rebuild passed a three-frame PDC control but failed an owned zero-latency echo256 history witness after the public action. Root cause: the new Runtime/native instance could not recover live DSP memory from authored state. | The admitted static-fader case now uses retained-F publication below. The former Engine-A reconstruction controls are historical and do not qualify current execution or arbitrary DSP recovery. |
| Retained terminal Host fader publication | Audio738, Media122 and App169 ordinary controls pass, plus seven original native App regressions. Owned CLAP publication/exclusion and echo256/latency0 gates preserve Session/epoch/native UUID/Media/FIFO: C137/F65536, all old queued samples, eight new8192 windows through131072 and first256 consumed post-F samples match. The mode0/latency3 gate passes two actions at F8192/16384, all96 windows through24576 and consumption16640, same DSP with new claims/parent leases. A real Surge XT Effects1.3.4 VST3 Runtime gate now passes three4096-frame runs: two identical continuous references and a same-instance -6dB publication at F2048; full PCM and original owner/cursor/Drop checks pass. | Verified within these fixed Windows48k/stereo subsets; original180s/2s, empty stderr, clean Job. Surge uses original1150B state/block64, predeclared absolute1e-6; pre-F error0/post-F max3.73e-9. It does not close the separate original Surge None-reference timeout below or qualify vendor algorithms/tails/App actions. Repeat gate uses non-harness test-profile opt0/debuginfo0 Product and strictly retains the real converter result, including legal PhaseRegression; continuous AudioDevice is not qualified. Queued latency, broader DSP histories, physical/gapless output, RT/performance and other platforms remain unqualified. See [ADR0011](../adr/0011-preserve-live-audio-state-at-the-processing-frontier.md) and [limits](native-audio-standard-features.md#shared-playback-shutdown). |
| Owned zero-input CLAP | Six fresh mono/offline Runtime cases at 44.1/48 kHz passed literal 14-frame PCM, NoMedia, Source/author/install/Ack and same-object/Drop-exit checks. | Owned fixture, not general third-party instrument support. |
| SDK restart-source and parameter-input ownership | Twelve owned SDK initial/cache/lifecycle/musical native controls pass. Same-native-owner witnesses and non-wrapping Source lineage preserve newer raw/gesture/feedback suffixes and refuse external retirement conflicts. The strict zero-sample path consumes either the original validated offset-zero host batch or certified initial COM queues; only the actual successful initial handoff marks their origin, and every subsequent write invalidates it. Mode14 SDK, joined-Worker and Product IPC controls pass; the Product has no authored lanes and produces literal .75 PCM from a component independently restored to .375. Existing mode13 deferred-setter and invalid/missing-handler controls remain passing. | Strict input consumes once through the original processor/context; mixed, foreign, busy, poisoned or nonzero-offset input still refuses. The Product IPC lane has no native call counter; its second no-pending Poll retains the existing replay behavior. No new parameter authority, realtime or whole-heap qualification. Broader Runtime/third-party recovery beyond the fixed MDA control below remains unqualified. |
| Joined VST3 parameter-rescan collector and child acknowledgement | Fourteen joined-Worker controls and Audio831 ordinary controls pass, with package all-target/all-feature Strict. Exact16 accepts valid caption changes while the complete ordered processing projection remains strict, then delivers empty NoValueRefresh without synthetic author values. The real changed caption reaches the existing Fact path and same-object stopped comparison. Separate exact4 with a historical caption preserves Values; initial/Fresh controls preserve native state and None ownership. Default/steps/raw flags/ID/count drift, mixed20, unknown or retired ownership still refuse without consuming Source. | Finite Windows Worker COM qualification; the separate exact4 case does not prove a same-instance16-to-4 sequence. Captured labels remain offline snapshots, live captions use existing Facts, and no new parameter authority is introduced. The projection is not full raw ParameterInfo equality: shortTitle/units/unitId are not current product consumers. Previous SDK409/eight native and Product IPC/Runtime controls remain separate evidence. Broader semantic metadata migration, held CLAP Product Runtime, full heap, realtime and other platforms remain unqualified. |
| Canonical VST3 parameter-rescan handoff | Four current normal-Product mode13 exact controls pass: adapter IPC, direct-root Runtime, peer-pending refusal and terminal joint frontier. The Runtime verifies two real HOLD/proposal/Source-install/Ack cycles on the same object/session/epoch: samples0..4 equal input*.75; after accepting authentic A=.75, a separate canonical static-value transaction declares .5; samples4..8 equal input*.5 and produce authentic B=.5. Both captured component/controller states, increasing cohort serials, sentinels, media/cursors, claims and Drop exit0 remain exact. Audio all-target/all-feature Strict passes with zero diagnostics. | Verified for finite Windows Offline/48k/stereo/block4. The old second-round failure at next8 depended on a same-value setter suppressed by valid constant deduplication; only the test stimulus changed. This grants no App pending-edit permission. Existing Audio830 ordinary and eleven Worker COM controls retain their separate earlier conditions; they were not rerun for this test-only change. No Note, third-party, full-heap,100ms graceful-exit, realtime or other-platform qualification. |
| Owned zero-input VST3 | MNE6 SDK2/Product2 gates pass: actual native inputs0/null, one stereo Main output, literal PCM/PPQ/full same-PID audits at 44.1/48 kHz. SDK covers Some/None; Product uses Some/direct-root Notes, NoMedia, Source/owner continuity and cold Ready topology refusal. Restore reconciles native bus activation before processing; cold setup preserves pending-input error priority. | Owned Windows/offline fixture; absent input has no auxiliaries. NoNote generators, live topology drift, MDA, GUI, App Playback/Export, full heap, realtime/performance and other platforms remain unqualified or refused. |
| Local VST3 Note Worker | Explicit scalar On/Off request with an existing musical reference requires active Main Event0 and no event outputs; it derives at most sixteen channels and SuccessfulOffBlock completion. Three real Product/owned MNE4 gates pass: 44.1/48 kHz, negative/near-limit anchors, exact PCM/PPQ/tuples, enabled empty batches, cold/parent refusals and terminal poisoning. Conversion storage has one cold capacity/layout admission; IPC19 requires the separate layout while retaining eight fixed owners. | Local explicit native IDs; canonical qualification is separate below. No true zero-input instrument, third-party Note, realtime/performance, whole Root heap or other-platform qualification. Failed-child termination can precede its audit; terminal tests establish parent non-reuse and same-handle settlement, not failed-tail tuples or natural 100ms exit. |
| Canonical VST3 Note Runtime | Owned MNE4 canonical/local controls retain literal PCM/PPQ, 1025-pair recycling, refusal and terminal-Poison checks. Origin-zero reentry passes three epochs with fresh SDK objects in one child and monotonic IDs. New Windows/offline48k/stereo/block4 S5 cold entry passes all four first/retained × D0/D3 combinations: ended-at-S and priming-window notes are absent, chased and original-at-S Ons have deterministic order, original Off tuples survive, complete PCM/native audits pass, and retained IDs advance from 0..2 to 3..6. Input-present MNE7 upstream audits require exact active mask 1+2+4=7 and zero input events. Current Audio861 ordinary controls, strict all-targets/all-features Clippy and eleven affected native regressions pass independently. | Direct-root eager Track, Some musical reference, active Main Event0/no event outputs and actual stereo Main audio input. Reentry requires closed author/editor boundaries, the admitted configuration and one actual installed replacement permit; ordinary rebind retains its same-producer guard. No continuous DSP-history/seek equivalence, automatic loops, broader zero-input/third-party shapes, App Playback/Export, realtime/performance or other-platform qualification. Complete terminated-component audit is not callback-return/COM-unload/100ms acknowledgement; failed-tail PCM rollback remains unqualified. The first fixed VST3 run failed an incorrect input-shape audit expectation (6); correcting it to exact7 changed no production/fixture behavior or acceptance range. |
| App first nonzero VST3 Note preroll | Windows owned MNE4 with real Main audio input, Realtime/48k/stereo/block4 and Some120: public frame1 at24fps lowers to S2000, then the original App renderer and Media `GenerationState` enter once and enqueue two four-frame Preroll windows at2000/2004. All16 Float32 words match independent literals. NoSource/wrong-generation refusals, SourceClosed, unchanged claim/author, same native object with new entry binding and old-lease refusal, NoMedia and consuming shutdown/HANDLE exit checks pass. After removing the fixed-zero metadata refusal, a separate audit exposed a zero-frame native Process returning InvalidArgument; the fixture now accepts strict empty controls without advancing DSP and retains every real Process/fail_call ordinal. The complete passing audit contains zero/4/4-frame successful calls; three local COM controls and affected canonical positive/nonzero regressions also pass. | First cold software FIFO enqueue only; output remains inactive with no callback consumption. No zero-input instrument, physical device/clock, continuous DSP-history seek, recovery/reprime/automatic loop, Export/idle warmup, new third-party, other-platform, realtime/performance or whole-shutdown100ms qualification. The original180s gate/190s outer/2s Job settlement, empty stderr and clean supervised ownership remain. |
| App first nonzero CLAP Note preroll | Windows owned zero-input NoteGate, native Main Mono output and Mono Program, Realtime/48k/block4/D0 and Some120: public frame1 at24fps lowers to S2000, then original App/Media `GenerationState` entry and two actual software FIFO enqueues at2000/2004 pass. The existing unity Mono-to-Stereo mapping produces all16 independent Float32 literals. Stable note-port IDs0/2147483671 retain their distinct event ordinals. Full MNG1 state and two editable author parameters are preserved; native readonly Exact Offs23 stays outside author lanes. NoSource/wrong-generation refusals, SourceClosed, unchanged claim/author, same native object with new binding and old-lease refusal, NoMedia and consuming shutdown/HANDLE exit checks pass. The affected VST3 App gate also passes with the same rebuilt harness. | First cold software FIFO enqueue only, with inactive output and no callback consumption. The earliest NativeEnd at2008 and its parameter callbacks are outside this case. Program/native Mono and App Stereo delivery are separate contracts; the earlier test's conflation correctly refused before DSP. No third-party instrument, physical device/clock, continuous DSP-history seek, recovery/reprime/automatic loop, Export, realtime safety/performance, other-platform or whole-shutdown100ms qualification. Both complete gates retain original180/190/2 supervision, empty stderr and clean Job settlement. |
| App CLAP Note-end author continuation | The same Windows owned zero-input contract above passes the original four software FIFO windows at2000/2004/2008/2012: all32 independent Float32 words, four Gain17=.5 callbacks at2008/2008/2010/2010, one complete16B MNG1 coherent prefix, two timed Gain keys and one Undo/Author Generation/Sequence revision advance. A separate six-window case changes only the original second-port key61 Off2007 to2015. All48 Float32 words pass; the voice remains audible across the first real canonical/Source CAS/all-native-Ack for the2008 callback prefix, accepts its original scheduled Off2015 and releases through2018; the second CAS follows that voice's End. Two real prefixes, each with two Gain callbacks, publish keys2008/2018 and exactly two history/generation/checked-next revision advances. Each Ack requires a new claim/binding, old-capability refusal, current-target natural readonly23=1 then2 and strict SourceClosed before proceeding. Quiet2012/2020 reuse only the still-current authenticated fact. Both cases preserve earlier PCM, complete Score/other author state, NoMedia, library revision and the same Source/native PID/object/sessions/epoch/FIFO and exact cursors. The new six-window and affected original four/two-window gates each pass on305a61bf; original consuming shutdown/HANDLE and180/190/2 supervision pass with empty stderr and clean Job settlement. | The original four-window case ends both voices before CAS; only the separate six-window case qualifies this finite active-voice/original-Off continuation. Neither proves numeric native Note IDs, a DSP checkpoint or arbitrary continuous-history equivalence. Inactive software FIFO only: no device/clock, recovery/reprime/automaticLoop, Export, third-party, realtime safety/performance, other-platform or whole100ms qualification. VST3 source and its two-window behavior are unchanged; its prior finite result is reused, not rerun on this PE. |
| Native-probe Factory preparation | Fresh discovered VST3 and normal CLAP probes move their bounded input-name snapshot into the existing Factory; actual create still validates restored native contracts and names in the original same-child Ready scope. Ten new ordinary controls pass, including 0+1 versus 1+1 launches, keyed name equality/drift cleanup, storage/seed ownership, strict Discovery3/normal Probe4 and the 31-port boundary. Both native sidechain negatives require the exact restored-port error from actual create; original PCM/state controls pass. CLAP seed, overflow and retained controls pass. | Manual/generic and CLAP Note-offer specs retain real validation launch plus strict shutdown. Probe is not proof of start/split/Ready; a name change is explicit unsupported presentation drift. Root-grant coverage of cold probes, complete native heaps, realtime safety and timed overhead improvement are not established. |
| CLAP authored beat position and constant Tempo | The original exact native gate passes all four rate × None/Some Runtime cases at 44.1/48 kHz: two epochs, different partitions, literal PCM, Scope-local gain automation, 3/2 source retiming and six-frame PDC. The native mode11 consumer now checks exact beat ticks against its independently configured reference; None remains strictly seconds-only. Six additional normal-Product instances at −17/±2^47 samples each pass two entries and [4,4,1] blocks, full PCM, same object/PID and observed Drop exit0, including negative beats and zero crossing. Actual SDK overflow refuses before start/process or consuming its real pending flush; a valid same-instance call then processes. Fixture7 and Audio789 ordinary controls pass. | Owned Windows stereo/offline native cases; canonical Runtime uses 32 MiB local scratch and an explicitly unbounded aggregate grant. Large-origin direct consumers do not extend canonical Scope coverage. Signed limits/rounding have ordinary arithmetic/SDK controls, not exhaustive native coverage. No bar/meter/playing/loop, tempo map, selected nested musical execution, realtime/performance or other-platform qualification. The separate 2s Drop observation does not prove the 100ms graceful budget. |
| Candidate VST3 anchored musical audio | The bounded Worker gate now passes all eight 44.1/48 kHz × None/Some × signed-start cases. The canonical gate again passes Scope-local automation, 3/2 source retiming, two three-frame delays, six-frame PDC and two epochs with independent Q/Tempo bits and PCM. Continuous objects and host-serial seek rebuilds pass. | The previous failing temporary Factory validation instance is removed from the fresh discovered VST3 path; this does not establish a 100ms teardown fix. Canonical control uses 32 MiB session scratch and an unbounded aggregate grant. No full callback-count/order, Note/Event PPQ, realtime/performance or other-platform qualification; overall integration remains HOLD. |
| Owned three-output CLAP | Direct scoped DSP observed 38 same-object SDK Process calls across three partitions and two entries. The matching normal IPC18 Product passed 16 canonical Runtime combinations: 44.1/48 kHz, four Main/Aux/fanout routes, two partitions and two entries each; literal PCM, author-state serialization/restore, NoMedia, Source installation and same-object Drop exit0 passed. | Windows mono/offline owned fixture. Aggregate grant is explicitly unbounded; canonical child Process count is unobserved. Live private-DSP state capture, finite aggregate-grant acceptance, third-party multi-output, stereo native workflow, GUI, realtime/performance and other platforms remain unqualified. |
| Owned VST3 multiple outputs and native roles | Windows/offline: original MNE7 40 fresh SDK configurations, 36 canonical mono/stereo routing/PDC cases and eight direct 1/3/32-slot cases pass with the current Product. New MNE8 stereo/48k qualifies Aux0 without a native Main input plus three native Main outputs: two SDK partitions, four direct None/Some Note x signal/zero cases, and four canonical routed/unrouted cases preserve complete PCM, Notes, Source, NoMedia and native PDC. Two legal same-count cold role alternatives independently probe, then refuse the frozen identity; the original owner subsequently processes correct PCM. Audio798 passes. Cold count mismatch and unrouted-NaN whole-window refusal remain qualified. | Discovery6/payload5/raw12; the fresh normal Product uses the existing test profile with test:false, App opt0/debug0 and normal Audio/SDK opt1/debug0, features[]. Current Audio798, all three SDK and all seven Runtime gates pass after the arrangement-status and early-parameter-bound fixes. Full plugin and prepared contracts are compared at matching stages. Canonical session scratch is 32MiB with an explicitly unbounded aggregate grant. Thirty-two slots remain direct Product qualification; new role shapes have stereo/48k qualification only. No native live topology recovery, third-party auxiliary sound, realtime/performance, complete heap, other-platform or whole-shutdown100ms qualification. |
| Surge XT instrument VST3 1.3.4 | Windows normal Product/offline48k/stereo/block256, fixed51928B state and all2855 native-owned None parameters (774 automatable/2081 nonautomatable), Some120BPM, authored On127/Off8193. The original16384-frame control and two-epoch origin0 control (6144/8448 frames,57 blocks) pass. New cold entries S1024→7168 and S2048→8448 pass49 complete finite blocks; both fixed16-block sustain windows [3072,7168) and [4096,8192) have positive energy in every block and total>1e-12. Same PID/fresh serials1→2/new binding, old lease/target refusal and unchanged current Source precede author service; two real install/Ack handoffs, NoMedia and consuming Drop/HANDLE zero-wait exit0 pass. The extracted old origin0 helper also passes. Separate Realtime48k/block4 App Idle GUI/query/Closed Rack Adopt controls pass; query canonical UI row2048 uses vst3.param.1835234182 (MIDI CC12\|110), and Adopt persists one Some(+0)/2854 None through SaveAs/ProjectClose/new-App author reload. | Finite cold retrigger/sustain, without cross-epoch bit-exact or continuous DSP-history seek, automatic loops, terminal-Off acoustics or native FFI Note-ID evidence. One occurrence/64MiB fixed/16MiB logical; original180/190/2 budgets, stderr0 and clean Job. Actual nonempty rescan and held-request counts are zero in the new cold-entry case. App query identity is authenticated by its query witness, not a GUI-attachment object or native ordinal2048; the three App cases observe no attachment object or initialization exchange. Adoption uses the actual author scalar, adds one history/generation step and rewrites no opaque state. Author reload does not qualify fresh native overlay, PCM/device/physical interaction. Auxiliary sound, complete live presentation, full100ms shutdown, realtime/comparative performance and other platforms remain unqualified. |
| Surge XT VST3 App nonzero software FIFO | Windows original1.3.4 and fixed51928B state, Realtime48k/stereo/block256/Some120, all2855 native-owned None parameters. Public frame1 seek at24fps enters S2000 through actual App resource admission and the original Source/Media path. Twenty-four successful software enqueues cover [2000,8144),6144 frames; all PCM is finite and each of the fixed last16 blocks [4048,8144) has positive energy, total>1e-12. Natural terminal proposals complete original App canonical publication/Source CAS/all native Acks; every window finishes with actual SourceClosed and current claim, same Source/native PID/object/sessions/epoch and exact cursors. Complete Score/schema/None parameters, library revision and original seed remain; coherent opaque configuration may update. Original consuming shutdown, weak retirement, claim revocation and same-HANDLE exit0 pass, with stderr0 and clean180/190/2 Job supervision. | Finite inactive software FIFO; the window ends before Off8193. No release acoustics, independent DSP golden, continuous-history seek/active-voice-through-CAS equivalence, device/clock, recovery/automaticLoop, Export, realtime safety/performance, whole100ms or other-platform qualification. First run failed when the test treated real PendingCallbacks as permanent; a test-only typed observation now waits original pending obligations under the same30s deadline and requires the original validator to return Ok. No production validator/dispatcher change. |
| Surge XT Effects VST3 1.3.4 | Current Some-reference gate passes two restored Runtime observations plus native bypass: Windows/offline/48k/stereo/block64,4096 finite frames each, original1150B state/input, exact reference, Source closure and same-object/Drop exit0. The fixed root grant remains one occurrence/32MiB fixed/16MiB prepared. The retained-F reference control is separately qualified above. | Partially Supported: the earlier pre-correction None-reference sample failed on restore-b render preflight with native-observation WorkerDeadlineExceeded, after restore-a4096/exit0 passed. Exact block/RPC phase is unknown;100ms is the operation budget and10.83s the whole test duration. Original180s/2s Job is clean; do not infer successful Runtime exit assertions or a cause. Some/retained-F passes do not close it or the old post-ACK exit failure. No independent DSP/Tempo-response, full tail, GUI, realtime/performance or other-platform qualification. |
| Official AGain Sample Accurate VST3 3.8.0.0 | The unmodified MIT SDK 3.8.0 reference passes actual List/Probe7 with one native Main input/output, capture and fresh restore. Fresh None default-capture produces the original 56B raw state through the canonical base64 field; Restore7 preserves all 14 non-capture facts. The raw-state model and 128 KiB policy remain unchanged. Three canonical Runtime cases pass 4096 frames each: two fresh Gain ID1=0.5 cases match an independent half-gain PCM reference bit for bit; native Bypass ID0=1 matches unity output. Windows/offline/48k/stereo Float32/block64, absent musical reference, sequence-zero Hold constants, finite grant, Source closure, native owner and Drop exit0 checks pass. | Current normal Product is opt1/debug2/test:false; Audio libtest is opt1/debug0/features[]. The strict consumer binds the retained List7, Capture7 and56-byte seed, including Main-prefix count1; the separate fresh Restore7 facts match. The reference was built with SDK /fp:fast; inputs are exact finite dyadics and the oracle includes canonical Graph zero normalization. Constant-parameter/reference-plugin qualification only. Canonical Gain automation fails both 4096-frame partitions (64 and 17/31/16): 192 amplitude differences and four signed-zero-only differences. The same DLL also fails a direct SDK opt0/debug0 control, without Runtime/worker: three fresh eight-frame cases give constant0/ramp8/jump2 mismatches. All owners settle. The separate SDK opt0/debug2 ordinary suite passes412 controls, including literal COM sparse-point round-trip; this does not inspect the DLL producer path. The observed forward-step behavior agrees with the reference source advancing before multiplication; final DLL attribution remains open. No automation qualification, host offset workaround, offset Bypass, GUI, realtime/performance, other-platform or whole-standard qualification. Original exit failures remain open. |
| Ring Out CLAP | Existing Windows64 sample, version string absent; unchanged normal Product0fe4d854. Stereo default/Mono cold selection passes24 Runtime cases: native0/-6dB, Mono44.1/48k and Stereo48k, both setup modes,64 versus17/47 partitions; all4096 frames match independent gain math within2 Float32 epsilons. Six public MOV/PCM24 Export jobs cover both states/rates/layouts, complete PCM and actual format metadata within2 PCM24 LSBs. Four public Project SaveAs/Close/fresh-App Open cases now cover native0/-6dB with None/HostSome(-3):48k Mono Program, Stereo software delivery, block4 and complete32 frames from sample0. Restored native gain/bypass, author data/opaque/curves, fresh Source identity, original-owner retirement and cleanup pass. Native-owned PCM uses independent gain math; changed Host targets use a same-DLL/state direct SDK process reference at the original2-epsilon bound. Eight direct SDK controls observe4096 frames; processing events retain a native transition whereas inactive-main flush starts at the target. Initial constant-gain expectation was wrong; no host retiming or transition skipping was added. Original Zam two-case Project and owned VST3/CLAP Project/PCM controls plus13 ordinary audio controls pass; all current gates std0/exit0/settled, original30s work/2s cleanup. | Finite empty-filter gain, cold configurations and pre-DSP Project persistence. Runtime/Export and Project have the distinct conditions above; Project evidence is software Timeline/Delivery PCM, without device playback or full GUI. No complete filter/preset/DSP-history, arbitrary config selection/persistence, dynamic rescan, physical realtime/comparative-performance, whole heap or other-platform qualification. Earlier Stereo/bypass and Audio906 ordinary qualifications remain; historical F06/App pressure failures remain open. |
| Clack SDK Gain reference | The existing unmodified MIT/Apache-2.0 example source, built through a local standalone manifest, passes actual normal Probe4 with embedded processing3, restored four-byte gain state, parameter and port metadata at 48 kHz/stereo/Realtime/block512. | Contract-probe qualification only. The example is not an oracle for sample-accurate automation; its event batches can apply gain repeatedly to the whole buffer. No DSP/PCM, Factory, upstream binary/build-lock, realtime-performance or platform qualification. |
| CLAP port/config metadata | Optional Main and ordered auxiliary inputs/outputs have owned controls. Normal Probe4 requires names with embedded processing3; current full NoteOffer6 also requires the same-instance inactive input-name snapshot, with notes2 and processing3 unchanged. Older incompatible envelopes are refused. Current full6 passes raw ABI/wire controls and the real Surge App cases below. The owned parent-query, standalone2, public zero-input Note and three-output gates retain their earlier finite qualifications; they were not rerun for this slice. | Surge default configuration exposes one auxiliary input and three outputs; configuration selection and real auxiliary-output sound remain unqualified. Envelope versions are private host protocols, not revisions of CLAP or its shared DSP metadata. No active port-rescan or new configuration service is implied. |
| Surge XT 1.3.4 CLAP instrument | Original None and refreshed Some Windows/offline Runtime cases pass, with exact quarter-note zero -19/7 and duration3/4 for Some. Each restores 50,208 bytes with NoteOffer5/notes2, NativeEnd and sixteen channels, then processes 16,384 finite Main frames with nonzero energy at48k/stereo/block256, On127/Off8193 and input cursor16384. Each completes one real nonempty rescan through the Track transaction, original Source installation and Preserved Ack, with same-object/cursor continuity, NoMedia0 and same-handle Drop exit0. Some retains its exact reference through compile, install and Ack; its latest run passes after adding explicit checked beat transport. Main1, auxiliary2/3 and zero-filled stereo auxiliary input32768 remain prepared. | Fixed finite cases, explicit unbounded aggregate grant. No independent Beat/Tempo/algorithm response, cross-instance PCM equality, release envelope, terminal-Off or auxiliary acoustics, GUI, App Playback/Export, project reload, realtime/performance or other-platform qualification. Drop exit0 does not resolve earlier shutdown failures or prove graceful ACK/100ms. |
| Surge XT CLAP App software FIFO and public project reopen | Windows original1.3.4, Realtime48k/stereo/block256/Some120, current code26ef884d and matching normal Product. A fresh None capture supplies50208B and775 native-owned None author parameters. Same-state NoteOffer6/notes2 includes actual input names, writable schemas, Main input absent and one-stereo-aux-input/two-stereo-aux-output shape. The original fresh-App playback gate passes. The public SaveAs/Close/new-App Open control also passes three predeclared complete fresh lifecycles: old Idle author work closes before saving, old owner retires, and public Open supplies the actual new Session/library with the complete saved Sequence including revision/opaque state equal before preparation; no cloned author/library/grant is injected. Each playback enters S2000 by public frame1@24fps seek and enqueues24 finite software windows [2000,8144),6144 frames; each fixed last16 block has positive energy and total>1e-12. Each observes one natural author prefix through canonical publication/Source CAS/all Acks/strict SourceClosed, with current claims, native-object and cursor continuity. Score/schema/None lanes and library revision remain intact; coherent opaque configuration may update. Both owner cleanups, weak retirement, claim revocation and HANDLE exit0 pass. The affected original VST3 playback gate also passes. All five complete native runs have stderr0 and clean original180/190/2 Job supervision. | Qualification is limited to these Windows software-FIFO cases; three fresh reopen passes are no statistical stability claim. The50208B capture does not prove identity with the old unavailable fixed seed. Only Main sound is qualified; auxiliary shape/NativeEnd negotiation does not prove auxiliary sound or Off/End execution, and playback ends before Off8193. No DSP golden/checkpoint, arbitrary continuous-history equivalence, device/clock, recovery/automaticLoop, Export, realtime safety/performance or other-platform qualification. The prior abc52e37 failure before SaveAs remains a real failure at the original validation-child100ms exit deadline. Complete NoteOffer6 evidence now avoids that redundant validation owner while preserving actual retained Ready and manual Unprobed validation. This is not a root-cause fix for F06 or proof of universal graceful100ms shutdown; cleanup exit0 alone gives no such proof. |
| Official mda DX10 VST3 3.8.0.0 | One Windows normal-Product/offline48k/stereo/block256 Some-reference Runtime passes64 distinct output windows/16,384 frames, On127/Off8193, 64 real unchanged-value canonical transactions, original Source install/exact Preserved Ack, NoMedia0 and same-object/cursor/Drop exit0 checks. At least one handoff has actual audible PCM before and after its same-request continuation; the test excludes initially silent prefixes from that acoustic witness while still servicing every hold. An independent SDK-only two-fresh-instance comparison observes On0 audible in its first window, whereas On127 is silent for the first256 frames and audible in the next window. Latest Audio Strict has zero diagnostics. | Fixed default-state case with an explicit unbounded Root grant. First-window silence remains a known limitation; no sample-accurate onset, native Note tuple/FFI-count/voice-state audit, terminal-Off acoustics, broader MDA recovery,100ms graceful-exit,realtime/performance or other-platform qualification. This corrects an unsupported test premise, not production DSP. The original unserviced flags4 gate retains its refusal semantics. |
| OpenFX CPU Filter and limited Generator | The independent BitCrush gate and finite native controls cover the stated current-frame, full-frame RGBA Float32/progressive CPU boundary. Three owned descriptor gates now verify separate Source/Output temporal refusal and disconnected optional admission; six existing capability/parameter regressions pass, including normal resident/Graph execution. The readonly-constructor correction passed 456 checks. Owned GeneratorProbe Direct and real NoImage graph now pass at 3x2/PAR2, times0/6/3/6 and gains.25/.5 with literal RGBA, retained owners and pre-native frame13 refusal; the separate unfetched-Output negative also passes. Product e98b0cc6 with test parent38091c0b additionally passes optional Source/Aux Direct/Graph, required Source and required Aux refusal, both original Generator gates and published Invert Direct/Graph: seven exact gates, empty stderr and clean180s Job/2s settlement. Required-input tests trigger lazy native construction and verify both Describe/request action records contain no Create/Render. Untouched official Basic authored/resident and SessionProbe capabilities regressions pass with empty stderr and clean original180s/190s outer/2s cleanup. | Published Generator qualification is listed separately below. Generator qualification is limited to this Windows owned fixed-canvas fixture, not a fully verified context or third-party qualification; these passes add no whole-shutdown100ms, whole-heap or realtime/performance acceptance. Timeline source materialization/Catalog insertion, contextual author defaults needed by official Rectangle, expanded/infinite RoD and ROI remain unavailable. Descriptor admission adds no temporal rendering; General/Transition/Paint/Retimer, tiles/scale/fields, GPU and other platforms remain outside this qualification. |
| Published OpenFX-Misc InvertOFX 2.0 | The official [Natron-2.3.16-beta3 Windows release](https://github.com/NatronGitHub/openfx-misc/releases/tag/Natron-2.3.16-beta3), class `net.sf.openfx.Invert`, passes real Discovery/Describe with nine exact Natron-build parameters. Direct resident RGB inversion/R on-off-on passes complete literal pixels and strict original shutdown. A separate canonical authored Effects Graph gate passes the original registry, seven author properties/two hidden defaults, nine actual bindings, static R/A=false and one persistent execution session at frames0/1/2. Both use 3x2 scene-linear Straight Float32 RGBA/unit sampling/project PAR1/24fps, preserve Source, PID and processor/native owner, and verify invocation1/2/3. The Graph also preserves author state, checks one actual receipt per frame, bank1/cache0 and Nondeterministic/Uncacheable. Each exact original gate passes with empty stderr and clean supervised Job settlement. | Finite Windows x64 acceptance for this binary and Filter scenarios only. Graph retirement uses the original void retire/drop API; it does not prove a successful graceful-shutdown Result or the100ms bound. No whole Misc collection, Timeline UI/Preview/Export end-to-end, state, GUI, other formats/contexts, GPU, realtime/performance, repeated-lifecycle stress or other-platform qualification. F06 failures in other controls remain open. GPL-2.0-or-later lab material stays external; no third-party source/core incorporation. |
| OpenFX XY defaults and published Generator | Finite Windows x64 Native Describe4/resident12 qualification: XY/XYAbsolute retains Canonical/Normalised semantics, resolves before Create/range validation, preserves explicit values and literal default-property fallback, and refuses old peers/invalid windows before processing. Published Misc Constant1.0 Direct passes0/6/3/6, exact unbounded Float32 RGBA PM pixels, same owner and original explicit shutdown. Published CheckerBoard1.0 also passes one full Windows gate at PAR1/PAR2 and8x4:8 Direct plus12 source-free Graph renders match fixed full-image literals, including four distinct/unbounded RGB colors, row orientation, canonical box sizing, linear curves/seeks, Reset64 and serde-restored animation. Both paths preserve exactly negotiated Opaque from a PM request; each Graph keeps one native owner, increasing invocations, NoImage input and zero cache. Both original explicit Direct shutdowns pass. Test83eec7af reuses unchanged native8906f289; no production changes. The affected Direct and existing literal-default Generator regressions retain their qualification. | Whole Generator compatibility, dynamic descriptor/default refresh, CheckerBoard fractional-alpha/lines/GUI and plugin-specific public project flow, expanded RoD/ROI, temporal/GPU, other platforms and comparative performance remain unqualified. CheckerBoard uses an alpha1 palette and one finite flow; its first PM-result oracle was corrected to the actually negotiated Opaque without weakening pixel assertions. Graph retirement does not prove natural100ms shutdown. Fixed IPC-storage estimates exclude native parameter/default arrays. GPL lab material stays external. Earlier native unit/scenario evidence retains its stated source/conditions; no full native-heap or universal100ms-exit claim. |
| OpenFX contextual author defaults | Windows CPU:355 Effects and126 affected App unit checks pass. One immutable literal base plus a project initializer produces concrete canonical PropertyBags; the same resolver performs full schema admission. Two canonical owner windows6x2/10x4 share3x2/PAR2 output and pass source-free owned Graph defaults, animation/seeks, explicit seed values, original Reset, serialization and changed-window refusal. Hidden/disabled spatial defaults stay native-owned. Published Constant1.0 passes a real16x16 square Project: public Clip effect addition, spatial Reset/Undo/Redo, color transactions, repeated actual pixels with one resident, SaveAs/Close/fresh App Open, complete saved instance equality and re-render. Native Direct and literal Graph regressions also pass. All five native gates have stderr0 and clean original180s/2s supervision. | Adapter/Graph parent f2af0c29, corrected public test d291d08e, Effects3886b723; unchanged normal native-v12 worker8906f289 reused truthfully. Zero-offset owner windows only; raw mismatched bindings still refuse. Explicit default-only Sequence geometry migration is qualified separately below. This is headless App authoring plus its saved Clip effects through the source-free Effects graph, not Timeline Generator source/Catalog/Preview/Export qualification. No other Context, temporal/ROI/GPU, performance/RT/platform or statistical stability claim. Graph retirement empties the bank without proving natural100ms shutdown; existing F06/GL failures stay open. The first public fixture used an invalid6x2 Sequence; only the fixture changed to valid16x16, with production validation and all assertions retained. |
| Contextual defaults in Sequence geometry edits | Windows:357 full Effects and93 distinct affected App checks pass. Two original public cases reproduce accepted settings with stale defaults/missing refusal and pass after correction: disabled nested-placement effects and every Grade version rebind in one transaction, child Sequence stays unchanged, and an unavailable later instance refuses without partial settings/name/default/history publication. Current normal Product and App parent b3e6fa60 both pass the new real Constant1.0 resize/PAR/history/Reset/SaveAs/Close/freshOpen gate and original fixed-window flow. The new flow changes16x16/PAR1 to32x16/PAR2, preserves current values/IDs, supplies Reset default64x16, verifies new native ownership, full pixels and complete saved settings/effect equality. All gates have stderr0 and clean original180s/2s supervision. | Only concrete defaults and construction geometry change; canonical values/curves are never inferred or scaled from equality to old defaults. Old/new complete schema admission is shared with render preparation. Structural or unavailable-context migrations refuse; ordinary literal definitions retain behavior. The test explicitly retires its own old-window processor bank, then resets to a full-window Generator before rendering. No automatic Preview retirement, partial/expanded effect RoD/ROI, Generator Timeline/Catalog/Preview/Export, transport-stop atomicity, RT/performance/long-run or other-platform qualification. Existing F06/GL remain open. |
| OpenFX finite native image domain and transient admission | Verified under finite Windows x64 CPU conditions. Ordinary Product465b552e (dev opt1/debug2/default features) passes12 native gates: original unchanged128MiB Invert Graph; exact319/320-byte authorization/refusal/reuse; real Graph denied before Begin/Render/End with retained feedback/owner, no quarantine and successful next frame; owned finite-domain Direct/Graph, published Constant Size/CheckerBoard, prior Generator/contextual/Invert and two public author/history/save/reopen flows. Effects360, App host99/provider10, Renderer672 and native image-ownership20 PASS; Host62/Renderer39 cases remain ignored. Final App parent e53b7ae1; stderr0/clean original180s/2s native supervision. | Original fixed128MiB reservation regression resolved by actual transient authorization from the existing Effects bank, with the original test budget unchanged. Fixed viewport Graph values; no full upstreamROI/variable-domain Graph/Tile/temporal/non-unit scale/GPU/Timeline Generator/whole-heap/RT/performance/platform grant. F06/GL stay open and protected integration remains held. See ADR0015. |
| OpenFX Generator in existing Timeline Effect placements | Verified finite Windows CPU/Full16x16/24fps with current ordinary Product/App df20a931 and published Misc Constant1.0. Public Solid/add/color, same Preview owner, Undo/Redo, five phases each0/6/3/6; actual Export working-frame+semantic-trace equality, SaveAs/Close/freshAppStateOpen/exact Clip serde/new native owner. Requested StraightFloat staging and actual PremultipliedFloat/sampling/associated RGBA are independently checked. Core361, Renderer673/39ignored and App Preview38 ordinary checks pass; original180s/2s/std0/stderr0/clean native supervision. | Existing Clip canvases and sole compiled graph/session; no new source/parameter authority. Renderer other placements/mixed Source/root+nested routes have ordinary owned-provider evidence, not corresponding native qualification. Native is static Constant/root Solid/working-frame Export; no encoded file, physical GUI/device, animation/Loop/resize/nested-native, dedicated Generator Clip/Catalog, upstreamROI/temporal/Tile/Reduced/GPU/RT/performance/platform grant. GL/F06 stay OPEN; protected integration HOLD. See ADR0015. |
| OpenFX explicit Catalog context and exact exported version | Verified finite Windows CPU with ordinary Product cf3325ab and App parent5ab5cdac. Filter/Generator selections persist independently; preference v4 migrates legacy Filter paths. One supervised metadata process retains a full native Host/cache/module lifetime per class, exact identity and binary checks, bounded acknowledged packets and original deadlines. Existing descriptor `(identifier, major, minor)` binds Catalog/Describe/factory/resident/SDK; legacy identifier-only ambiguity refuses. Four out-of-order versions2.1/3.0/1.0/2.0 pass exact metadata and Graph pixels; mutated export refuses before SetHost, foreign constructor exception dies before module retirement and a fresh worker handles the later class. Real123-header Misc Catalog install/preference restore/public Timeline roundtrip passes42.62s. Nine exact native gates include healthy/crash/cancel/context and retained-bank controls; each exit0/stderr0/clean180s/2s. Ordinary OpenFX141 PASS/79 ignored. | Catalog2/Describe5/resident15, unchanged Effect keys and Project schema; only OFX execution/cache identity advances. Original shared120s and resident100ms unchanged. Earlier real-gate timeouts, version-manifest rejection and display-label test failure are superseded only for this scope. No dedicated Generator Clip, other contexts/ROI/temporal/Tile/GPU, physical GUI, encoded export, statistical/RT/whole-heap/comparative-performance/other-platform qualification. GL/F06 OPEN; integration HOLD. See ADR0015. |
| OpenFX current-Source identity | Implemented, not fully verified: current-time Source reuse requires matching connected FloatRGBA, full extent/PAR and alpha. After an identity query, only ReplyDefault enters the original Render path; incompatible preflight cases retain Render without querying. Unsupported OK selections refuse without publishing. Three owned Windows 3x2/PAR2 frames reach literal pixels, immutable Source, same PID and actual OK/Default/OK with three queries and one Render. Six original unfetched, CPU, PAR, alpha and Render-error/recovery gates pass with the rebuilt paired Product. Two owned Windows direct qualification-child CABI controls pass under Lab 30s/2s supervision: Basic returns OK, then selects Output or time+1; both return exact7 with query1/Render0, UNSET/invalid alpha, unchanged raw staging and immutable Source. | The identity gate still fails at original shutdown DeadlineExceeded; its later Rust ordered assertion was not executed, although the complete native action record independently confirms order. Older animation and fixed-canvas samples reached their direct functional assertions before shutdown failure; Graph was not reached in those failed fixed-canvas samples. Those failure records remain HOLD; a separate newer instrumented fixed-canvas full gate passes under the conditions below. The two raw rejection controls do not execute the Rust wrapper or normal Product. Identity-error, other unsupported selections, overlap, identity Graph/cache, third-party, GPU, comparative-performance and other platforms remain unqualified; old exit failures remain unresolved. |
| OpenFX metadata notices and lifecycle | Windows: 76 distinct ordinary controls and 22 actual owned/official Basic cases passed with the fresh normal Product. Describe returns six ordered UTF-8 notices without Render; the live Catalog delivers four feed notices without replay or author mutation. Exact 32/33 limits, Fatal/invalid-UTF-8/native-failure prefixes, cancellation, single Load/Describe, failed-Describe Unload and live descriptors during Unload passed. | Schema2 metadata requests only; whole-request context is not emitter or Render identity. Per-request32 notices, per-bundle256/1 MiB text+id, one result slot. Log uses the existing tracing path; sink output is not separately qualified. No whole-heap, realtime, third-party, GUI, GPU, other-platform or comparative-performance qualification. |
| OpenFX instance animation state | Windows normal Product: IsAnimating now derives from the existing Constant/CurrentFrame/Curve source before plugin callbacks. The pre-fix CreateInstance query returns0 instead of1; after the fix, all nine same-current-value query/notification/pixel/PID phases pass. The bounded fixture also checks Destroy; its complete resident record ends with Unload return0 and reports no property mismatch. Four unchanged curve/query/lifecycle gates pass exact1/stderr0 with original180s/2s Job settlement. | The new animation gate still fails its final shutdown deadline and remains HOLD. This does not close earlier exit failures or qualify plugin authoring, third-party, GPU, performance or other platforms. |
| OpenFX Boolean Hold animation | Verified finite Windows owned SessionProbe controls with the paired Product/integration validation dependency graph. Nine same-current-value source/query/notification/pixel/PID phases, both explicit shutdowns, seven forward/reverse seek frames and five shared-Graph pixel/author checks pass. The separate nonanimatable curve gate rejects native status4 before Create/Render. Both original gates pass again on the Choice candidate with empty stderr and clean original180s/Job2s settlement;107 ordinary OFX and the separate206 affected App controls pass. | Boolean animation is enabled. Choice has the restricted qualification below; StrChoice/String/Custom animation remain disabled. The same Child handle supplies OS-exit waiting, preserving the shared100ms Send/Closed/exit deadline, late-status refusal and sole-owner cleanup. This does not close the prior exit cause or prove independent natural shutdown of Boolean Graph residents. No strict-default graph, third-party, realtime/performance, GPU, long-run or other-platform qualification. |
| OpenFX Choice Hold animation | Partially supported. Fixed ordinal-domain/Hold implementation passes four Windows owned native refusal gates: nonanimatable curves before Create; mismatched descriptor count before Create; actual Create count drift; and successful Render count drift before output publication. The positive gate passes nine source/query/notification/pixel phases, its first shutdown and seven seek frames, then fails the second shutdown with DeadlineExceeded; Graph checks remain unattempted. The original constant index/label/rank/calculus/pixel/Graph gate passes after correcting its fixture's omitted-parameter assumption. | Curve schema keys must be exactly0..N−1. Any count change, including A→B→A, invalidates the same session; same-count labels/ranks are presentation. No second domain authority or dynamic metadata refresh. Direct calculus refusal controls explicitly bind CurrentFrame; canonical Graph inputs stay Constant, with strict source-specific oracle branches. A separate fixed CurrentFrame CABI gate passes; the mixed fixture alone does not prove the sampled branch. Full positive/Graph closure, P21 refresh, third-party, realtime/performance, GPU, long-run and other-platform qualification remain open. |
| OpenFX scalar Angle | Finite Windows owned scalar Double metadata/default/ranges retain unwrapped degrees through the existing parameter model. The independent Graph passes seven seeks with a complete canonical Curve, keys/calculus/readonly queries, literal Float pixels, immutable author/input and one resident with Create/Destroy/Unload return0. Time and Double2Angle refuse at Describe with exact status7 and no Create/Render.109 ordinary OFX,206 affected App, the fixed CurrentFrame CABI and four original numeric controls pass. | Partially supported: direct passes nine source phases, seven seeks and three multiturn constants, then fails the original shared100ms shutdown; complete lifecycle remains open. Graph cleanup does not independently qualify natural100ms exit. Refusal does not distinguish C++ from Rust guards. Compound Angle is a host capability limit. Original180s/Job2s and validation dependency union remain; no strict-default, project round-trip, third-party, GPU, realtime/performance or other-platform qualification. |
| OpenFX parameter write admission | Windows owned SessionProbe: both String setters refuse local writes at readonly-GUI Begin, two Renders and End; a separate no-GUI two-Render gate refuses both ordinary/current-time Double and String setters with unchanged immediate values. All four Create setters and host A/B assignments, literal Float pixels, immutable author/input and original same-instance Close/Destroy pass. Original gui_cycle, rebuilt paired normal-Product String modes/64KiB/error-reuse and 96 ordinary OpenFX controls pass with empty stderr and original180s/2s Job settlement. | Finite ordinary CPU Render and internal readonly-GUI policy. Other setters share the permission predicate by source review; exception/nested restoration is source-reviewed only. Complete action-specific/analysis-Render admission and the C2 canonical author bridge remain unsupported; no native author Undo/save, Product GUI, third-party, old exit-failure closure, realtime/performance or other-platform qualification. |
| OpenFX primary failure and cleanup | Windows: the pre-fix exact shutdown deadline control reproduces cleanup Unavailable replacing the primary. Three post-fix controls preserve DeadlineExceeded, wrong-response detail and admission Canceled under injected OS query/kill refusal; the same real Child is subsequently reaped. Shutdown uses real controlled TCP; admission first prepares the normal Product and official Basic. Two original owner controls and the unchanged Product instance-reuse/unfetched-output regression also pass. All six exact gates have stderr0 and original180s/2s Job settlement, independently reviewed. | Controlled shutdown exchanges do not qualify native OFX Ready/DSP. Output-allocation, inner-render and GUI policy changes have source review and related regression, without new per-site fault injection. Original fatal exceptions and Retiring/sole-Child semantics remain. App builds pass; strict Clippy fails on ten unchanged diagnostic warnings. No old exit-failure closure, final-Drop bound, cold-construction custody, whole-shutdown100ms, realtime, third-party or other-platform qualification. |
| OpenFX exit observation | Prior child-owner, official Basic and six deterministic final-Mapping controls retain their conditions. One new schema5 fixed-canvas sample without a debugger passes its complete original gate: distinct direct and retained-Graph residents naturally exit0, completing graceful shutdown in90.54/74.18ms with no cleanup kill/wait. Inner return to OS-exit observation takes71.23/60.82ms in this instrumented sample. A separate debugger sample authenticates normal CRT exit, LdrShutdownProcess entry/return and natural exit0 on the same resident, but its parent observes exit after the original deadline and the gate still fails. | Earlier untraced fixed-canvas and Choice second-shutdown failures remain open; one passing sample cannot replace them. No extra App shutdown was found after the early resident return, but specific Rust/CRT/imported-module costs remain unidentified. Debugger and nondebugger samples cannot be combined into a timing attribution. The original shared100ms Send/Closed/OS-exit and180s/2s Job budgets remain. Current97595db8 normal Product/App Direct transient-admission gate additionally passes once with sealed schema5: graceful16.47ms/native drop3.65ms/inner-to-exit observation9.23ms and no cleanup kill/wait. Minimal same-binary network/thread controls show no tail; deliberately retained Misc/OpenGL reproduces55-72ms versus3.2-3.5ms after release. These are condition controls, not proof of the old Worker cause. The debugger-wrapped real Misc attempt fails its original120s install assertion before resident exit; it adds no normal compatibility/performance result. No common root-cause closure, long-run bound, comparative performance, third-party or other-platform qualification; see the [current diagnostic boundary](native-openfx-standard-features.md). |
| CLAP Windows timer lifecycle | Verified finite owned Sidechain fixture with Realtime and Offline setup. The original retained Product instance registers two timers through a main callback; a controlled due snapshot invokes A on the plugin-main/non-audio role. A unregisters B and itself, duplicate unregisters refuse, and no stale-snapshot or later-Poll callback survives. Ten ordered HostLog records, including both SDK refusal errors, preserve exact severity/text, sequence, loss checks and instance identity. Same PID/object and original Drop followed by zero-wait HANDLE/exit0 checks pass.780 ordinary Audio and the original native sidechain/state/delay/partition regression pass, with empty stderr and original180s/Job2s settlement. | A bounded fixture-only main-thread wait establishes both timers are due; registration order is not claimed as a CLAP requirement. No independent IPC-free idle wake, precise cadence, GUI-close persistence, outstanding-timer teardown, native capacity/ID exhaustion, DSP/realtime-performance, whole-shutdown100ms, third-party or other-platform qualification. |
| Airwindows inactive metadata, discovery and parameter state | Windows package 1.2026.285: two fresh-unseeded diagnostics and the normal None/Offline/48k/stereo/block64 Product probe retain matching 14-row metadata, with index6 default/current +0. An inactive typed flush changes Input Level from 0.5011872053146362 to 0.25; exact same-instance readback and bounded save yield 300 state bytes, published after instance/library Drop. A separate fresh normal Product CLI loads those unchanged bytes and its first parameter snapshot recovers exactly 0.25 without host parameter mutation; other values and metadata match the unseeded reference. These finite parameter/state checks pass with empty stderr, natural exit and original180s/2s settlement. Current ordinary controls pass as Audio774 plus two Runtime case/PCM controls in the existing integration role; the prior776 controls are preserved after moving the two module selectors, without duplicate registration. | Runtime PCM remains blocked. The first attempt stops at cold Source DspFeedbackPending before Enter. The test now services one original owner Poll before independently requiring SourceClosed; this change has source/ordinary qualification only. Its changed-source attempt stops earlier in fresh Runtime preparation: index8/id2944917348 returns default_nonfinite, bits FFFFFFFFE0000000, current0/range0..1. No cold Poll or PCM is reached; original failure and clean owner settlement are retained without retry. Pinned upstream [default storage](https://github.com/baconpaul/airwin2rack/blob/8a67b80e04230a5cf40e84d6e8b9a2fd70b13a75/src-juce/AWConsolidatedProcessor.h#L300) is read by the [default getter](https://github.com/baconpaul/airwin2rack/blob/8a67b80e04230a5cf40e84d6e8b9a2fd70b13a75/src-juce/AWConsolidatedProcessor.cpp#L58), while its [inactive branch](https://github.com/baconpaul/airwin2rack/blob/8a67b80e04230a5cf40e84d6e8b9a2fd70b13a75/src-juce/AWConsolidatedProcessor.cpp#L415-L419) leaves those slots unwritten. Exact DLL/wrapper build causality and the historical finite negative default remain unproven. No clamp or plugin-name exception. No author/Project, private DSP-state/PCM, GUI, PDC, realtime, comparative-performance or other-platform qualification; queried tail96000 is not measured DSP tail. |

The current local environment uses Windows x64, FFmpeg 8.1.2 and static OCIO
2.5.2. No macOS/Linux runtime or performance result is implied by these checks.
Normal Product retains ten existing warnings; the App library test build has
twenty-five warnings in unchanged areas. The current Audio library test build
has zero warnings and all780 unit controls plus two integration controls pass.
This is not a new strict Clippy qualification. OpenFX qualification is
stated separately above.

Current IPC18 retains bounded JSON startup metadata followed by the exact opaque adapter
bytes on the existing connection. It removes a second JSON byte-array encoding
that could inflate an admitted 512 KiB payload beyond the 1 MiB control limit.
Complete metadata is preflighted before spawn; the child validates version,
nonce and combined extent before reserving raw storage. Raw remains at most
512 KiB, metadata plus raw at most 1 MiB, excluding only the four-byte prefix.
The required nullable musical reference is checked in preparation, Ready and
callback binding; schema34 preserves absent/null legacy references and refuses
legacy-tagged non-null values. Eight ordinary wire controls cover exact limits, arbitrary/empty bytes, truncation,
stream alignment, old-version refusal and preflight before spawn. Original
adapter/Seed formats, decoded-byte accounting, owners and timeouts remain.
These are wire and individual-allocation ceilings, not a complete finite
startup-peak grant: metadata encoding/parsing and adapter decoding have temporary
overlapping owners. The actual Surge startup and later finite author/PCM result
have the separate tested conditions above.

Native rescan projection now fallibly reserves its tuple destination and moves
each ID/value in order. A real 775-value delivery previously became capacity1162
when a fallible collect reused a larger-element allocation, correctly tripping
Core's 1024 cumulative-capacity limit after 12 rendered blocks. The limit remains;
four Runtime DTO controls cover 1024, 512+512, aggregate overflow and missing
scalars. Input and target capacities are checked. The two Vec owners briefly
overlap on the non-realtime poll; retained-capacity checks do not establish a
complete transient-peak grant or whole-plugin heap qualification.

The shared author event carries `NoValueRefresh`, `Values` or
`ParameterSetAndValues`; CLAP retains its original raw flags and query facts in
the adapter. That typed migration introduced IPC17; the matching IPC18 regression
above also preserves `RequestOnly` versus the original `Cohort`, and distinguishes
a delivered prefix from a newer live pending cohort. Carrier matching alone does
not authorize raw retirement or Runtime recovery; the collector and child
acknowledgement have their separate finite qualification above.

## VST3 event contracts and Note execution

Required Discovery5/payload4 carries the expected event-bus contract, main-audio shape and native output count
through the existing probe, registration and payload owners. The actual restored,
configured, stopped instance is rechecked during preparation, cold rebuilding and
retained configuration. Canonical direct-root VST3 Note now requires the existing
explicit musical reference; None and unsupported entry scopes refuse before
media or native Factory resolution.

The shared Note contract now requires completion policy and admitted channel
width. `SuccessfulOffBlock` returns pairing slots after the actual successful
call, without claiming DSP voice termination; CLAP keeps `NativeEnd`/16 channels.
Whole-root admission follows actual priming/public calls, grants no same-call Off
credit, and preserves IDs and failure poisoning. Cached identity checks can be
quadratic; this foundation does not qualify realtime or performance.

The Windows owned test-profile Product passed eleven original native gates:
VST3 retained/seed and two event-metadata controls, CLAP retained/seed/overflow,
editor user-close/frame-clear retry, CLAP full/standalone Note probes, zero-input
public Notes and three-output Runtime/DAG. The latter
covers 16 rate/route/partition combinations and two entries with literal PCM.
MNE3 stable capture produces fresh ordinary PCM; unannounced default-flag drift
is refused on the same cohort/object with zero DSP calls. Post-Drop same-handle
Exit0 is an OS observation, not proof of the original 100 ms graceful budget.

The current Windows normal Product (dev opt1/debug2) and Audio libtest
(opt1/debug0) pass four configuration gates and 739 ordinary Audio controls.
The owned configuration gates use stereo/48 kHz/block4 without Notes or a musical
reference. The new VST3 gate reproduces the old parent rejecting an actual committed
3→7-frame contract, then passes supported closed-peer state-entry in Realtime
and Offline after the parent cache is synchronized. Existing seed refusals and
two fresh-Enter PCM checks remain; the new receipt, configuration bytes/digest,
actual object and same-child new incarnation are checked. Original VST3
retained/seed and CLAP seed regressions also pass, with empty stderr and original
180s/2s Job settlement. This does not implement CLAP author state-entry or full
canonical reprepare/PDC/Source installation, author Undo/save, old exit-failure
closure, realtime/performance, third-party or other-platform qualification.

The complete VST3/CLAP Worker Box header, including the fixed 32-by-2 event
contract, now enters the original lane profile and sole Root FixedResident fee.
Startup compares the actual dynamic Box layout before Ready; scoped lane checks
observe their own seven roles separately. Current IPC18 requires eight fixed owners and
rejects old or unknown shapes. Resource controls prove exact-header admission
and a one-byte-short Root refusal before Session factory creation. Validation
children run before the final Root grant; cold nested/transient allocations,
plugin-private heap and physical realtime remain unqualified.

The stopped-owner control and backend lifecycle-record fixes use the same
retained owner. The primitive census writes directly to bounded metadata without
collecting names; its successful-call probe observed zero same-thread Rust host
allocator operations. This does not cover plugin heaps or prove realtime safety.
The shared foundation passed 656 ordinary Audio, eight resource and strict Audio
Clippy gates. The SDK now checks the complete signed sample window before input
consumption or DSP, including continuous time only when valid. Five newly linked
Main SDK Note integration gates pass: public/scoped near-limit refusal preserves
PCM and same-instance audit; negative, partial and exact-limit windows remain
valid. Post-native counter failure keeps cleanup and the original plugin error.
The anchored musical SDK slice now passes 342 ordinary controls and sixteen
owned DLL gates. Its validated exact reference projects absolute signed samples
to quarter notes and constant Tempo, with nearest-even binary64 rounding across
native chunks. Independent mode7 DSP controls cover 44.1/48 kHz, local/scoped
roles, None/Some and negative/near-limit positions, including same-object
processing counts and queued-parameter refusal/recovery. Explicit checked Note
batches now project Event PPQ from the same immutable reference, original call
start and event offset, after whole-batch validation. Queued/staged legacy events
and busy/poisoned ingress still refuse before consumption. A real reference-counted
COM list, created during cold preparation even for None, temporarily owns the
original chunk vector backing. Only the native callback exposes its read-only
window; exposure closes before the original inputEvents pointer is restored.
Nonblocking query/cleanup faults prevent later Some and None processing; late
legacy ingress remains in its original owner. Native failure retains priority.
Only the current failed chunk avoids output copy: earlier successful chunks or
an outer cleanup failure can leave a written prefix, without event replay.
New Main-only MNE4 controls verify ten complete actual-PID records against
independent PPQ bits and PCM at both rates, scoped/local roles and signed anchors.
MNE5 tests actual retained COM references, closed views, cold release and its
44-byte component-state round trip without restoring observation counters.
The SDK storage observer reports the complete process Box, concrete COM payload
and unique vector capacity separately. This does not qualify the complete Arc
allocation or nested native owners against the Audio Root grant.
The candidate no-Note VST3 adapter now consumes the same Core reference without
a second clock or state source. Exact preparation/Ready/seed and original native
metadata/rebuild checks remain; selected nested musical execution stays refused.
Canonical VST3 Note has the finite Runtime qualification above. Its initial-entry
guard reuses the existing ledger origin and continuity state before native polls
or Source revocation; a failed target takes precedence over recoverable re-entry
refusals. The owned pure-audio canonical and real Surge controls above pass,
but the bounded Worker shutdown failure keeps integration on hold. CLAP retains
its separate seconds/constant-Tempo semantics; system clocks remain unavailable.
Older Gain/Surge and OFX exit failures remain open; a late reaped Exit0 never
converts a deadline failure into a pass. No third-party instrument or other-platform
qualification is implied.

The independent MNE4 SDK oracle admits Main Event0 with Aux inactive, preserving
the old three modes and SDK gates. Inline/scoped calls each pass two runs of 1025
pairs with literal PCM and complete actual-PID records of 515 calls/4104 tuples.
Its 1024-call/8192-event audit is reserved on control; complete records are written
only after processing stops, separately from the twelve-byte plugin state.

The same component retains declarations and accepted activation facts; a query
does not invent a native active-state getter: `host_active` records only that
owner's last successful activation ACK, independently of its restored default flag.
Preparation rejects topology drift.
Loading now limits event buses to 32 per direction and rejects invalid metadata;
zero-channel declarations remain visible. The checked subset requires nonnegative
note IDs, MIDI channels 0..15 within an active declared bus, valid ordered offsets
and an empty legacy input, preserves original
scalar values, and retires remaining input after a started processing failure.
It does not roll back an executed prefix. Local Product adaptation now lowers
the existing Note IR through that checked SDK with one admitted conversion Vec.
Its typed zero-capacity layout preserves element alignment; CLAP without this
owner uses its separate neutral layout. The complete Worker header is measured
independently, including when Note input is disabled. Canonical Runtime admission
and early refusal of unsupported re-entry pass the finite owned controls above.
Third-party
instruments, other platforms, realtime and performance remain unqualified.

## Open issues and integration constraints

- The temporal-clip baseline Output case failed before its Describe action chain
  and left a descendant for Job reaping. It is not a valid admission RED. The
  corrected candidate passes all three descriptor gates; this does not explain
  or close that startup/ownership failure.
- The intermittent Surge failure was reproduced after a valid shutdown ACK,
  with the child still running beyond the existing 100ms exit budget. Host
  termination and reaping succeeded; the resulting exit1 is not a spontaneous
  plugin crash. Bounded diagnostic runs have not located the failed phase or
  root cause; neither Gain nor Surge has been attributed to plugin destruction.
  Later successful runs do not establish a fix. No timeout widening or retry
  workaround has been applied.
- Windows/MSVC Product now delay-imports optional OpenGL (`bd3c8e83`), preserving
  the WGPU GL backend. Same-source controls verify both deferred loading and
  actual WGL getter resolution. In 20 matched early-error controls per Product,
  parent-observed error-line-to-exit intervals change from about 57–71 ms to 9–13 ms;
  these are neither child Drop timings nor realtime/performance qualification.
  The rebuilt Product's import table confirms the change, and the original
  Surge CLAP project-reopen/CLAP playback/VST3 playback gates all pass with the
  byte-identical prior parent harness and original budgets. The current OFX
  harness and rebuilt normal dev Product also pass four complete gates: PAR
  noop, DefaultOpaque, and published Invert Direct/Graph, with original pixel,
  ownership and shutdown assertions and unchanged budgets. Historical failures
  remain failures; these finite passes do not establish universal exit reliability.
  Actual GL validation remains incomplete: the existing `mondrian-surface-reopen`
  binary, built with `validation` and the same explicit delay-import flags,
  failed its first two-cycle batch with main-thread stack overflow (0xC00000FD)
  before publishing a report. Its PE retains delayed WGL imports. Native
  exception-stack capture localizes the overflow to Preview construction before
  Window GPU initialization: large by-value Host/Preview startup frames exhaust
  the default 1 MiB main stack. Candidate `796ab244` retains the existing Preview
  heap owner through Host publication. Startup regressions pass 52 tests (9
  originally ignored), including all five Preview partial-owner tests. The
  production-linked Host qualification passes 26 faults, two success routes and
  opaque-payload handling. The same Windows dev/validation image passes both
  original Surface/Device reopen cycles with explicit DX12 and actual presents.
  GL after the fix reaches GPU initialization but fails its original first-cycle
  60s deadline after the progress worker panics on the HAL WGL context lock;
  its failure report does not qualify cleanup. The process tree exits cleanly.
  No stack or timeout increase was applied. Physical native Window termination,
  pixel readback, native OFX GPU interop and performance remain unqualified;
  the ordinary Product (`09e649c5`, dev, no optional features) is now rebuilt,
  retains the delayed WGL imports and passes the FFmpeg/OCIO runtime CLI check.
  One instrumented follow-up did not reproduce the GL panic; it does not close
  the original failed gate or identify the context-lock holder.
  This isolates avoidable unused-image overhead, not the original F06 cause.
- The report's `exit_loop` timer describes the whole exit
  loop plus its last existing result. It is not the timestamp/duration of the
  final `try_wait`, nor a native Drop duration.
- The preceding IPC15 consumer run's public CLAP Note gate passes the first two
  44.1 kHz Track-rack partitions, then fails Factory validation for `[1;14]`:
  ACK at 0.325ms, still running at 108.703ms, and late Exit0 after kill/reap.
  It has no child-phase observation or evidence attributing the delay to the
  SDK change. Later partitions and all 48 kHz cases remain untested in this run.
- CLAP and VST3 native multi-output and the real Surge Main Runtime have the
  finite offline qualification above. Broader instruments, third-party auxiliary
  sound, product output selection and realtime remain open.
- OFX same-handle exit-time observation passed the finite Windows controls and
  official Basic sessions above. The original 100ms policy and sole Child owner
  remain unchanged. The disconnected-optional-clip gate now passes all six
  format/image checks, same-resident bit-exact RGBA half gain and unchanged
  Source, then fails that original shutdown deadline despite native Unload0.
  Five separate temporal-admission/CPU/PAR resident-and-Graph regressions pass.
  This is finite correctness evidence, not a passing lifecycle gate or an
  identified exit root cause. One existing schema2 diagnostic with the newer
  matched Product also fails: native Drop, pump join and inner return finish by
  39.01ms, while OS exit remains unobserved at the 100.47ms deadline. This narrows
  that sample to the tail after inner return; it identifies neither main/CRT/TLS
  nor the old build's failed phase. The original unobserved failure is retained.
- Audio and resident OpenFX forced termination use independent two-second exit-observation
  deadline on the original Child, replacing its unbounded post-kill wait.
  Successful kill alone does not settle ownership: a timeout retains Retiring
  and the existing Unconfirmed/staging-retention policy. A verified late exit
  settles resources without replacing the original graceful failure; the
  100ms graceful budget is unchanged. Windows Audio ordinary tests pass
  871/0 failed/189 originally ignored, including three new controls. Six native
  ownership/error/unwind controls and real Surge XT 1.3.4 CLAP/VST3 Main Runtime
  regressions pass under the original Job owner, with expected caught-panic
  stderr reviewed. Each Surge case processes 16,384 frames at 48k/stereo/B256
  Offline and confirms post-Drop exit0; graceful ACK and terminal-Off acoustics
  remain unqualified. The parent is the current opt1 Audio libtest; its normal
  `bd0e9be4` worker supplies reviewed unchanged SDK/worker paths. This does not
  qualify the current desktop parent, real kernel-I/O stalls, other platforms,
  or a strict total shutdown/RT bound: kill, OS scheduling and logging remain
  separate costs. OpenFX Host/Effect/Adapter ordinary module scopes additionally
  pass 127 tests with 69 original ignores, including three new deadline/status/I/O
  controls. Nine original-Job native gates pass with empty stderr and settled
  descendants: the new real-Child pending-kill timeout, ownership/primary-error
  controls, and official Basic processing/close. The App parent is opt0/validation
  with the same reviewed unchanged normal worker. Cleanup uses the existing
  CleanupProbe without entering graceful-exit query statistics; no new owner,
  thread, state slot or wire schema is introduced. Current normal release
  `d16bee70` additionally passes all ten App/Export/Surge consumer gates and both
  published Misc Invert Direct/Graph gates; the fresh Export parent passes all
  298 ordinary tests with 16 original ignores. The App consumer parent remains
  opt0/validation and Export opt1. The OFX integration parent is release opt3/debug0
  but includes Cargo's dev-dependency feature union (Editor test-support and
  Media/Renderer/Export validation), distinct from the default-feature worker.
  Cargo also emits its binary targets; the exact compile-time worker path was
  restored from the separately retained normal binary before both OFX tests.
  All twelve gates retain their original assertions and Job/deadline policies;
  stderr is empty and descendants settle. Graph Drop does not prove a successful
  100ms graceful result. This closes current normal-worker alignment for these
  finite workflows, not a fresh full-Workspace, GUI, physical-device, realtime,
  performance or other-platform qualification. The original F06 failures remain open.
- A safe conservative resource-overlap bound may reject a tight grant. It is
  not a minimum-peak or whole-plugin-heap measurement; physical realtime safety
  requires actual instrumentation.

## Qualification and record policy

The CLAP main/DSP and Params/State/Timer services now use the existing executor
on every native platform; only the actual GUI backend remains platform-specific.
Real Linux execution exposed a main-equals-audio identity assumption, corrected
by a scoped actual pthread/Win32 identity guard. Ordinary Audio passes on
Ubuntu24.04.5 x86_64 (848 passed, 65 ignored, `cdfafaa2`) and Windows x64
(907 passed, 207 ignored, `d47cceed`), using Rust1.97.1, default features,
opt1/debug0, zero filtering and empty stderr. Windows retains workspace-owned
TMP because sandbox `AC\Temp` refuses native rename after handle closure; the
original held-handle test, 180s deadline and Job settlement pass.

At `d47cceed`, the same real Ring Out1.0.0 Runtime/Graph gate passes on both
platforms with matching normal Products. Mono/Stereo at48k/offline/block64 use
actual native0/-6dB states, native getters and project save/reopen. Every sample
of257-frame entries at0/0/97 is compared against independent gain math under
different block partitions, without warmup omission. Legitimate restarts use
the original capture/prepare/install/Fresh path; HOLD preserves output, native
identity and the canonical author source remain stable, and original child
owners settle. This is finite offline PCM/state/lifecycle qualification, not
native GUI, realtime/device, performance, all CLAP extensions or macOS execution.

FFmpeg integration at `78d9eede` passes the ordinary Media library suite on
Ubuntu24.04.5 x86_64 (671 passed, 16 ignored) and Windows x64 (678 passed,
18 ignored), with Rust1.97.1, default features, opt1/debug0, zero filtering and
empty stderr. Windows uses an owned temporary directory and settled Job. Linux's
private patched8.1.2 SDK retains NASM and both precision changes; all six actual
ELF dependencies resolve inside that SDK. The six RGBA64 precision cases and an
explicit ignored FFV1/libx264 Proxy round trip pass. Actual ProRes422 HQ CLI
encode/decode with explicit source/output TV/BT709 gives twelve-bit black0 and
white4092, with byte-identical native/scalar decode under the original thresholds.

ALSA's transitive system-library search previously linked FFmpeg6 against SDK8
headers. SDK-first Rust search fixes selection, and one shared initialization
gate checks all six library ABIs before struct-dependent FFI. A versioned scalar
probe fault is explicitly rejected in an isolated process. Explicit renderer
device selection now constrains probes before recording fallback evidence,
including unsupported codecs; CUDA/VAAPI/D3D12/D3D11 controls pass without requiring
physical hardware. CI/runtime recipes carry the matching SDK; Linux packaging
retains its source and clears loader overrides before closure verification.
Workflow syntax passes. Normal Product/plugin execution has the finite CLAP
qualification above; new packaged artifacts, GPU, realtime/performance and
macOS physical behavior remain separately unverified.

The complete default Windows workspace baseline at `150ef2df` passes all30
members and127 executable targets:9,455 passed, zero failed,654 originally
ignored and zero filtering. All30 library doctest groups pass, including two
audio-thread compile-fail controls. Original Job/census/deadline checks settle;
the two expected negative-control stderr roles were reviewed. This supersedes
the earlier combined baseline after the independently checked scaled-image
golden correction, which retained the original pixel tolerance. Later scoped
results do not replace final-candidate CI. Ignored native gates, other feature
sets, physical GPU execution, realtime/performance and other platforms retain
their separate acceptance requirements.

A result records the tested feature, plugin/version, platform, relevant input and
render settings, suite/result, and remaining limits. A mock, SDK validator,
successful load or compilation does not establish third-party compatibility.
Preview/export, state round trips, interruption and ownership are checked where
applicable. A changed build must not be silently substituted during an active run;
correctness and release-performance results remain distinct.

Performance records use comparable hardware, plugin, input, sample rate/block
size or image format, quality and build profile. Measure warmup, processing and
host overhead, percentiles/deadlines and resource costs as needed; a crash timeout
is not an audio deadline, and a wall-clock gate duration is not DSP cost.

Current Windows release measurement (2026-10-25, compiled source `4f3ffda2`):
the original public resident Basic gate completes all four cases, excluding32
warmup frames and measuring600 frames per case. Release opt3/default features,
MSVC official Basic and unchanged80% frame-stage budgets are used. The timed
public call includes admission, identity, transport, native processing and output
materialization; the independent every-pixel HDR/negative/alpha oracle is outside
the timer. Same-Worker, shutdown and process settlement checks pass without
functional errors. **All four performance gates still fail.**

| Case | Stage budget ms | Before p50 ms | Current p50 ms | p95 ms | p99 ms | Max ms | Stage misses |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1080p30 | 26.67 | 63.05 | 35.48 | 37.34 | 38.44 | 41.59 | 600/600 |
| UHD30 | 26.67 | 233.88 | 121.18 | 125.47 | 127.64 | 163.83 | 600/600 |
| 1080p60 | 13.33 | 67.37 | 35.17 | 37.94 | 43.41 | 45.93 | 600/600 |
| UHD60 | 13.33 | 233.45 | 121.17 | 124.36 | 126.28 | 161.39 | 600/600 |

Before p50 uses `3fd7463c` with the same hardware, MSVC Basic, release/default
features and sampling conditions. The observed combined median reduction is
about44-48%; the latest inline slice reduces medians about16-24% relative to the
preceding `70aaf7cd` normal build. These are whole-call observations, not pure DSP
or a mature-host comparison. Historical r30 used MinGW/validation features and
is not a matched baseline. Whole-test600s and owner cleanup2s do not change the
frame budgets or original shared100ms plugin shutdown. The earlier UHD60
1248.47ms maximum is not reproduced or explained; no long-run latency bound,
realtime, full-NLE, GPU or other-platform performance qualification is granted.

Two measured causes were addressed without removing admission. The native MSVC
float classifier called `_fdtest` per component; `70aaf7cd` replaces it with an
IEEE binary32 representation check. Its unchanged-bit/standard-predicate
FramePixels control previously passed75,776 cases; that C++ source is unchanged
in this slice. Parent output admission exposed a second issue: final machine
code of the ordinary materializer called the shared Rust `validate_sample` once
per pixel with a result buffer, while diagnostic call-site optimization differed.
Actual UHD owner/process checkpoints average about0.17ms for128 calls, insufficient
to explain that gap. `4f3ffda2` adds only an inline hint and rationale to the
existing predicate, making its body available to generic cross-crate consumers.
The separate named predicate and direct calls disappear from the fresh normal
parent, and paired ordinary/instrumented timing converges. All numeric/alpha
rules, checkpoint positions, errors, allocations, transport and ownership remain
intact. Both typed APIs scan once. Native output validation precedes
EndSequenceRender; parent admission remains the independent publication boundary,
not a redundant scan to remove. No identity cache or GPU-interoperability claim
is introduced.

Current verification includes361 Effects release library tests with0 failed and
0 ignored, the materializer cancellation/frontier control, and six exact native
gates on the freshly built default release Product and original integration
parent: authored Basic pixels, repeated resident isolation, partial-block/raster
transfer, Direct/Graph alpha negotiation, invalid/nonfinite refusal with
retirement, and HDR disk-cache/nested/opaque/stock-OCIO output. All six have exit0,
empty stderr and original180s/2s ownership settlement; shared100ms shutdown remains.
The ignored integration arena adds no ordinary-test coverage. No new workspace-wide
or strict-Clippy qualification is claimed. Finite behavior and measured median
improvement are verified; deadlines, the earlier tail, broad third-party/GUI/RT
and other-platform qualification remain open. Further optimization requires a new
measured cause, not repetition of the unchanged failed matrix or a budget increase.

Keep concise current conclusions, useful reproduction inputs and actionable open
issues. Do not accumulate historical executable copies, source-hash inventories,
raw receipts or repeated audit packets. Rerun the complete affected suite when
needed. Local next steps and candidate ownership are in the small ignored
`.scratch/plugin-host-sprint/processor-output-routing-checkpoint.json`; Git and
the feature matrices retain the durable implementation/contract record.
