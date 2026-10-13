---
status: accepted
---

# Keep audio authoring Sequence-owned, placement-derived, and typed

## Decision

Each Sequence is one validated authoring aggregate containing its Timeline,
Audio Roles, Audio Program, and public audio outputs. Ordinary PCM routes never
cross that aggregate. All strong references inside it must resolve before a
snapshot can be saved or compiled.

`Track -> Clip` is the only placement authority. An audio Clip owns one or more
`AudioComponentEdit` values. An edit selects a stable media audio component or
nested public output and owns only placement-local behavior: enable state,
Role, local gain/pan automation, fades, and a restricted processing binding.
It does not persist `TrackId`, Sequence range, speed, source in/out, or nested
placement. Those facts are derived from its owning Track and Clip.

Clip-level processor state lives in Sequence-owned `AudioProcessingScope`
values. A binding contains exactly `scope_id` and exact `scope_in`. A Scope may
be shared by edits created by a razor operation, but cannot become a second
placement model: it has no Track, range, source, speed, Route, or output fields.
Copying into another independent edit/Sequence forks Scope, Processor,
Keyframe, and Edit identities. Razor splitting forks placement-local Edit
identities while retaining the Scope identity and advancing both edit-local and
Scope-local origins. This preserves the shared processing definition and its
author-time coordinate without duplicating placement truth. Scope identity
alone never authorizes mutable DSP-state sharing: preparation derives a
separate processor occurrence for each Contribution. Mutable state may be
shared only when the complete signal projection, processor contract, parameter
delivery, nested instance path, evaluation mode, and continuity epoch prove one
identical Audio State Domain; absent that proof, execution remains independent.

Duplicating a Sequence forks every Sequence-local Audio Role, Component Edit,
Processing Scope, processor instance, Bus, Program Output, Route, Transition,
and automation-keyframe identity. A nested Component Edit continues to refer to
the child Sequence's existing public output because that reference crosses the
duplicated aggregate boundary.

The Audio Program contains keyed Track Mixer Channels, Mix Buses, Program
Outputs, explicit typed Routes, Processing Scopes, and explicit two-endpoint
Transitions. A Track Mixer Channel uses the owning audio Track ID and lifetime;
Buses and Outputs use their own IDs. The entities reuse `AudioChannelStrip` and
`AudioProcessorRack` values but do not collapse into one generic node type.
Routes name typed source ports (`PreFader`, `PostFaderPreMute`, `PostMute`) and
typed destinations. Instantaneous cycles are invalid.

The same Audio Program owns default-empty, versioned `AudioNoteLane` values.
Each lane and note has its own stable typed author identity. A lane strongly
references an existing audio Track and a processor in its pre/post rack, even
when its score is empty or its target is bypassed. Notes retain exact Sequence
times, concrete channel/key and finite on/off velocities; velocity-zero On is
still On. The port key is opaque author intent until native preparation binds
it. It is not a ParameterId or a scan index. Footprint, snapshots and Undo/Redo
use the existing author aggregate. Track removal prunes its lanes; Sequence
duplication forks lane/note IDs and remaps their Track/processor references.
Removing only a strongly referenced processor is invalid; removing its lane in
the same transaction is valid. Document schema32 protects this data, with an
explicit absent/empty31 migration; archive format and library schema stay intact.
Stored score data does not itself grant Note execution or instrument support.

An `AudioTransition` strongly references two `AudioComponentEditId` values and
an exact Sequence-time range. It affects only those endpoints. Ordinary overlap
is ordinary summing. Structural edits prune transitions whose endpoints or
range are no longer valid; they do not silently retarget them.

Every rack contains the same `AudioProcessorInstance` author type for built-in,
VST3, CLAP, and future adapters. Stable definition/instance/parameter IDs,
bypass, exact automation, schema version, and opaque plugin state are author
data. File paths, scan indexes, display names, video `EffectNode`, and generic
JSON property bags are not audio processor identity.

Mute is persistent Track mixer intent and gates only `PostMute`. Disabled Clip
or Component Edit means absence from the compiled Program. Solo is a transient
audition overlay supplied to compilation; it is not persisted on Track and
never changes the canonical Program Output.

Audio Roles remain Sequence-local semantics beside the routing graph. Each
Component Edit has at most one Role; ancestors follow an acyclic Role hierarchy.
Projects may provide templates and standard semantic keys but own no live Role
graph referenced from a Sequence. A parent binds child PCM by stable public
output ID and assigns parent-local semantics separately.

## Consequences

- Moving, trimming, slipping, splitting, copying, overwriting, and nesting must
  mutate the Clip and its restricted audio coordinates atomically.
- There is no compatibility author model or flat fallback graph in alpha.
- Missing Sequence-local entities and Project-contained nested Sequence or
  public-output references reject validation. Missing media, plugin, or other
  provider bindings preserve recoverable author intent but block execution
  unless an explicit consumer policy permits a reported degradation.
- Sends, sidechains, feedback, and richer layouts must extend typed ports and
  validation; they cannot introduce string connections or a parallel graph.
