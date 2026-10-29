# VST3 3.8.1 interface inventory

Standards and source reviewed on 2026-10-08. This inventory retains all 85
distinct interface-like entries from the official class index; `IPtr` is a smart
pointer template. Each row links its primary reference and separates the stated
normative role from the actual product execution boundary.

Declaration-gated MIDI/unit/program/expression rows were updated on 2026-10-15;
their finite qualification is recorded in the linked compatibility report.

**P** product path within the stated boundary; **V** vendor API without a complete
product consumer; **N** absent; **C** conditional role/platform/tooling. **M/O**
retain the official class page's mandatory/optional labels. Requirements for
wrappers, validators or iOS services apply when acting as those roles. A class
page without an explicit role/level retains unspecified status.

The [SDK provenance](../../vendor/vst3-sdk/PROVENANCE.json) records verified SDK
3.8.1 inputs, generated bindings and original generator exclusions. Independent
layout/GUID/signature and actual COM calls qualify the selected new ABI slice.
They do not implement host transport, MIDI 2 or articulation workflows. Current
native processing does supply signed absolute signal samples/rate and truthful
optional validity. Dynamic canonical restart/reprepare, native editors, events,
presets and other optional services remain incomplete.

The [audio standard feature matrix](native-audio-standard-features.md) covers
workflows, all 27 stable CLAP extension families and 20 draft extension/factory
families. [Native plugin compatibility and qualification](native-plugin-compatibility.md)
is authoritative for completed source/binary receipts and remaining platform,
vendor and performance qualification. Native release callback deadlines remain
unqualified. The [audio architecture](../architecture/audio-pipeline.md) describes
canonical authoring, occurrence and execution ownership.

## Source abbreviations

| Key | Repository path / audited seam |
| --- | --- |
| W | `crates/mondrian-audio/src/processor_isolation/vst3_worker.rs` (prepare, process, validate_loaded, audio_inputs_from_metadata) |
| WD | `crates/mondrian-audio/src/processor_isolation/vst3_discovery.rs` |
| WP | `crates/mondrian-audio/src/processor_isolation/protocol.rs` |
| PH | `crates/mondrian-audio/src/processor_host.rs` (canonical latency-adjusted signal requests) |
| U | `crates/mondrian-app/src/app_ui/audio_processor_rack.rs` (product audio rack) |
| VLOAD / VGUI | `vendor/vst3-host/src/internal/plugin_impl.rs` (load/setup/process/control APIs and native views) |
| VCOM | `vendor/vst3-host/src/internal/com_implementations.rs` (host interfaces/streams/queues/frame) |
| VDATA | `vendor/vst3-host/src/internal/data_exchange.rs` |
| VEVT | `vendor/vst3-host/src/midi.rs` |
| VDISC | `vendor/vst3-host/src/discovery.rs` (factory metadata and platform search roots) |
| VMODULE | `vendor/vst3-host/src/internal/module_info.rs` |
| B | `vendor/vst3-sdk`: generated from verified official 3.8.1 headers using upstream b2a45f2d40e602eff5a224b41613735388868590 generator/runtime; original exclusions in [SDK provenance](../../vendor/vst3-sdk/PROVENANCE.json) |

## Exact interface inventory

### Core ABI, discovery, state and setup

