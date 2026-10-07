# CLAP sidechain signal fixture

Owned validation plugin `org.mondrian.fixture.sidechain`, implemented with the
pinned Clack SDK under MIT OR Apache-2.0. It is a separate binary from the
sample gain fixture and does not implement Mondrian routing or curve semantics.

Stereo DSP is `gain * (2 * main + 3 * input11 + 5 * input7)`, delayed by exactly
three sample frames. Native input IDs11 and7 deliberately differ from index and
share the same display name. State is five bytes: one mode followed by a finite
little-endian f32 gain in [0,1]. Mode0 orders inputs11,7; mode1 reverses them;
mode2 removes input7. Ports change only while deactivated during state load.
Reset clears the delay. The plugin advertises latency3 and finite tail3.
Process buffers are allocated at activation, and every callback overwrites them.

```powershell
cargo build --offline --manifest-path crates/mondrian-audio/tests/fixtures/clap-sidechain/Cargo.toml --target-dir target/clap-sidechain-fixture
$env:MONDRIAN_CLAP_TEST_HELPER = (Resolve-Path target/debug/mondrian.exe).Path
$env:MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN = (Resolve-Path target/clap-sidechain-fixture/debug/mondrian_clap_sidechain_fixture.dll).Path
cargo test -p mondrian-audio --lib installed_clap_sidechain -- --ignored --test-threads=1
```

Build the current product first. This fixture does not qualify arbitrary vendor
plugins, other layouts, MIDI, native editors or other platforms.
