# Stateful VST3 Gain fixture

This isolated test crate adapts the permissively licensed `vst3-rs` Gain example
at the pinned revision in `Cargo.lock`. It adds component and controller state
serialization so the Mondrian host can be qualified against an audible
nondefault saved state. Its DSP consumes every VST3 parameter queue point
as a linear segment, including the previous block's final value at implicit
sample offset -1; missing queues hold that value. It allocates no processing
buffer and contains no Mondrian curve evaluator. The source is licensed MIT OR Apache-2.0; see
`LICENSES/VST3-RS-MIT.txt` at the repository root.

On Windows, build the fixture outside the Mondrian workspace:

```powershell
cargo build --manifest-path crates/mondrian-audio/tests/fixtures/vst3-stateful-gain/Cargo.toml --target-dir target/stateful-vst3-fixture --locked
$env:MONDRIAN_VST3_TEST_HELPER = (Resolve-Path target/debug/mondrian.exe).Path
$env:MONDRIAN_VST3_STATEFUL_TEST_PLUGIN = (Resolve-Path target/stateful-vst3-fixture/debug/mondrian_vst3_stateful_gain_fixture.dll).Path
cargo test -p mondrian-audio --lib stateful_vst3_snapshot_round_trips_and_restores_isolated_worker_audio -- --ignored
```

Build the `mondrian-app` binary first if `target/debug/mondrian.exe` is absent.
The fixture is not a distributed effect or a compatibility layer.

For sample-accurate queue qualification in both realtime and offline modes:

```powershell
cargo test -p mondrian-audio --lib installed_vst3_stateful_gain_preserves_linear_parameter_queues -- --ignored --test-threads=1
```

The signal oracle covers intermediate samples, an omitted sample-zero point,
last-sample changes, unchanged blocks and adjacent points describing a jump.
See the official `IParamValueQueue` contract:
https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParamValueQueue.html
