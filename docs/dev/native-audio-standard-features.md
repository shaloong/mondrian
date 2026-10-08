# Native audio standard features

Standards and source reviewed on 2026-10-08. This document separates the
negotiated product implementation from vendor APIs, unavailable services and
remaining work. Qualification is tied to exact source and binary receipts in
[Native plugin compatibility and qualification](native-plugin-compatibility.md).
The [audio architecture](../architecture/audio-pipeline.md) describes the
canonical execution and authoring boundaries.

## Standards and interpretation

- Official VST SDK release history lists **3.8.1, published 2026-08-11**.
  [Release history](https://steinbergmedia.github.io/vst3_dev_portal/pages/Versions/Index.html),
  [3.8.1 changes](https://steinbergmedia.github.io/vst3_dev_portal/pages/Versions/Version%2B3.8.1.html).
- Official CLAP's latest numeric public tag checked on 2026-10-09 is **1.2.10**,
  resolved to commit `195b42a004144fab0b3cf95e9c067187d15365b7`. Its immutable version
  header declares 1.2.10. That commit's committer timestamp is
  **2026-07-13 09:51:20 UTC**; this is not an asserted release publication date.
  The official source is MIT licensed. Stable interfaces are included by
  `clap.h`; draft interfaces remain separately included by `all.h`.
  [Tag](https://github.com/free-audio/clap/tree/1.2.10),
  [pinned version](https://github.com/free-audio/clap/blob/195b42a004144fab0b3cf95e9c067187d15365b7/include/clap/version.h),
  [pinned commit](https://github.com/free-audio/clap/commit/195b42a004144fab0b3cf95e9c067187d15365b7),
  [MIT license](https://github.com/free-audio/clap/blob/195b42a004144fab0b3cf95e9c067187d15365b7/LICENSE),
  [stable inventory](https://github.com/free-audio/clap/blob/195b42a004144fab0b3cf95e9c067187d15365b7/include/clap/clap.h),
  [draft inventory](https://github.com/free-audio/clap/blob/195b42a004144fab0b3cf95e9c067187d15365b7/include/clap/all.h).
  The independently checked main snapshot
  `a47f6badb49d948fd009998f28309cdab78979c9` also declares 1.2.10;
  a main-branch version alone is not evidence of a release tag.
- VST3 runtime, vendored host and both owned VST3 fixtures resolve one canonical
  `vendor/vst3-sdk` package generated from verified official 3.8.1 inputs.
  Original generator exclusions remain explicit in the
  [SDK provenance](../../vendor/vst3-sdk/PROVENANCE.json).
  Independent C++/Rust layout/GUID/signature and all-nine-method COM call-through
  gates exercise the selected new interfaces. **Bindings are available; host
  transport/MIDI2/articulation features are not thereby implemented.**
- CLAP currently uses Clack `27ca28391b57a61bad4bb5099288a7bf9247350d` and
  clap-sys `25d7f53fdb6363ad63fbd80049cb7a42a97ac156` (1.2.2). CLAP 1.x ABI
  compatibility does not create 1.2.10 host services. Enabled host extension
  features are audio-ports, latency, tail, state, params, render and thread-check;
  the worker actually declares only the host thread-check extension.

Status: **P** actual product path within stated limits; **R** explicit admission
refusal outside that subset; **V** vendor implementation without a product
consumer; **N** absent; **G** accepted/callback path has an unresolved behavior
gap. **Q** always describes independent qualification, never universal vendor
compatibility. **Perf U** means current source has no completed native release
deadline acceptance receipt in the reviewed evidence. Missing optional interfaces
can be negotiated unavailable; an implemented core callback cannot become correct
merely by calling it optional.

Mandatory/optional roles for all 85 official VST interface entries are retained
in the [VST3 interface inventory](vst3-interface-inventory.md). An interface marked mandatory for a format
wrapper or validator is conditional on acting as that role; Mondrian is a native
desktop host. CLAP stable extensions are negotiated optional families. Their
method/thread/lifetime obligations become binding when exposed or consumed.

## Source owners

| Key | Actual source / owner |
| --- | --- |
| CW | `crates/mondrian-audio/src/processor_isolation/{clap_worker,clap_parameter_events}.rs`: host, lifecycle, typed SDK parameter-event storage, input conversion and native processing |
| CD | `crates/mondrian-audio/src/processor_isolation/clap_discovery.rs`: identity/state/mode/ports/parameters probe |
| VW | `crates/mondrian-audio/src/processor_isolation/vst3_worker.rs`: native prepare/reset/process/buses |
| VD | `crates/mondrian-audio/src/processor_isolation/vst3_discovery.rs`: class/state/mode/contract probe |
| IP | `crates/mondrian-audio/src/processor_isolation/{mod,protocol,worker,supervisor}.rs`: immutable payload, mapping, bounded process transport |
| PH | `crates/mondrian-audio/src/{processor,processor_host,processor_parameters,schedule,latency,render,runtime,delivery}.rs`: exact occurrences, canonical curves, prepared PDC and runtime admission |
| VL | `vendor/vst3-host/src/internal/plugin_impl.rs`: native component/controller, setup, queues and vendor control methods |
| VC | `vendor/vst3-host/src/internal/com_implementations.rs`: host interfaces, messages, streams, parameter/event lists and callbacks |
| VE | `vendor/vst3-host/src/midi.rs` and `src/internal/data_exchange.rs`: owned rich events and exchange queues |
| SDK | `vendor/vst3-sdk/{src,com-scrape-types,PROVENANCE.json,GENERATION-SOURCES.json}`: one generated ABI/runtime |
| UI | `crates/mondrian-app/src/app_ui/audio_processor_rack.rs` and `crates/mondrian-app/src/app/audio_rendering.rs`: product controls and sole delivery adapter |
| PERF | `crates/mondrian-audio/src/processor_isolation/tests/native_performance.rs`, `scripts/validation/collect-native-perf-context.py`: release-only isolated-worker acceptance harness and artifact receipt |

## VST3 workflow matrix

Each row has **Perf U** at this audited source. Current worker source contains
no native-editor event loop or MIDI/event IPC contract; vendor methods alone are
not product feature support.

| Feature / primary specification | Actual status and execution boundary | Qualification / next closure |
| --- | --- | --- |
| [Factory/component/controller lifecycle](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstcomponent.h) | P: fingerprinted selected class, initialize/connect/state/setup/activate/start/process/stop/deactivate/disconnect/terminate; VD/VW/VL/VC | Owned native state/sidechain fixtures. Vendor/platform/reference Hostchecker matrix remains. |
| [One main and auxiliary buses](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstaudioprocessor.h) | P/R: one mono/stereo main in/out, up to 31 same-layout auxiliary inputs, actual native activation and ordered bus keys; VW `audio_inputs_from_metadata`, VL `BusAudioBuffers` | Distinct weighted auxiliary PCM, state-controlled ports, reset, partitions and shared PDC/history gates. |
| [Mixed/surround/ambisonic layouts](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/vstspeaker.h) | R: broader valid arrangements are refused by VW; canonical channel-layout/routing/device closure absent | Add actual typed layout model and multichannel PCM oracles before relaxing admission. |
| [Generators/instruments/multiple outputs](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstcomponent.h) | R/N: zero-input, multiple output and event-only topologies do not fit current product strip contract | Needs author event ports and output routing, not fabricated silent main input. |
| [Sample32/sample64](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstaudioprocessor.h) | P/V: product PCM is Float32; VL can negotiate native Float64 and convert. No independent current Float64-only native receipt found | Float64-only official/vendor fixture, precision/conversion/allocation/deadline matrix. |
| [Realtime/offline setup](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstaudioprocessor.h) | P: mode selected before actual setup/activation; probe and worker verify the same mode-specific latency/tail; VW/VD | Both modes covered for owned references. Prefetch mode has vendor primitives but no canonical product policy. |
| [Latency/tail and causal PDC](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstaudioprocessor.h) | P/R: activated immutable facts, stale mismatch refusal, PH sole compensation owner | Dynamic latency/restart canonical reprepare not implemented; current fixture contracts are stable. |
| [restartComponent](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsteditcontroller.h) | G/V: VC atomically records flags; VL can service requests; VW does not consume them into prepared plan/generation/PDC | Typed deferred outcomes and a complete non-RT candidate installation remain unimplemented. Blind vendor reactivation leaves canonical contract stale. |
| [Sample-offset automation](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstparameterchanges.h) | P: exact author curves → bounded lanes → normalized sample-offset native points; stepped values use admitted indices; PH/VW/VC | Owned stateful queue and common author Undo/save/reopen output. Saturation and actual allocator/deadline gate still needed. |
| [Plugin-origin begin/perform/end edits](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsteditcontroller.h) | G/V: vendor records edits; product does not consume them into author sessions/history | One ordered typed gesture stream plus value validation and exactly one author transaction. |
| [Group edit/dirty/editor-open notifications](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsteditcontroller.h) | G/V: VC IComponentHandler2 queues notifications separately from edit values, losing cross-stream ordering; UI consumer absent | Preserve cross-kind order before group Undo/state capture/UI activation. |
| [Output parameter/read-only meters](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstparameterchanges.h) | G/V: native output changes exist in vendor; product does not drain them to display/recording | Bounded typed output observations; runtime feedback is not automatically an author edit. |
| [Display/text/plain conversion](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsteditcontroller.h) | V/N: vendor formatting methods exist; product controls use generic values, not full native conversion/round-trip | Native units/enum/name precision, locale-independent stored values, conversion round-trip oracle. |
| [Host editing/automation state/function names/remap](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsteditcontroller.h) | V/N: some vendor methods exist; product has no complete host-edit brackets, recording modes, remap or named randomize action | Respect canonical parameter identity and gesture domain; do not infer functions from translated names. |
| [State IBStream](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstcomponent.h) | P/R: component/controller opaque state saved/restored; product cap 128 KiB; larger conforming states explicitly refused; VD/VW/VC | State is configuration, not a generic DSP checkpoint. Bounded large-state/resource transport and editor-origin changes pending. |
| [State stream context/size](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivststreamattributes.h) | V/N: IStreamAttributes primitives exist; general preset/project stream context and ISizeableStream not product-complete | Correct context/name/size negotiation and resource lifecycle. |
| [Units/programs/preset data](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstunits.h) | V/N: vendor unit/program metadata, program-change table and opaque unit/program data; no product hierarchy/browser/transactions | Program gestures, native notifications, project assets and save/reopen integration. |
| [Channel context](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstchannelcontextinfo.h) | N: native controller gets no canonical track/Scope name/color/context | Exact current occurrence context and stale-generation rules. |
| [Absolute ProcessContext](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstprocesscontext.h) | P: signed latency-adjusted signal samples + exact sample rate supplied per block, optional validity = available ∩ requested; VW typed setter/VL | Actual time-dependent PCM includes negativePDC/nonzero seek/rates/partition oracles. No invented 120 BPM or 4/4. |
| [Musical/playing/recording/cycle/system context](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstprocesscontext.h) | N with truthful unavailable flags: product canonical seam currently provides none of these optional facts | Author tempo/signature map and actual transport/device clocks before setting validity. RT mode is not playing state. |
| [ITransportControl3.8.1](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsttransportcontrol.h) | N host: generated ABI and independent COM calls exist, product HostApplication does not implement it | UI-thread typed actual playback actions with state/refusal and position semantics. Unsupported recording stays unavailable. |
| [Rich event IEventList](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstevents.h) | V/N: typed vendor note/expression/SysEx/chord/scale payloads; product IPC carries parameter values only. Vendor event-list mutex/payload copies can allocate in process | One author/execution event model, fixed admission, legal direction/time/bus validation and owned pointer payloads. |
| [MIDI1 mapping/learn](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstmidicontrollers.h) | V/N: vendor caches/methods; product has no MIDI routing/device/learning author workflow | MIDI CC mapping and assignment-change invalidation, legal event direction, no lossy seven-bit canonical representation. |
| [MIDI2 mapping/learn3.8.1 ABI](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstmidimapping2.h) | N feature: byte1/byte2 verified generated layout, real mapping/learning COM slot tests; vendor does not query these plugin interfaces | n-to-m assignment semantics, UMP precision/range, mapping invalidation and product recording. |
| [Note expression/keyswitch/articulation](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstnoteexpression.h) | V/N: vendor expression metadata exists; keyswitch queries and the product author event route are absent. New articulation IInfo ABI tested, host consumption absent | Per-note identity/lifetime, expression events, articulation metadata and 3.8.1 type IDs. |
| [Output events/SysEx](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstevents.h) | G/V: native addEvent has explicit caps/refusal; vendor subsequent output queue drops oldest on overflow. Product does not drain/route output events | Exact bounded output ownership and overflow policy; no silent accepted event loss. |
| [IPlugView/IPlugFrame editor](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/gui/iplugview.h) | V/N product: attach/remove/resize primitives exist; product isolated child has no editor window/control loop | HWND/Cocoa/X11/Wayland owner lifetime, resize constraints/DPI/key/focus/capture/modal interaction and teardown. |
| [Content scale/parameter finder/context menu](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/gui/iplugviewcontentscalesupport.h) | V/N: vendor scale/menu registry methods; product GUI bridge absent; parameter finder absent | Real monitor scale and popup selection, exact native targets, control transactions. |
| [Linux runloop/Wayland](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/gui/iplugview.h) | Conditional V/N: vendor Linux timer/FD primitives not pumped by product protocol loop; Wayland host/frame absent | Native supported-platform editor event loop and independent platform acceptance. |
| [Data exchange](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstdataexchange.h) | G/V: aligned bounded queues and background delivery exist; worker does not pump requested main-thread receiver delivery while processing | Correct requested thread delivery/queue lifetime, current product consumer and allocation/deadline evidence. |
| [Progress/context menus/unit notifications](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivsteditcontroller.h) | G/V: exposed vendor callbacks queue requests; App consumes none of these as complete workflows | Truthful capability exposure and typed product control outcomes. |
| [IPlugInterfaceSupport](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstpluginterfacesupport.h) | G: VC advertises vendor-known plugin interfaces including editor/prefetch/expression whose product workflows are incomplete | Advertise only the product's admitted consumer capabilities; vendor-wide static list is insufficient. |
| [Bus activation/presentation latency/system time callbacks](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstaudioprocessor.h) | N: optional IComponentHandlerBusActivation/SystemTime and IAudioPresentationLatency callbacks absent | Canonical bus requests and true physical latency/clock facts, distinct from algorithmic PDC. |
| [Silence/tails/zero-sample flush](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/vst/ivstaudioprocessor.h) | P/N: normal nonzero PCM/tail schedule; no product idle zero-sample event/parameter flush or silence suspension policy | Must retain ringing tails/generator/event behavior; VST has no CLAP sleep status. |
| [Clipboard VST-XML3.8.1](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical%2BDocumentation/Clipboard%2BVST-XML/Index.html) | N: not equivalent to IXmlRepresentationController; no product native audio/music exchange route | Parse/validate exchange metadata and exact time through existing import/author transactions. |
| [Latest standard scan roots](https://steinbergmedia.github.io/vst3_dev_portal/pages/Versions/Version%2B3.8.1.html) | V/N: selected-file product discovery; vendor Linux roots still omit new `/usr/lib64/vst3` and `/usr/local/lib64/vst3` | Honest standard-location catalog scan, dedup/revision handling and selection UX. |
| [Compatibility/class migration](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/4f547e8e102b47de4a8b8aaf343c73b700786372/base/iplugincompatibility.h) | V/N: vendor metadata parse exists; author exact class/schema/binary identity preserved, mismatch explicitly refused | Explicit user binding/migration policy, not alpha compatibility aliases. |
| [Wrappers/iOS/SDK test interfaces](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/annotated.html) | Conditional not ordinary desktop host obligations; generator test/helper exclusions documented | Do not expose AAX/AU/VST2 wrapper identity or claim generated validator interfaces that original generator excludes. |

## CLAP core and all 27 stable extension families

All implemented rows have **Perf U**. For N rows the extension is unavailable,
unless the row explicitly identifies an accepted core/output behavior gap.

| Core/family / official primary header | Actual status and source | Remaining execution/product contract |
| --- | --- | --- |
| [entry/plugin factory](https://github.com/free-audio/clap/blob/main/include/clap/factory/plugin-factory.h) | P: selected descriptor, fingerprinted discovery child and worker; CD/CW/IP | Standard locations/vendor corpus/platform validation. |
| [plugin lifecycle](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) | P bounded: SDK handles init failure destruction; inactive state/mode/ports then activate; audio-role start/reset/process/stop; deactivate/destroy; CW/CD | Additional editor/timer/pool workflows need matching roles/lifetimes. |
| [request_callback](https://github.com/free-audio/clap/blob/main/include/clap/host.h) | P bounded: atomic pending, serial main callback service capped at 16; CW | Delayed idle main-loop servicing for future GUI, not unbounded recursion. |
| [request_restart](https://github.com/free-audio/clap/blob/main/include/clap/host.h) | G: core callback records atomic; current CW service reports error/fails processing, no recoverable prepared invalidation | Preserve admitted triggering root window, next-root zero-DSP typed defer, non-RT observed contracts and canonical reprepare/install. |
| [request_process / process status](https://github.com/free-audio/clap/blob/main/include/clap/process.h) | G: request_process is a no-op; process status return not used for scheduler sleep/wake; CW | Wake inactive/sleeping process when requested, bounded owner scheduling, tails/events must not disappear. |
| [Transport seconds](https://github.com/free-audio/clap/blob/main/include/clap/events.h) | P: signed canonical latency-adjusted sample/rate→checked 2^31 seconds, HAS_SECONDS only; CW `clap_transport`; steady counter explicitly −1 | Canonical tempo/beat/signature/loop/playing/recording and true independent steady facts remain unavailable. |
| [Standard input/output events](https://github.com/free-audio/clap/blob/main/include/clap/events.h) | P input scalar parameters; G output: SDK void output returns accepted while discarding events; CW/IP | Typed heterogeneous input/output event ownership, direction/bus/time validation, overflow and UI/recording semantics. |
| [audio-ports](https://github.com/free-audio/clap/blob/main/include/clap/ext/audio-ports.h) | P/R: mono/stereo main and up to 31 same-layout aux, stable native IDs/order/state probe, Float32; CD/CW | Unspecified port type, mixed layouts, other flags/topologies refused; Float64 native and all valid layouts not qualified. |
| [audio-ports-activation](https://github.com/free-audio/clap/blob/main/include/clap/ext/audio-ports-activation.h) | N: not queried/declared | Author-selected active ports, legal while-processing feature and exact runtime topology. |
| [audio-ports-config](https://github.com/free-audio/clap/blob/main/include/clap/ext/audio-ports-config.h) | N | Config enumeration/selection before activation, user choice and non-RT contract rebuild. |
| [configurable-audio-ports](https://github.com/free-audio/clap/blob/main/include/clap/ext/configurable-audio-ports.h) | N | Explicit requested port layouts and actual native acceptance, not channel-count guesses. |
| [ambisonic](https://github.com/free-audio/clap/blob/main/include/clap/ext/ambisonic.h) | N/R current layouts | Canonical order/normalization metadata, channel map and output/device matrix. |
| [surround](https://github.com/free-audio/clap/blob/main/include/clap/ext/surround.h) | N/R current layouts | Exact channel roles/masks and multichannel graph routing. |
| [params](https://github.com/free-audio/clap/blob/main/include/clap/ext/params.h) | P plugin metadata/plain sample values/stepped/hidden and bounded SDK ParamValueEvent/ordinal storage; N host rescan/clear/request_flush; CD/CW | Actual owner-capacity growth and warmed full-capacity/reset processing allocation/order/PCM gates pass for the owned fixture. Native value/text/module/flag semantics, UI-origin gestures, output values, modulation, native flush and full resource/deadline acceptance remain. |
| [state](https://github.com/free-audio/clap/blob/main/include/clap/ext/state.h) | P/R bounded restore/save ≤64 KiB, captured factory state, canonical history; CD/CW | Larger resource state and native-editor-origin dirty changes; save/load is not mandatory DSP checkpoint. |
| [state-context](https://github.com/free-audio/clap/blob/main/include/clap/ext/state-context.h) | N | Purpose-specific project/preset/duplicate context and matching resource lifetime. |
| [render](https://github.com/free-audio/clap/blob/main/include/clap/ext/render.h) | P/R: RT/OFF selected inactive after state restore and before metadata/activation; hard-realtime/declined mode refused; CD/CW | Owned distinct quality/gain/latency modes and mode refusal are named references; arbitrary vendor quality/performance unqualified. |
| [latency](https://github.com/free-audio/clap/blob/main/include/clap/ext/latency.h) | P plugin latency queried activated on main role; N host changed callback; CD/CW | Dynamic restart/reconfigure at legal inactive boundary with new canonical PDC, not active mutation. |
| [tail](https://github.com/free-audio/clap/blob/main/include/clap/ext/tail.h) | P plugin finite/infinite tail queried audio role, exact admission; N host changed callback | Dynamic tail observation/schedule update, sleep/wake and export termination policy. |
| [thread-check](https://github.com/free-audio/clap/blob/main/include/clap/ext/thread-check.h) | P host declaration: fixed main OS identity plus instance-owned AtomicBool symbolic audio RAII !Send/!Sync; CW | Current worker/discovery lifecycle tested including errors/unwind/background false; future thread-pool/UI paths need own guards. |
| [gui](https://github.com/free-audio/clap/blob/main/include/clap/ext/gui.h) | N, optional host unavailable | Isolated native create/show/hide/destroy/resize/DPI/focus/keyboard/window ownership and real platform tests. |
| [timer-support](https://github.com/free-audio/clap/blob/main/include/clap/ext/timer-support.h) | N | Bounded timer registration/cancellation/owner main-loop delivery, no DSP lock. |
| [posix-fd-support](https://github.com/free-audio/clap/blob/main/include/clap/ext/posix-fd-support.h) | Conditional N on POSIX | FD readiness/lifetime/event-loop integration, unregister/teardown races. |
| [note-ports](https://github.com/free-audio/clap/blob/main/include/clap/ext/note-ports.h) | N | Declared note/MIDI dialects, author input/output port identity/routing and restart metadata. |
| [note-name](https://github.com/free-audio/clap/blob/main/include/clap/ext/note-name.h) | N | Note/bus/channel name metadata and changed notifications. |
| [voice-info](https://github.com/free-audio/clap/blob/main/include/clap/ext/voice-info.h) | N | Voice limits/overlap/polyphonic lifetime and changed observations. |
| [event-registry](https://github.com/free-audio/clap/blob/main/include/clap/ext/event-registry.h) | N | Custom event-space IDs and typed bounded ownership; unknown bytes not presumed safe. |
| [log](https://github.com/free-audio/clap/blob/main/include/clap/ext/log.h) | N host extension | Bounded severity/thread-safe diagnostics, quiet product status and no per-event log flood. |
| [thread-pool](https://github.com/free-audio/clap/blob/main/include/clap/ext/thread-pool.h) | N host extension | Domain-owned bounded tasks, proper symbolic audio roles, deadline/cancellation/instance teardown. |
| [track-info](https://github.com/free-audio/clap/blob/main/include/clap/ext/track-info.h) | N | Actual current track/Scope name/color/layout with exact occurrence and generation. |
| [context-menu](https://github.com/free-audio/clap/blob/main/include/clap/ext/context-menu.h) | N | Typed native/product targets, nesting/disabled flags, popup selection and lifetime. |
| [param-indication](https://github.com/free-audio/clap/blob/main/include/clap/ext/param-indication.h) | N | Actual automation/mapping state indication without fictional recording controls. |
| [remote-controls](https://github.com/free-audio/clap/blob/main/include/clap/ext/remote-controls.h) | N | Pages/parameters/control focus, changed/rescan and canonical parameter mapping. |
| [preset-load](https://github.com/free-audio/clap/blob/main/include/clap/ext/preset-load.h) | N | Preset location/key, loaded/error callbacks and project resource/history transactions. |
| [preset-discovery factory](https://github.com/free-audio/clap/blob/main/include/clap/factory/preset-discovery.h) | N optional factory | Provider lifecycle, file types/locations/metadata/UID matching and bounded index scan. |
| [universal-plugin-id utility](https://github.com/free-audio/clap/blob/main/include/clap/universal-plugin-id.h) | N integration; not an extra `ext/` service | Explicit cross-format/preset matching; no automatic authored definition replacement. |

## CLAP drafts (all unavailable in the actual product)

These are **draft** negotiation families; stable ABI version compatibility does
not establish draft feature compatibility. Each retains its own primary header:

| Family / official primary header | Status / product contract still needed |
| --- | --- |
| [plugin-invalidation factory](https://github.com/free-audio/clap/blob/main/include/clap/factory/draft/plugin-invalidation.h) | N: catalog rescan and exact binary/definition invalidation |
| [plugin-state-converter factory](https://github.com/free-audio/clap/blob/main/include/clap/factory/draft/plugin-state-converter.h) | N: explicit typed conversion input/output and author transaction |
| [background-activation](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/background-activation.h) | N: actual non-RT worker/thread/lifetime negotiation |
| [background-progress](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/background-progress.h) | N: owned progress/cancel and product outcome |
| [background-state-context](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/background-state-context.h) | N: state purpose/background lifetime |
| [extensible-audio-ports](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/extensible-audio-ports.h) | N: exact dynamic topology and canonical invalidation |
| [flush-events](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/flush-events.h) | N: typed no-PCM event execution contract |
| [gain-adjustment-metering](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/gain-adjustment-metering.h) | N: typed runtime-only meter observations |
| [mini-curve-display](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/mini-curve-display.h) | N: bounded curve representation/UI lifetime |
| [octave-number](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/octave-number.h) | N: actual musical presentation convention |
| [params-origin](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/params-origin.h) | N: parameter origin and truthful author/runtime separation |
| [param-hovered](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/param-hovered.h) | N: actual native UI interaction identity |
| [project-location](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/project-location.h) | N: bounded project capability/path semantics |
| [resource-directory](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/resource-directory.h) | N: native resources, copy/package publication and lifetime |
| [scratch-memory](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/scratch-memory.h) | N: owner-accounted grants and requested memory lifetime |
| [transport-control](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/transport-control.h) | N: actual canonical UI transport actions/refusal |
| [triggers](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/triggers.h) | N: exact trigger event semantics/author action |
| [tuning](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/tuning.h) | N: canonical musical tuning and per-note lifetime |
| [undo](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/undo.h) | N: existing author-session transactions/history, no second Undo owner |
| [webview](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/webview.h) | N: isolated editor/web resource and platform lifecycle |

## Qualification and performance interpretation

The owned suites exercise real native workers, main and auxiliary PCM,
parameters/state, render mode and refusal, thread lifecycle, reset/seek/uneven
partitions, canonical signed transport, shared native PDC, author Undo and
saved/fresh project output. Independent C++ layout/signature and actual COM calls
exercise the selected latest SDK interfaces. These are separate correctness and
ABI scopes; test coverage does not establish universal plugin compatibility.

[Native plugin compatibility and qualification](native-plugin-compatibility.md)
records completed receipts and remaining qualification. A pass certifies its
exact source, product, fixture and environment hashes; an earlier pass cannot
certify a changed source/binary pair. Current native release deadline performance
is **unqualified**. Test-profile correctness evidence is separate from release
performance evidence.

The release harness measures the **public process call including parent IPC,
child native DSP and PCM copy**. Any native request preflight IPC at the root
window is part of that timed production path; its round trip must not be exempted
or replaced with stale cached flags. Exclude setup/discovery/reset/warmup, PCM
oracle/input generation and report I/O. It checks independent weighted PCM,
fixed/uneven buffers of 64/128/256/512/1024 frames, p50/p95/p99/max and the exact
realtime deadline `elapsed_ns × sample_rate ≤ frames × 1e9`. Offline throughput
has no invented realtime pass threshold. For 64 frames at 48 kHz the derived
budget is approximately 1.333 ms; the 100 ms crash deadline is not an audio
performance budget. The harness does not itself establish a pass. Device
underrun, simultaneous chains, automation density, foreign allocations, GUI and
independent vendor workloads need their own declared qualification.

Host-owned preparation payload/state/descriptors/Vec capacities, bus buffers and
old/candidate coexistence must be accurately admitted, with shared Arc
allocations counted once. Current native probes report zero session scratch
bytes while omitting known owners; bounded shared mappings alone do not describe
the full memory footprint. Unknown plugin heap/RSS requires separate observation.
Typed CLAP scalar parameter-event storage has actual allocator qualification;
recoverable deferred native requests and the complete non-RT reprepare service
remain implementation work. This local storage result does not close the full
native resource contract or establish a realtime deadline pass.

## Implementation priorities

1. Account all known host owners and measure complete release callbacks
   against the derived audio deadline. Preserve the qualified typed scalar-event
   carrier as heterogeneous event admission is added.
2. Consume legal VST restart and CLAP restart requests through typed deferred
   outcomes. Preserve the complete already-admitted processing window; defer
   the next window before nested/sibling DSP or destination mutation.
3. Install non-RT reprepare candidates for the exact occurrence, prepared plan,
   session and continuity epoch. Recompute sole canonical PDC and resource
   grants; integrate media generations and Clock Master handoff. Validate the
   final native activated profile. Controller values alone do not prove
   component configuration, and opaque `getState` is configuration rather than
   a general DSP checkpoint.
4. Make capability advertisements match actual product consumption. Add legal
   typed input/output event ownership and one ordered plugin-origin gesture
   stream through canonical author transactions. Runtime observations do not
   automatically become Undo entries.
5. Complete native editor/parameter display/preset/resource workflows, richer
   audio topology, MIDI 1/2/notes/expression/articulation, musical transport and
   optional control/exchange services. Each requires explicit platform/vendor
   oracles and resource/deadline evidence. Stable and draft families retain
   distinct negotiation and qualification.


### Typed CLAP carrier correctness qualification (2026-10-09)

The actual source-bound test-profile gates passed **20 CLAP worker tests**
(four installed-plugin tests remained ignored in that filter) and **one typed
storage test**. The preparation allocator oracle first failed with a measured
growth of 13,547,520 bytes between 64 and 4096 maximum frames, versus the
independent typed PCM/event/ordinal expectation of 580,608 bytes. With typed
SDK ParamValueEvent owners and fixed insertion ordinals, that same oracle passed
with the expected 580,608-byte growth. The warmed full-capacity two-parameter
control retained equal-time insertion order and independently checked PCM
through reset, positive/negative starts and both render modes with zero Rust
allocations/reallocations in the measured process/reset calls.

These are allocator-request and correctness results for the owned fixture.
They do not measure arbitrary plugin heaps, full host-owned peak resources,
native IPC deadline cost or release performance. The full worker log SHA-256 is
`f275902bbb66fc740ea4ce5caace6c29f5a7e5a67c9207aee630c04a62ed4376`;
the typed storage log SHA-256 is
`70bf26830f52e95a3cfcb528ce83e394067a34f2e0da2a16e1a25b888d6d0606`.