| Interface (official reference) | Normative role / level | Actual status | Source seam and boundary |
| --- | --- | --- | --- |
| [FUnknown](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUnknown.html) | role unspecified; level unspecified | P | VCOM / VLOAD: COM reference counting, queryInterface and owned pointer lifetimes underpin every loaded component; arbitrary vendor lifetime conformance still requires independent ABI tests. |
| [IPluginBase](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginBase.html) | plug imp; level unspecified | P | VLOAD: Component/controller initialize and terminate on worker control thread; teardown occurs before unloading the module. |
| [IPluginFactory](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory.html) | plug imp; level unspecified | P | VDISC / VLOAD: Factory class enumeration and class instantiation support binary-fingerprinted admission. |
| [IPluginFactory2](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory2.html) | plug imp; level unspecified | P | VDISC: Extended class metadata is read when available; absence remains a valid fallback. |
| [IPluginFactory3](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory3.html) | plug imp; level unspecified | P | VDISC / VLOAD: Factory host context is set when exposed; SDK factory Unicode metadata is consumed. |
| [IComponent](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IComponent.html) | plug imp; M; 3.0.0 | P bounded | W / VLOAD: State, controller association, bus enumeration/activation are used. Product admits one mono/stereo main input/output and same-layout auxiliary inputs; not all valid component topologies. |
| [IAudioProcessor](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAudioProcessor.html) | plug imp; M; 3.0.0 | P bounded | W / VLOAD: Realtime/offline setup, activation, sample32 or sample64 conversion, processing, latency/tail probing are used. Dynamic changes are not integrated into the canonical graph. |
| [IEditController](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IEditController.html) | plug imp; M; 3.0.0 | P partial | W / VLOAD: Parameter metadata and normalized values are used. Vendor has value formatting; product has no plugin string parsing/plain conversion/native editor workflow. |
| [IHostApplication](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IHostApplication.html) | host imp; M; 3.0.0 | P | VCOM: Host name and mandatory message/attribute creation are implemented inside isolated worker; no editor application state authority is duplicated. |
| [IConnectionPoint](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IConnectionPoint.html) | plug imp; M; 3.0.0 | P | VLOAD / VCOM: Separate component/controller connections are established and disconnected. Single-component controller implementation is handled separately. |
| [IMessage](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IMessage.html) | host imp; M; 3.0.0 | P | VCOM: Typed attribute-backed message objects are supplied to plugin/component/controller communication. |
| [IAttributeList](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAttributeList.html) | host imp; M; 3.0.0 | P | VCOM: Integer, float, UTF-16 string and binary message attributes are implemented with owned bounded data. |
| [IBStream](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IBStream.html) | role unspecified; level unspecified | P bounded | VCOM / W: Seek/read/write state streams exist. Product opaque-state payload cap is 128 KiB; valid larger vendor state is not admitted. |
| [IStreamAttributes](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IStreamAttributes.html) | host imp; O; 3.6.0 | V | VCOM: Memory stream exposes state-context/file-name metadata primitives. Product does not offer general .vstpreset browsing and file-context workflows. |
| [ISizeableStream](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1ISizeableStream.html) | role unspecified; level unspecified | N | VCOM: No host stream size extension implementation found; base IBStream works. Absence is not itself a mandatory-host failure. |
| [IPluginCompatibility](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginCompatibility.html) | plug imp; level unspecified | V | VDISC / VMODULE: Factory compatibility JSON is parsed into metadata; product identity matching does not automatically migrate authored class IDs. |
| [IPlugInterfaceSupport](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IPlugInterfaceSupport.html) | host imp; O; 3.6.12 | P bounded | VCOM / W: Instance-local immutable declarations describe actual product effect/editor consumers. The same declaration gates optional MIDI/unit/program/expression cache construction and consumption; standalone defaults remain available. Other optional workflows retain their individual qualification boundaries. |
| [IProcessContextRequirements](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IProcessContextRequirements.html) | plug imp; M; 3.7.0 | P bounded | VLOAD / W: Requests are queried; the production typed per-block signal context supplies signed absolute sample position and rate. Optional validity is available intersect requested, with unknown tempo/signature/clocks cleared. Actual owned transport PCM qualification is recorded separately. |
| [IPrefetchableSupport](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IPrefetchableSupport.html) | plug imp; O; 3.6.5 | V | VLOAD: Optional prefetch query and vendor ProcessMode exist. Product supports realtime/offline only and has no prefetch scheduling contract. |
| [IAudioPresentationLatency](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAudioPresentationLatency.html) | plug imp; O; 3.1.0 | N | W / PH: No callback found. Physical input/output presentation latency differs from algorithmic PDC; do not substitute one for the other. |

