# Native VST3 sidechain reference fixture

Separate owned MIT OR Apache-2.0 adaptation of the existing pinned vst3-rs Gain
fixture, with new class/controller identities and state magic MSS1. Existing
gain fixture binaries and historical identities are not changed.

Default state: gain0.5, mode0, stereo main plus two default-inactive stereo auxiliary
inputs, one stereo output. All input display names intentionally match. Native
activateBus is tracked and processing refuses inactive admitted slots. DSP is
`gain * (2*main + 3*aux1 + 5*aux2)`, delayed by exactly3 frames with tail3.
Mode1 swaps auxiliary weights; mode2 removes native input2. Component/controller
state uses 20bytes (magic/version/f64 gain/u32 mode) inside the existing host state
envelope. Delay storage is fixed per instance with no process allocation. Fresh
worker state entry reconstructs the instance and clears delay.

Modes10/11 retain those lifecycle, gain and latency contracts. Mode10 declares
Stereo Main, Mono input1 and Stereo input2; mode11 declares Mono Main, Stereo
input1 and Mono input2. Output0 follows Main, and a one-channel input contributes
to channel0. Native BusInfo, Speaker Arrangements and ProcessData widths must
agree. The mixed-input gate checks independent complete DAG PCM, state restore,
44.1/48 kHz, both processing modes, block partitions and seek. Modes0–9 are unchanged.

Build outside the workspace with `cargo build --locked --manifest-path
crates/mondrian-audio/tests/fixtures/vst3-sidechain/Cargo.toml --target-dir
target/vst3-sidechain-fixture`. This is a qualification oracle, not a shipped
plugin or a copy of the product renderer. Native output qualification is pending.

Transport state 3 preserves default buses/delay and applies the signed timeline
gain modulation used by the CLAP fixture, keeping zero-input silence and finite
tail semantics. The native
component always requests all SDK context fields; only state 3 requires absolute
projectTimeSamples and prepared sampleRate, zero optional values and zero
optional validity flags. Requests alone cannot establish host fact availability.
States 0–2 retain their original DSP and bus behavior.

## Dynamic latency reference (state mode 4)

Modes 0 through 3 retain their previous sidechain, automation and transport
meaning. Mode 4 changes the configured delay/tail from three to seven samples
when Gain crosses 0.75. The controller sends an owning host-created IMessage
through the actual IConnectionPoint pair and calls
IComponentHandler::restartComponent(kLatencyChanged) on its initialization
thread. DSP keeps its current valid delay until the host deactivates/reactivates;
only that lifecycle changes the reported delay and clears the delay history.
Neither process() nor a background thread issues a restart callback.

The independent `installed_vst3_legal_latency_restart_reference` test proves the
request was acknowledged and checks exact PCM before/after the legal lifecycle
in realtime and offline modes. The product
`installed_vst3_latency_restart_invalidates_prepared_contract` test uses existing
APIs and is expected to fail because the current worker ignores the request at
the next window boundary. The complete trigger window remains legal under the
old active contract; it must finish once, including nested cache fills and
already advanced stateful sibling processors. The next root window must defer
before any DSP. A post-DSP opaque getState is not an exact checkpoint and cannot
authorize replay of the same unpublished window. An eventual production fix
must retain the child, reprepare the sole canonical audio plan/PDC and resume;
returning a fatal error alone is insufficient acceptance.


## Constant meter DSP oracle (mode 9)

Mode9 keeps the existing state envelope, ports and latency3. Actual IAudioProcessor
callbacks independently check anchored PPQ, Tempo80, explicit7/8, bar start and
validity flags; native bar/signature values contribute to PCM across boundaries.
Quarter zero is2/rate; samples−8 through3*rate at44.1/48k and1..16384 callback
frames cover Runtime, App preroll and Export. Endpoint checks precede PCM;
per-frame tags use no block-sized scratch and no FFI panic. Older modes retain
their contracts. This owned oracle does not qualify third-party or RT behavior.

## Constant component configuration service controls (modes 5/6)

Mode 5 initially activates with a three-frame delay. Actual DSP records desired
mode 6 without changing the active contract. One four-byte foreground DataExchange
block reports this real DSP change on the host's existing main-thread service,
including when all author lanes are None. Ordinary scalar synchronization can
also observe the component through ConnectionPoint; whichever arrives first
requests latency reactivation once, without manufacturing its profile. Legal
deactivate/reactivate realizes seven-frame delay/tail, and the actual saved
component/controller configuration restores that profile into independent fresh
instances. Unchanged host parameter delivery after the controller observed mode
6 must not generate another restart. These controls do not promise that generic
state captures DSP history or that a candidate is already installed into PDC.
