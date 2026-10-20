---
status: accepted
---

# Restore native audio state before preparation

## Problem

The VST3 convenience loader prepares buffers at its internal 44.1 kHz/512-frame
defaults before applying the Host configuration. Component state is restored
later and can change the bus topology. Post-load metadata validation therefore
cannot bound those earlier allocations. Applying the final Mono/Stereo channel
limit to the default topology would also reject a component whose saved state
legitimately restores a different layout.

Checking only sample arrays misses inactive-bus pointer arrays, activation and
arrangement inventories, meters, caller-owned buffers and old/new ProcessData
overlap. A later storage visitor cannot provide admission before allocation.

## Decision

Add an explicit deferred load-preparation policy, separate from optional,
immutable processing-resource bounds. The generic Eager policy remains the
default. Deferred loading initializes and connects the same component/controller
without preparing DSP buffers or activating default audio/event buses. It does
not resubmit the component's current arrangements as a new host request.

Mondrian restores state and uses its existing configuration operation. The first
successful native setup precedes actual audio/event bus acknowledgements and
ProcessData construction. Existing pending-input guards remain authoritative;
deferred loading grants no permission to discard, replay or flush foreign input.
No new Ready flag, state inventory, execution scheduler or native owner is added.
The existing prepared-data/setup facts govern later operations.

This follows the [VST3 processor lifecycle](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical%2BDocumentation/Workflow%2BDiagrams/Audio%2BProcessor%2BCall%2BSequence.html):
state restoration is permitted before setup, while bus activation belongs after
successful setup. The generic Eager ordering is not qualified by this decision.

The SDK checks each actual allocation-driving count, channel total and frame
extent. Its checked request uses actual owning-type layouts and a conservative
buffer-family peak: `2P + 2A + Q + W + 2L + M`. P covers ProcessData and sample/
pointer storage at F64 width; A covers audio activation vectors; Q covers owned
and raw arrangement arrays; W is the consumer's single retained F32 bus-buffer
set; L covers queried layouts; M is preallocated meter channel storage. Inactive
caller samples and SDK pointer arrays remain included. Deferred meters have no
valid channels until preparation succeeds and never grow during DSP callbacks.

The original native lane profile carries this separate peak through strict IPC19
and the existing parent Session scratch admission for retained creation. The
child recomputes the same-binary preparation profile before binary access. This
does not move discovery probes or manual Unprobed validation launches behind
the canonical Root grant; those existing entry boundaries remain separate.
Bounds are not a second grant.
Fresh uses the same Host policy, drops the previous Plugin before new loading,
and retains its one W; it does not invent two overlapping Plugin instances.

Cold and Fresh first validate class, buses, parameters and events, then use the
original native start and compare exact latency/tail before publishing Ready or
installing the Fresh Plugin. No process/PCM call occurs before that comparison.
The old Eager load implicitly activated before validation; Deferred exposed that
activation-dependent latency was being compared too early. The deliberate error
order is now static metadata, native start, then execution facts. The official
[call sequence](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical%2BDocumentation/Workflow%2BDiagrams/Audio%2BProcessor%2BCall%2BSequence.html)
permits these UI queries in Processing. No extra activation API or state owner is
introduced, and failed matching remains terminal with the original hold/fees.

## Migration and validation

Finite Windows qualification is complete for this buffer family: SDK401,
Audio770 and App181 ordinary controls, plus 27 native exact controls (ten SDK,
sixteen Audio/Product and one App). Eager/no-bounds 33-bus, zero-channel and
fallback controls remain passing. The real DLL default-six-channel to saved
Stereo case, first 48k/4-frame setup, Event16, original Note/initial-input/zero
paths and real MDA rescan pass. The actual Runtime latency failure turns green
under its unchanged assertions after the activation-phase correction; seed and
App regressions pass. Resource tests cover checked geometry, actual overlap and
exact/one-byte-short parent admission. These are finite functional conditions,
not release-performance or complete-heap acceptance.

This request excludes other metadata, COM payloads, complete Arc allocations,
plugin/module/OS heaps and physical memory commitment. It does not qualify full
heap usage, realtime safety, other platforms or every SDK load path. Audio DAG,
author state, Timeline semantics, project format and retirement authority remain
unchanged. The current capability matrices remain the qualification authority.

Initial processing mode is a separate existing gap: cold/Fresh restore prepares
with the SDK's default Realtime mode before the Worker sets Offline. The current
rate/block-size qualification does not certify the first setup's mode. A later
initial-mode input must follow the same load body without preparing default buses.