### Parameters, authoring callbacks and transport requests

| Interface (official reference) | Normative role / level | Actual status | Source seam and boundary |
| --- | --- | --- | --- |
| [IComponentHandler](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IComponentHandler.html) | host imp; M; 3.0.0 | V, product gap | VCOM / W / U: Begin/value/end gestures and restart flags are recorded. Product does not drain them into canonical authoring transactions or service restart flags. |
| [IComponentHandler2](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IComponentHandler2.html) | host imp; O; 3.1.0 | V, product gap | VCOM / U: Dirty/editor-open/group notifications are queued. Group brackets and gestures use separate queues, losing their mutual ordering; cannot form correct group Undo until one ordered stream exists. |
| [IComponentHandler3](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IComponentHandler3.html) | host imp; O; 3.5.0 | V | VCOM / U: Host context menus can be created; product has no native plugin menu popup/selection bridge. |
| [IComponentHandlerBusActivation](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IComponentHandlerBusActivation.html) | host imp; O; 3.6.8 | N | W / VCOM: Optional request interface absent. Bus requests must be validated then applied later with matching graph/routing changes, especially multi-output instruments. |
| [IComponentHandlerSystemTime](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IComponentHandlerSystemTime.html) | host imp; O; 3.7.9 | N | VCOM: Optional host monotonic-time callback absent; expose only an actual clock with declared units and precision. |
| [IEditController2](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IEditController2.html) | plug imp; O; 3.1.0 | N | VLOAD: Knob mode, help and about APIs are not bridged; independent editor integration needed. |
| [IEditControllerHostEditing](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IEditControllerHostEditing.html) | plug imp; O; 3.5.0 | V | VLOAD / U: Vendor can delimit host-originated parameter editing. Product normalized automation works, but native UI authoring gesture workflow does not use this control interface. |
| [IAutomationState](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAutomationState.html) | plug imp; O; 3.6.5 | V | VLOAD / U: Vendor automation-state API exists; product does not map read/write states to a recording authoring workflow. |
| [IRemapParamID](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IRemapParamID.html) | plug imp; O; 3.7.11 | V | VLOAD / W: Vendor remapping query exists; product schema verification rejects mismatched authored parameters rather than guessing new identifiers. |
| [IParameterChanges](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParameterChanges.html) | host imp; M; 3.0.0 | P input / V output | W / VCOM / VLOAD: Sample-offset input automation feeds DSP. Vendor captures output parameter changes; product does not drain meters/read-only feedback into UI. |
| [IParamValueQueue](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParamValueQueue.html) | host imp; M; 3.0.0 | P input / V output | W / VCOM: Normalized sample-offset points are queued with bounded storage. Product lanes have stable admitted identifiers; feedback path and full queue saturation oracle remain required. |
| [IParameterFunctionName](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParameterFunctionName.html) | plug imp; O; 3.7.0 | N | VLOAD / U: Optional named functions such as randomize are not queried/exposed; do not infer semantics from parameter display names. |
| [IProgress](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IProgress.html) | host imp; O; 3.7.0 | V | VCOM / U: Vendor progress callbacks queue notifications. Product does not display/cancel corresponding long-running operations. |
| [ITransportControl](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1ITransportControl.html) | host imp; O; 3.8.1 | N host; ABI qualified slice | B / VCOM / U: Canonical generated 3.8.1 binding and independent real COM calls are available. Product HostApplication does not expose this interface. Future actions must advertise actual playback/locate/cycle/record capabilities and state-based refusal. |

### Events, MIDI, expression, units and presets

