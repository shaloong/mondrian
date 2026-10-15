# Owned CLAP NoteGate candidate

**Owned validation fixture, not third-party or Mondrian Note compatibility evidence.** Root owns protocol/Note integration and actual compilation/native gates. Retained receipts and docs/dev/plugin-host-sprint.md identify the tested source and acceptance. This fixture declares no host Session, author model, IPC or Timeline projection. Its private `Gate` is the actual plugin's fixed DSP state and independent input-tuple oracle.

The baseline class is `org.mondrian.fixture.note-gate`, version `0.0.0`, using the existing official CLAP 1.2.10 `clap-sys` ABI. There is one mono Float32 input (ID `0x101`) and one mono Float32 output (ID `0x201`), with reciprocal in-place pairing. Disjoint and exact in-place buffers are accepted; overlapping nonidentical buffers, Float64, zero or >8192 frames and additional audio buses are refused. Constant input is cached before any in-place output write, so a changed Gain cannot feed processed sample zero back into later input frames. This class is a PCM-input **effect**; the fixed zero-input variants are described below. One plugin instance owns one Box until main-thread destroy, and exactly one native audio callback owns its UnsafeCell DSP state. Factory creation is thread-safe and identifies no main thread. Native init/metadata require the actual host ThreadCheck extension; destroy enforces available actual main-role checks and inactive/stopped state. If init failed before that optional service was usable, the official main-thread destroy caller precondition still permits release of the inactive Box. No GUI or external resource is used.

## Fixed zero-input variants

The factory also exposes `org.mondrian.fixture.note-gate.zero-input` and `org.mondrian.fixture.note-gate.zero-input.three-outputs`. Both use a constant carrier of 1 with the same Gate, note identities, End rules and `MNG1` parameter state. The former has one mono output (ID `0x201`). The latter has three mono outputs in native order: ID 1 Main, ID 2 Auxiliary, ID 3 Auxiliary. Their topology is immutable per class; no audio-port configuration or host rescan service is introduced.

For the three-output class, each sample advances the Gate exactly once. The three outputs are respectively `y`, `-y * 0.5`, and `y * 0.25`; the literal ABI oracle checks their Float32 bits, including the auxiliary negative zero. All declared native outputs must have valid, disjoint buffers before the Gate or an Off count is changed. A zero input count accepts either a null pointer or a non-null pointer without dereferencing it. Only this class adds read-only parameter 29: the actual Process callback invocation count, including refused calls, since the last accepted activation/reset. It is diagnostic state, absent from `MNG1`, and emits no host parameter events. The original two classes retain their three parameters.

The library test inventory is expected to contain the original ten controls plus three new raw ABI controls for literal multi-output PCM and partitions, shape/alias/read-only refusal, and original Off/End sink behavior. Actual retained build and execution receipts determine their validation status. These tests invoke the real exported CLAP callbacks in a test executable; they do not show that Mondrian loaded the separately built DLL or routed multiple native outputs. Canonical DAG, native adapter/IPC, third-party, real-time and other-platform acceptance remain separate gates.

## Stable native identities

| Kind | ID / index | Meaning |
|---|---|---|
| Note input | unsigned stable ID `0`, native index 0 | gate weight ×1 |
| Note input | unsigned stable ID `0x80000017`, native index 1 | gate weight ×2 |
| Writable parameter | 17 | Gain, plain range [0,1], default 1 |
| Writable parameter | 19 | End scenario, stepped [0,7], default 0 |
| Read-only parameter | 23 | Exact Off count, reset on native activation/reset |

Both note inputs expose CLAP-native dialect only. There are **zero note output ports**: a NoteEnd refers to the original input tuple, not an output port. Input Notes require concrete `(port_index 0..1, channel 0..15, key 0..127, note_id >=0)` and finite velocity [0,1]. Eight fixed records distinguish two same-key voices by exact ID and full tuple. Duplicate held IDs, excess records, wrong/duplicate Off tuples, unknown input event types, parameter cookies/per-voice parameter values and out-of-range scalars cause bounded process refusal; no silent stealing or reinterpretation as MIDI is performed. Flags 0..3 are retained as known input metadata; no unknown flags are admitted. Invalid Params flush is latched for the next Process because the ABI flush has no status result.

