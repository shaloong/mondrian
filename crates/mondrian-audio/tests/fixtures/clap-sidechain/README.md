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


## Deferred core restart (state mode 7)

Modes 0 through 6 retain their existing binary state and DSP meanings. Mode 7
adds one validated zero/one byte recording that a core restart has been
requested. During process, the first block covering canonical input-signal
sample nine requests restart on the audio role. The entire completed block
still uses its active three-frame delay and exact existing sidechain PCM.
Only a subsequent activation installs seven-frame delay/tail; the latency
query does not prematurely advertise the requested future value. The persisted
fixture flag is a known test control state, never a claim that generic opaque
plugin state is an exact DSP-history checkpoint.

`installed_clap_process_restart_preserves_completed_trigger_window` uses the
current production isolated worker and existing host API. Its real RED is
loss of the completed trigger block when the worker turns a valid core request
into a fatal service_callbacks error before copying output. The next window
must instead defer before DSP. `installed_clap_restart_activation_profile_pcm`
independently checks actual native mode/state activation profiles and exact
three/seven-frame PCM in realtime and offline modes. It does not qualify the
future canonical reprepare/continuity implementation.


## Retained configuration control edge cases (modes 8/9)

Modes 0 through 7 retain their previous state and PCM meaning. Modes 8/9 use the
same three-to-seven-frame activation profile and persisted desired flag as mode
7. Mode 8 issues another legal core restart during actual getState after the
new profile is active. The original request cohort cannot consume this same-bit
new generation. Mode 9 deliberately ignores a failed 70 KiB output-stream write
and returns success; the host writer must independently refuse the truncated
configuration and retain a blocked live child. Neither control automatically
retries or installs a new plan. Scalar fixtures and PCM references remain
independent of the host's conversion and lifecycle implementation.

## Standard host-log producer (state mode 10)

Modes 0–9 retain their exact state/DSP meanings. Mode10 requires the standard
Host Log extension during actual activation and emits all seven official
severities, in order, from activation, one joined background thread, and each
process callback. The independent original sidechain PCM and latency3 remain
unchanged. The process callback allocates, waits and spawns no threads.
These are legal log callbacks; separate defensive controls for invalid UTF-8,
null strings and unknown signed severities are not conforming-vendor evidence.


## Finite echo history (state mode 12)

Modes 0–11 retain their existing PCM, state and extension contracts. Mode12
keeps the same ports and five-byte mode/gain configuration, but has latency0
and finite tail256. After the original sidechain mix and gain, its native DSP
is `y[n] = x[n] + 0.5*x[n-256]`. The stereo ring has 2048 bytes of sample data
plus its cursor and allocation; reset and a fresh activation clear it. State
does not include the ring. The local impulse and cold/reset controls exercise
the same DSP helper used by actual native processing.

The ignored App selector
`native_clap_playing_host_fader_preserves_finite_echo_history` uses the actual
Product fader action and software Output. It requires an observed postcommit
anchor A >= 256 and checked A+256 <= the old completed public frontier F; the
fixed workload does not guarantee this on every machine. `NOT_COVERED_ECHO_HISTORY`
fails the gate when that prerequisite is absent. With complete old/new windows,
the first 256 new samples are checked against both old actual PCM scaled by the
new Host fader and the independent absolute-sample echo formula. Only
`HISTORY_DISCONTINUITY`, with all other checks and cleanup complete, identifies
that finite output-history failure. It does not prove a private ring/reset
cause, exact create counts, native migration, arbitrary tails or hardware audio.