| Interface (official reference) | Normative role / level | Actual status | Source seam and boundary |
| --- | --- | --- | --- |
| [IEventList](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IEventList.html) | host imp; M; 3.0.0 | V, product gap | VEVT / VCOM / W: Owned rich events exist in vendor, but addEvent locks and allocates payloads and output queue overwrites oldest events. Product carries automation only; richer events need direction/time validation and bounded RT storage. |
| [IMidiMapping](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IMidiMapping.html) | plug imp; O; 3.0.1 | V declared | VLOAD / VEVT: Cache and mapped sending APIs require the instance declaration. Checked local queries cover 0..130; legacy remote backends explicitly refuse 130. Product has no MIDI-device/event authoring route. |
| [IMidiLearn](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IMidiLearn.html) | plug imp; O; 3.6.12 | V | VLOAD: Vendor MIDI1 learn API exists; product has no live learning workflow or dynamic mapping invalidation service. |
| [IMidiMapping2](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IMidiMapping2.html) | plug imp; O; 3.8.0 | N integration; ABI qualified slice | B / VLOAD: Canonical 3.8.1 byte1/byte2 layout, size/alignment/offset/GUID and mapping call-through are independently checked. Vendor does not query the interface; product has no MIDI2 input/author routing. |
| [IMidiLearn2](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IMidiLearn2.html) | plug imp; O; 3.8.0 | N integration; ABI qualified slice | B / VLOAD: Canonical 3.8.1 binding and actual COM learn call-through exist. Product has no learning workflow or n-to-m assignment mapping/invalidation; ABI availability does not create host semantics. |
| [INoteExpressionController](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1INoteExpressionController.html) | plug imp; O; 3.5.0 | V declared | VLOAD / VEVT: Enumeration, dedicated sends and generic value/text/int-value event sends require the instance declaration. Product lacks per-note identities, scheduling and expression authoring. |
| [INoteExpressionPhysicalUIMapping](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1INoteExpressionPhysicalUIMapping.html) | plug imp; O; 3.6.11 | N | VLOAD / U: Preferred physical mappings not queried; refresh on note-expression-change restart after presets change. |
| [IKeyswitchController](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IKeyswitchController.html) | plug imp; O; 3.5.0 | N | VLOAD / U: Keyswitch/articulation metadata not queried or presented; requires event-bus/channel-aware authoring. |
| [NoteOnOrchestralArticulation::IInfo](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1NoteOnOrchestralArticulation_1_1IInfo.html) | plug imp; O; 3.8.1 | N integration; ABI qualified slice | B / VLOAD / U: Canonical 3.8.1 IInfo layout/GUID/signatures and real 17-by-16 articulation output COM calls are independently checked. Product does not consume articulation metadata/events. Distinct from ChannelContext::IInfoListener. |
| [IUnitInfo](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IUnitInfo.html) | plug imp; O; 3.0.0 | V declared | VLOAD / U: Units/program cache, pitch names and selection APIs require the instance declaration. Program Change uses only declared MIDI mapping/unit alternatives. Product rack does not present hierarchy or program selection. |
| [IUnitHandler](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IUnitHandler.html) | host imp; O; 3.0.0 | V | VCOM / U: Unit selection/program-list notifications are queued, not product-serviced. |
| [IUnitHandler2](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IUnitHandler2.html) | host imp; O; 3.6.5 | V | VCOM / U: Unit-bus assignment change notification exists in vendor queue; no product routing/metadata refresh. |
| [IProgramListData](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IProgramListData.html) | plug imp; O; 3.0.0 | V declared | VLOAD / U: Per-program opaque get/set requires its independent instance declaration. No product preset/program browser or authoring transaction bridge. |
| [IUnitData](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IUnitData.html) | plug imp; O; 3.0.0 | V declared | VLOAD / U: Per-unit opaque get/set requires its independent instance declaration; product persists whole admitted plugin state only. |
| [IXmlRepresentationController](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IXmlRepresentationController.html) | plug imp; O; 3.5.0 | N | VLOAD / U: Hardware-control representation XML not queried; distinct from 3.8.1 Clipboard VST-XML. |
| [ChannelContext::IInfoListener](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1ChannelContext_1_1IInfoListener.html) | plug imp; O; 3.6.5 | N | VLOAD / U: Full name ChannelContext::IInfoListener. Product channel names/colors/identities are not passed to native controller. |