## Independent PCM oracle

Input events are consumed in their actual supplied order at sample `n`, **before** producing sample `n`. The fixture neither sorts events nor calls any Mondrian lowering function. The copied input event array has 128 slots; event timestamps must be nondecreasing and `< frames_count`.

For each native voice `j`, with original port `p` and On velocity `v`:

`w_j = (1 + v) * (p + 1) / 32`

Before Off, `e_j[n]=1`. For an Off at absolute sample `o`, its envelope at `o,o+1,o+2,o+3` is exactly `1, 3/4, 1/2, 1/4`, then zero. Off velocity is range-checked but deliberately does not modify this oracle. Native NoteEnd is emitted **after** producing sample `o+3`. A zero-velocity On has nonzero `w_j`; it remains a real On. Every sample is:

`y[n] = f32(f64(x[n]) * Gain[n] * sum_j(w_j * e_j[n]))`

The sum uses fixed slot order and double precision; there is no clipping or precision substitution. The attached JSON vectors choose dyadic values exactly representable in Float32, so equality is a legitimate PCM oracle. Compare one long block with partitions such as `[3,2,4,5]`, translating these independently declared native absolute event samples to each block's offsets. A split cannot restart release envelopes or repeat Ends. These vectors are declarations, not executed test evidence.

Default scenario 0 supports held voices and the four-sample release. Scenario 1 ends a held voice after its **second audible sample**, emitting early End before authored Off. Its weight is zero thereafter, but the fixture retains the original tuple until one exact Off arrives; parameter 23 increments only for that original tuple. A changed tuple is refused and no replacement ID is manufactured. If Off arrives before those two samples finish, it initiates the normal release. This supports the parent's independent End-before-Off pairing test without implementing a host ledger inside the plugin.

Every End is immediately surrounded by two actual Gain parameter callbacks:

`ParamValue(time=final_sample_offset, same current Gain)` → `NoteEnd(time=u32::MAX)` → `ParamValue(time=final_sample_offset, same Gain)`.

The End timestamp is intentionally ignored by the standard, while parameter timestamps remain ordered. Each End carries the original input port/channel/key/concrete ID in normal modes. Two Ends at one sample each have their own parameter pair. `try_push(false)` is a genuine callback refusal and latches `CLAP_PROCESS_ERROR`; no retry, error logging, blocking wait or pretending success occurs.

## Controlled End scenarios

Scenario is sampled on **On** and remains fixed for that voice; changing parameter 19 cannot retroactively corrupt another voice's release.

| Value | Output alteration | Classification / intended use |
|---|---|---|
| 0 | Concrete original tuple after four release samples | Positive reference |
| 1 | Concrete original tuple after two held samples; later original Off required | Early End positive reference |
| 2 | Emit the identical End twice | Controlled duplicate; host must not grant two capacity credits |
| 3 | Flip original port index between 0 and 1 | Controlled tuple mismatch |
| 4 | Emit `note_id=-2` | Controlled illegal scalar |
| 5 | Emit `note_id=-1` with concrete remaining tuple | Standard-valid wildcard, deliberately outside the proposed first host subset; no accusation of standard violation |
| 6 | Concrete ID, wildcard port/channel/key (`-1`) | Standard-valid unique-ID match; positive reference for the proposed subset |
| 7 | Flip original key bit 0 | Controlled tuple mismatch |

These are separately selected native test inputs, **not format/name compatibility hacks**. Their bad output must never be used for a positive normal-plugin claim. Actual consumer acceptance/refusal and copied observations determine each host test's result. Duplicates cannot be called passed merely because the plugin returns Continue.

## State, lifetime and resource boundary

State is a 16-byte `MNG1` header, little-endian Gain f64 bits, then little-endian scenario u32. Partial stream transfers are handled in at most 16 successful iterations, over-report/zero/error is refused. Load validates all values before publication and requires inactive main ownership; Save reads only scalar parameters. Trailing bytes are not interpreted. This is **parameter/preset state**, not voice/DSP history. Native reset kills all old voices and clears diagnostic counts/fault, preserving parameters; stop alone preserves voices until reset/deactivate. Seek/loop equivalence requires the host's correct reset-and-replay, not saving this preset and claiming exact DSP continuation.

