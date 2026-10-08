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