### Native views, menus, message exchange and platform event loops

| Interface (official reference) | Normative role / level | Actual status | Source seam and boundary |
| --- | --- | --- | --- |
| [IPlugView](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPlugView.html) | plug imp; level unspecified; 3.0.0 | V, product gap | VGUI / U: Vendor attach/remove/size primitives exist. Product worker has no native view UI loop; ABI key-down/up/focus forwarding was not found outside test doubles. |
| [IPlugFrame](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPlugFrame.html) | host imp; M; 3.0.0 | V | VGUI / VCOM: Vendor resizeView calls view.onSize synchronously in the resize call. Product native-editor frame, constraints and focus lifecycle remain absent. |
| [IPlugViewContentScaleSupport](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPlugViewContentScaleSupport.html) | plug impl; O; 3.6.6 | V | VGUI / U: Vendor content-scale callback exists; product has no actual monitor/DPI-change bridge. Platform coordinates differ; unit conversion must be explicit. |
| [IParameterFinder](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParameterFinder.html) | plug imp; O; 3.0.2 | N | VGUI / U: Parameter-at-view-point lookup absent; context automation menu cannot reliably guess which plugin control is selected. |
| [IContextMenu](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IContextMenu.html) | host imp; O; 3.5.0 | V | VCOM / U: Owned items, groups/flags, popup requests and selection registry exist; product popup absent. |
| [IContextMenuTarget](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IContextMenuTarget.html) | host imp; O; 3.5.0 | V | VCOM / U: Vendor retains and invokes target on explicit selected item; product has no native selection-to-target bridge. |
| [IDataExchangeHandler](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IDataExchangeHandler.html) | host imp; O; 3.7.9 | V partial product | VDATA / VCOM / W: Bounded aligned queues and realtime lock/free exist. Worker does not drain main-thread delivery; background delivery is serviced by vendor dispatcher. |
| [IDataExchangeReceiver](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IDataExchangeReceiver.html) | plug imp; O; 3.7.9 | V partial product | VDATA / VLOAD / W: Receiver is configured and flushed before deactivate. Product does not pump requested main-thread deliveries while processing, so continuous requested-main delivery is incomplete. |
| [IRunLoop](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1Linux_1_1IRunLoop.html) | host imp; level unspecified; 3.6.8 | V Linux / product gap | VGUI / VCOM / W: Vendor Linux file-descriptor/timer registration and pump APIs exist; product worker blocking protocol loop does not pump editor run-loop work. |
| [IEventHandler](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1Linux_1_1IEventHandler.html) | plug imp; level unspecified; 3.6.8 | C Linux | VGUI / VCOM: Plugin file-descriptor callbacks belong to conditional Linux native-view hosting. Vendor primitives retain handlers; product editor loop absent. |
| [ITimerHandler](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1Linux_1_1ITimerHandler.html) | plug imp; level unspecified; 3.6.8 | C Linux | VGUI / VCOM: Plugin timers belong to Linux native-view run loop; no product editor timer servicing. |
| [IWaylandHost](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1IWaylandHost.html) | host imp; level unspecified; 3.8.0 | N, C Linux | VGUI / VCOM: No Wayland host service implementation found; optional native platform must use matching supported embed mechanism. |
| [IWaylandFrame](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1IWaylandFrame.html) | host imp; level unspecified; 3.8.0 | N, C Linux | VGUI / VCOM: No Wayland frame interface implementation found; latest scale/lifecycle behavior needs platform test. |

### Conditional wrapper and iOS services