Original process/reset/flush paths use only fixed arrays, numeric arithmetic, native atomics and existing CLAP callbacks. All FFI helpers are private; the pure DSP module denies unsafe. The one lifecycle Box allocation is outside audio processing, and error status has no framework logging path. Native 64-bit atomics are required. This is a source-level bounded-storage assessment, **not measured allocation/RT/deadline proof**: compile, allocation instrumentation, real callback/thread/lifetime tests and the complete Mondrian path remain required. Foreign readable pointers and exclusive CLAP callback lifetimes are explicit ABI safety preconditions; a null/count/extent check cannot prove arbitrary foreign memory safe.

Integrated from sealed R2 A649F6AA. The dependency path now points to the existing official ABI from this fixture directory. Root generated Cargo.lock offline and formatted the source before build admission. Build with `cargo build --offline --locked --manifest-path crates/mondrian-audio/tests/fixtures/clap-note-gate/Cargo.toml --target-dir <isolated-fixture-target>`. Retained external build and test receipts determine actual acceptance; this source alone does not qualify Mondrian Note execution.

The source is original Mondrian fixture code under the existing MIT notice. It reuses the owned metadata fixture's official-ABI table/Box/ThreadCheck pattern, not third-party plugin DSP. The existing `clap-sys` dependency remains `MIT AND (MIT OR Apache-2.0)` with its recorded official provenance and notices; no new dependency, download, core licensing restriction, SDK replacement or redistribution conclusion is introduced.

## R2 changes and real ABI controls

R1 stays sealed as negative source evidence. Independent report `note-gate-independent-review-r1/report.md` (`CB28211F4EFA0D91B14D45FDDFC0444BB550B6923008A8102F16145F7B0272A0`) found the constant in-place feedback and incorrect factory-thread destroy requirement. R2 changes only those production-candidate responsibilities plus test-only Drop observation and ABI control source. `src/gate.rs`, all three `oracle-cases.json` vectors, capacity constants, native identities and controlled End rules retain the original bytes/assertions.

`src/abi_tests.rs` introduced three controls. Two create via the real exported factory on a joined different OS thread, then call real init/destroy on actual main: one successful ThreadCheck init, one deliberately failed init without the optional service. Test-only `Instance::drop` counts actual Box destruction; it does not turn a boolean helper result into lifetime proof. The third invokes real create/init/metadata/activate/start/process/stop/deactivate/destroy callbacks with actual raw mono buffers and typed Note/parameter inputs, both exact in-place and disjoint. With constant input 2, port0 zero-velocity On at 0 and Gain changed from 1 to 1/2 at 2, its independently declared six samples are `[1/16,1/16,1/32,1/32,1/32,1/32]`; output constant_mask must be zero and both Boxes must drop. The old R1 in-place path predicts wrong samples; its factory-thread guard predicts no main-thread drop. These are static regression expectations, **not executed red/green evidence**.

The test driver serializes only its own Drop counter outside plugin callbacks. Pointer addresses cross only the joined thread-safe factory call with exposed provenance and live host/context lifetime. It supplies a small CLAP caller/thread-check stub, not a second Mondrian Session/IPC model. Production candidate callbacks gain no test mutex, counter or allocation. Actual fixture test gates and future Mondrian DAG gates remain separately identified under the central native schedule.

Root integration also removes the independently confirmed main/audio exclusivity restriction: main callbacks require the main predicate, while audio callbacks require the audio predicate. The fourth ABI control exercises real inactive main lifecycle and actual Box destruction when ThreadCheck reports both roles. Standard callback/lifecycle exclusivity remains a safety precondition, not a consequence of these predicates. The original parameter-metadata fixture is unchanged. An actual raw-callback release/early-End trace with copied Param→End(MAX)→Param and split-block PCM is a separate next control; these three ABI controls alone do not qualify that behavior.
