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
- [CLAP public version header](https://github.com/free-audio/clap/blob/main/include/clap/version.h)
  declares 1.2.10. Stable extensions and draft extensions are separate obligations.
  The pinned 1.x binding's ABI compatibility does not establish feature parity.
- [OpenFX 1.5.1 release](https://github.com/AcademySoftwareFoundation/openfx/releases/tag/OFX_Release_1.5.1).
  Main-branch clarifications and extensions must be identified separately from
  the published release. The current product advertises host API 1.4; selected
  newer properties do not establish complete 1.5.1 hosting.

## Implementation boundary

| Feature family | Current implemented boundary | Remaining work |
| --- | --- | --- |
| Native discovery and isolation | Selected class/descriptor, fingerprinted native binary, supervised discovery and persistent processing child, bounded control traffic and shared buffers | Standard scan locations/workflow, permitted vendor corpus, platform and native editor lifecycle |
| VST3 SDK interfaces | One canonical runtime binding package generated from verified SDK 3.8.1 sources; independent layout/signature and actual cross-language calls for all nine methods of the four new interfaces | Optional host-service implementation, MIDI 2 event routing, native MSVC/macOS/Linux qualification; upstream generator excludes SDK testing interfaces |
| Audio layout | Mono/stereo main in/out, up to 31 auxiliary inputs of the admitted layout; restored-state native port metadata and causal PDC | Mixed layouts, surround/ambisonic, multiple outputs, zero-input generators/instruments |
| Audio parameters | Canonical author curves and sample offsets; CLAP plain values, VST normalized values; insertion/rebinding/history/archive state | Native display conversion, units/programs/presets, output events, plugin-origin gestures, large state/resources |
| Audio render mode and threads | Explicit VST realtime/offline selection; CLAP requested-mode probe and execution, hard-realtime/declined-mode refusal; instance-local CLAP symbolic thread checks | Dynamic restart/rescan/latency/tail handling, sleep/request-process scheduling and extension event loops |
| Audio transport and events | Canonical signal sample position reaches VST3 ProcessContext and CLAP seconds transport; unavailable tempo/beat/signature/transport facts are not invented | Tempo-map authoring, MIDI 1/2/SysEx, notes/expression/polyphonic modulation and event routing |
| Native editors | Vendor primitives exist; product-native window hosting is incomplete | Isolated editor protocol, parameter/state transactions, resize/DPI/focus/keyboard, GUI/timers/FDs |
| OpenFX CPU image execution | Filter Source/Output, current-frame full-frame RGBA Float32, progressive; typed output association and finite division into the existing straight coverage graph; resident native instances | Complete finite Float association including zero-alpha emission and signed/out-of-range alpha, other contexts/required inputs, formats, fields, mixed PAR/depth, tiles/scale/temporal/ordered scheduling |
| OpenFX capabilities and bootstrap | Actual Float32/CPU advertisement; optional early host bootstrap; CPU-disabled admission refusal; canonical PAR dimensions and progressive fields | GPU execution and negotiated colour/lifetime/synchronization contracts |
| OpenFX author controls | Numeric tuples/colour/bool/choice/StrChoice/UTF-8 text/resources; canonical curve queries and shared transactions | Spatial semantic types, hierarchy/buttons/custom/parametric/interacts, dynamic descriptors, plugin-origin writes/private data |
| OpenFX product callbacks | Native message/progress/timeline primitives exist | Typed application messages, real cancellation/progress/transport responses and consistent persistence |
| Product interaction | Shared Rack control identity preserves keyboard ownership through canonical refresh; focused text owns clipboard and bounded local Undo/Redo; exact native text modifiers; fresh author callbacks and values | Remaining control adoption, accessible numeric names, complete visual BiDi navigation and physical/native-editor acceptance |
| Performance | Persistent children, bounded mappings, preallocated native audio buffers and execution budgets | Measured native DSP plus IPC percentiles/deadlines, video frame budgets, warm/cold/memory/overload and vendor/platform matrices |

This table describes source capabilities. Each qualification below remains tied
to its exact source revision, binary hashes, environment and named tests.
The complete [audio feature matrix](native-audio-standard-features.md) and
[VST3 interface inventory](vst3-interface-inventory.md) distinguish actual product
consumers, vendor primitives, unavailable negotiation and remaining work.

## Required qualification evidence

For each feature: normative contract and negotiation; independent owned oracle;
official reference or validator where applicable; permitted independent vendor
coverage; author Undo/Redo and save/reopen; interruption/error/teardown; supported
platform/device/native UI; measured execution cost. Audio checks use per-sample
references across nonzero starts, partitions and seeks. Image checks include
HDR/negative/alpha, PAR, canonical/pixel coordinate seams and actual encoded output.

Audio performance measures the complete native worker callback after excluded
warmup, at declared rates/layouts/buffer sizes, with p50/p95/p99/max and deadline
misses. The available callback deadline is derived from frames/sample-rate;
crash timeouts are not audio performance budgets. Video measures actual resident
worker frame execution against a declared frame budget and image correctness.
Report build profile, CPU/OS, source and fixture hashes, iterations, setup cost,
tail/overload/cancellation and memory. No universal zero-overhead or compatibility
claim follows from a fixture, cache counter or a single machine.

Correctness qualification builds the product with the same test profile used by
the integration harness, then verifies a copy in a content-addressed directory
before starting any native tests. Every helper uses that immutable copy. The
initial receipt records source, product and fixture hashes; completion rechecks
all of them. Cargo can rebuild its shared `target/debug/mondrian.exe` during an
integration test, so that shared path cannot serve as a frozen helper artifact.
Test-profile correctness results are separate from release performance evidence.

## Current execution receipts

The full pre-change workspace baseline is documented in
[modal-resource-lut-acceptance.md](modal-resource-lut-acceptance.md).
New plugin and control changes need fresh named qualification and are not covered
by the earlier complete Golden or native Scope receipts.

The new SDK ABI slice has executed independent official C++ layout/signature
checks and all nine C++ calls into real Rust COM interfaces on Windows x64,
including reference lifetime and interface identity. This establishes that slice's
ABI behavior, not implementation of the corresponding optional host services.

The complete-line geometry and bounded local history stage passed all 57 UI
text tests, 244 text-input tests, 32 numeric tests, one cold-owner admission test,
14 actual Host numeric draft/clipboard/history tests and the modal release test.
The Host test includes twenty real 512 KiB Pastes: eviction or exhausting local
Undo does not invoke project Undo, and confirming a fresh numeric edit releases
local ownership. Sources and logs were hashed before/after each frozen gate.

The subsequent complete selection stage reproduced three actual pixel failures
for partial RTL selection, disjoint BiDi spans and IME origin. After repair, all
248 text-input tests, three new renderer contracts and the cold-owner selection
admission test passed. These include reverse-anchor Undo restoration, Arabic
partial clusters and a long contiguous selection without per-glyph rectangles.
Visual BiDi arrow traversal and native/physical IME acceptance remain open.
Strict workspace lint and fresh native product gates are separate requirements.

The frozen foundation native run completed on 2026-10-09 (Asia/Shanghai):
**21 gates, 46 tests, zero failures**. It covers real CLAP/VST3 buses/state/modes/
signed transport; OpenFX bootstrap/API admission/PAR/CPU/limited alpha and the
official Basic filter; five supervised resource workflows; ten native sidechain/
Scope Undo/save-reopen workflows; and eleven portable LUT/media/OCIO output cases.
The test-profile helper SHA-256 is
`BA21F174FC0B7976DCAC25DFB48C0E8E6457FCE64702A094EE133F533A3D4000`.
The receipt SHA-256 is
`FD02B0662CBCDF353067AB870DEE5D9497BE99683BCCEB3EE3C80034A005B74A`
(`.scratch/plugin-standard-audit/foundation-native-pinned-receipt.json`).
All tested source, seven native binaries and all 21 logs were independently
rechecked after completion. Only documentation changed before the checkpoint
commit. The first run's shared-product artifact check had failed; the immutable
helper run resolves that failure without weakening the stability checks.

`cargo fmt --all --check` and strict workspace/all-target/all-feature Clippy with
`-D warnings` passed for this source. Clippy log SHA-256:
`0099A18186DBB3B075A7E9CCA5A80A2808766E83E55D2677AC62C3BE1259B33B`.
These selected correctness results do not qualify native release performance,
the unfinished standard features, arbitrary vendors or other platforms.

The public SDK reproduction script also completed a fresh end-to-end run from
pinned official archives and the independent generator lock. Its generated
binding SHA-256 is `9cf5371fc7a883a857e4c74d8d4c9de43cdd958e14a73e3158cb568ff6d23940`,
identical to the canonical binding. The generator used LLVM 18.1.8 with its
original Windows GNU target selection; this does not qualify native MSVC,
macOS or Linux ABI behavior.

Packaged distributions copy `LICENSES`, which now retains the exact official
Steinberg SDK MIT notice, the binding runtime's license options, and the
HarfRust notice alongside the existing native-adapter notices. Source and
generation manifests keep their independent license identities.
