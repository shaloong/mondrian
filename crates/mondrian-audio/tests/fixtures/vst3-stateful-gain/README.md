# Stateful VST3 Gain fixture

This isolated test crate adapts the permissively licensed `vst3-rs` Gain example
at the pinned revision in `Cargo.lock`. It adds component and controller state
serialization so the Mondrian host can be qualified against an audible
nondefault saved state. The source is licensed MIT OR Apache-2.0; see
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
