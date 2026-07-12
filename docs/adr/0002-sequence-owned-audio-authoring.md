---
status: accepted
---

# Keep audio authoring sequence-owned and typed

Each Sequence root owns its Timeline, Semantic Catalog, Audio Program, and
public output interface as one validated authoring aggregate. Its internal
entity references are strong and closed; ordinary PCM routes never cross a
Sequence boundary. Nested Sequences are instanced composite sources exposing
stable typed outputs, while Project/export data packages those outputs and
user/workspace state maps them to physical monitors. A missing external plugin,
media asset, or nested Sequence may remain a recoverable dependency with an
expected contract, but a missing local Track, Bus, Route, Role, processor slot,
or port rejects the authoring transaction.

The Audio Program is one typed routing model containing Timeline Track Mixer
Channels, Mix Buses, Program Outputs, and explicit typed Routes. A Track Mixer
Channel uses its owning Audio Track identity and lifetime rather than inventing
a second independently-lived entity; Buses and Outputs have their own stable
identities. These owners compose the same Channel Strip and Processor Rack
values without erasing their different legal ports or lifecycle rules. Sends,
sidechains, faders, and route tap points remain explicit routing semantics, not
magic processors or string-named connections. A Program Output's main source is
a closed choice between typed routed inputs and one narrowly typed Semantic
Projection; both cannot feed the same main input implicitly.

Every independently processable clip audio contribution may own its Clip rack;
Track Mixer Channels, Mix Buses, and Program Outputs may own ordered racks at
fixed insertion points. Every rack contains the same Audio Processor Instance
author type whether its definition is built in, VST3, CLAP, or a future adapter;
formats differ only behind typed host adapters and capability contracts.
Instances preserve stable definition and parameter identities, bypass state,
bus layout, parameter values and automation, and versioned opaque plugin state.
Video `EffectNode`, JSON parameter bags, file paths, display names, and registry
indexes are not audio processor identity.

Audio Roles are Sequence-local semantic entities beside, not inside, the
routing graph. A contribution has at most one authoritative Role, with ancestor
roles implied by an acyclic local hierarchy. Projects may provide templates,
namespaced Standard Semantic Keys, derived indexes, and atomic cross-Sequence
commands, but own no live Role graph referenced by Sequences. Parents bind child
PCM through public output identity and separately assign parent-local semantics;
they never reference child-internal Roles.