| Interface (official reference) | Normative role / level | Actual status | Source seam and boundary |
| --- | --- | --- | --- |
| [IVst3ToAAXWrapper](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IVst3ToAAXWrapper.html) | host imp; M; 3.6.8 | C, not applicable | VCOM: Mandatory when acting as that format wrapper. Mondrian is a native VST3 host, not a wrapper; do not falsely expose wrapper identity. |
| [IVst3ToAUWrapper](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IVst3ToAUWrapper.html) | host imp; M; 3.1.0 | C, not applicable | VCOM: Mandatory when acting as that format wrapper. Mondrian is a native VST3 host, not a wrapper; do not falsely expose wrapper identity. |
| [IVst3ToVst2Wrapper](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IVst3ToVst2Wrapper.html) | host imp; M; 3.1.0 | C, not applicable | VCOM: Mandatory when acting as that format wrapper. Mondrian is a native VST3 host, not a wrapper; do not falsely expose wrapper identity. |
| [IVst3WrapperMPESupport](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IVst3WrapperMPESupport.html) | host imp; O; 3.6.12 | C, not applicable | VCOM: Optional wrapper MPE service; native host event support is evaluated separately from wrapper identity. |
| [IInterAppAudioHost](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IInterAppAudioHost.html) | host imp; O; 3.6.0 | C iOS, not applicable | platform boundary: Inter-App Audio services are conditional iOS integration; not a desktop-host completeness requirement. |
| [IInterAppAudioConnectionNotification](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IInterAppAudioConnectionNotification.html) | plug imp; level unspecified; 3.6.0 | C iOS, not applicable | platform boundary: Inter-App Audio services are conditional iOS integration; not a desktop-host completeness requirement. |
| [IInterAppAudioPresetManager](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IInterAppAudioPresetManager.html) | plug imp; level unspecified; 3.6.0 | C iOS, not applicable | platform boundary: Inter-App Audio services are conditional iOS integration; not a desktop-host completeness requirement. |

### SDK general services and validator interfaces

| Interface (official reference) | Normative role / level | Actual status | Source seam and boundary |
| --- | --- | --- | --- |
| [IAttributes](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IAttributes.html) | host imp; level unspecified | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IAttributes2](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IAttributes2.html) | host imp; level unspecified | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [ICloneable](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1ICloneable.html) | plug & host imp; level unspecified; N4.12 | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IDependent](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IDependent.html) | plug imp; level unspecified | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IErrorContext](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IErrorContext.html) | plug imp; level unspecified; Sequel 2 | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IPersistent](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPersistent.html) | plug imp; level unspecified | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IString](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IString.html) | host imp; level unspecified | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IStringResult](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IStringResult.html) | host imp; level unspecified; SX 4 | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [IUpdateHandler](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IUpdateHandler.html) | host imp; level unspecified | SDK service, not integrated | VCOM: General Steinberg SDK service. Class page does not state ordinary VST3 host mandatory/optional level; no product integration found. Do not confuse IAttributes with mandatory Vst::IAttributeList. |
| [ITest](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1ITest.html) | role unspecified; level unspecified | C validator/tooling; generator exclusion | test boundary: Ordinary desktop host operation does not expose validator identity. This original-generator exclusion is documented in canonical SDK provenance, not claimed as a generated runtime interface. |
| [ITestFactory](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1ITestFactory.html) | plug imp; level unspecified | C validator/tooling; generator exclusion | test boundary: Official plugin-supplied validator infrastructure; original generator excludes test interfaces. Independent official tooling is a separate acceptance path. |
| [ITestResult](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1ITestResult.html) | host imp; level unspecified | C validator/tooling; generator exclusion | test boundary: Official validator result infrastructure; original generator excludes test interfaces. No product execution service is claimed. |
| [ITestSuite](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1ITestSuite.html) | host imp; level unspecified | C validator/tooling; generator exclusion | test boundary: Official validator suite infrastructure; original generator excludes test interfaces. No product execution service is claimed. |
| [ITestPlugProvider](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1ITestPlugProvider.html) | role unspecified; level unspecified | C validator/tooling; generator exclusion | test boundary: Official validator provider infrastructure; original generator excludes the test-provider header. Product is not acting as this validator role. |
| [ITestPlugProvider2](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1ITestPlugProvider2.html) | role unspecified; level unspecified | C validator/tooling; generator exclusion | test boundary: Official validator provider extension; original generator excludes the test-provider header. Product is not acting as this validator role. |
