# CLAP sidechain signal fixture

Owned validation plugin `org.mondrian.fixture.sidechain`, implemented with the
pinned Clack SDK under MIT OR Apache-2.0. It is a separate binary from the
sample gain fixture and does not implement Mondrian routing or curve semantics.

Stereo DSP is `gain * (2 * main + 3 * input11 + 5 * input7)`, delayed by exactly
three sample frames. Native input IDs11 and7 deliberately differ from index and
share the same display name. State is five bytes: one mode followed by a finite
little-endian f32 gain in [0,1]. Mode0 orders inputs11,7; mode1 reverses them;
mode2 removes input7. Ports change only while deactivated during state load.
Reset clears the delay. The plugin advertises latency3 and finite tail3 while
activated; its latency callback returns an invalid sentinel before activation
to expose illegal host lifecycle queries.
Process buffers are allocated at activation, and every callback overwrites them.

```powershell
cargo build --offline --manifest-path crates/mondrian-audio/tests/fixtures/clap-sidechain/Cargo.toml --target-dir target/clap-sidechain-fixture
$env:MONDRIAN_CLAP_TEST_HELPER = (Resolve-Path target/debug/mondrian.exe).Path
$env:MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN = (Resolve-Path target/clap-sidechain-fixture/debug/mondrian_clap_sidechain_fixture.dll).Path
cargo test -p mondrian-audio --lib installed_clap_sidechain -- --ignored --test-threads=1
```

Build the current product first. This fixture does not qualify arbitrary vendor
plugins, other layouts, MIDI, native editors or other platforms.

Render and thread lifecycle qualification: states 0–2 retain their exact original
ports, gain and three-frame delay. State 3 exposes realtime gain/delay unchanged,
and offline gain multiplied by 1.5 with five-frame latency/tail. State 4 declares
a hard realtime requirement; state 5 declines offline mode. The fixture requires
the standard host thread-check extension and validates init, state, ports, render,
activation, latency, tail, callback, reset, start, process, stop and deactivation
roles. A background query is performed during main initialization; processing
never creates or joins threads. The fixture's render mode is an execution fact
and is not persisted in its five-byte authored state.

Transport state 6 retains default ports/latency and modulates gain by the signed
timeline coordinate before the delay: `gain + ((sample % 251) / 1024)`. Silence
stays silent and the finite tail remains valid. It requires seconds timeline validity only, absent steady counter,
and zero unavailable musical, loop and transport fields. It independently
decodes CLAP's 2^31 seconds factor to the prepared integer sample grid; no
product converter is linked. Other states 0–5 retain their original meaning.
