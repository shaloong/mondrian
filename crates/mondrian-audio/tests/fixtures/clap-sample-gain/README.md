# Sample-accurate CLAP gain fixture

This independent native test plugin adapts the Clack gain example at revision
`27ca28391b57a61bad4bb5099288a7bf9247350d` (MIT OR Apache-2.0;
see `LICENSES/CLACK-MIT.txt`). Its distinct plugin ID is
`org.mondrian.fixture.sample-gain`. Upstream source and binaries remain unchanged.

The DSP applies each parameter event only to its SDK event batch sample range.
It carries the last gain into blocks with no events, supports stereo input/output
and saves one little-endian f32 gain. It does not implement Mondrian curves:
the external ABI event offsets independently determine the output signal.

The upstream example applies every batch gain to the whole buffer. Static state
and single-event tests remain useful for that example, but it cannot qualify
sample-accurate multi-event automation. This fixture supplies that signal oracle.

```powershell
cargo build --manifest-path crates/mondrian-audio/tests/fixtures/clap-sample-gain/Cargo.toml --target-dir target/clap-sample-gain-fixture --locked
$env:MONDRIAN_CLAP_TEST_HELPER = (Resolve-Path target/debug/mondrian.exe).Path
$env:MONDRIAN_CLAP_SAMPLE_TEST_PLUGIN = (Resolve-Path target/clap-sample-gain-fixture/debug/mondrian_clap_sample_gain_fixture.dll).Path
cargo test -p mondrian-audio --lib installed_clap_sample_gain_preserves_event_ranges -- --ignored --test-threads=1
```

Build the Mondrian executable first. This is a validation plugin, not a bundled
production effect or a claim of arbitrary vendor plugin qualification.

On Windows the same plugin additionally supports only a fixed 320¡Á100 Win32
embedded GUI. The real child is titled `Mondrian CLAP Gain Editor`; its
per-instance class begins `MondrianClapGainEditor_`. The supplied parent HWND is
borrowed, must be on the initializing main thread, and must outlive GUI destroy.
The fixture owns and destroys only its child and class. Floating windows,
resizing and scaling are explicitly unsupported. Click the child to focus it;
one non-repeated `WM_KEYDOWN G` sets the original Volume to 0.75 and calls the
standard host parameter `request_flush`. `value_to_text` still returns `75.00 %`.

Main/audio parameter flush and process output share one bounded pending
Begin/Value/End gesture. A busy gesture ignores another G. Output capacity
refusal retains the exact undelivered phase, including End; main requests a
flush retry, while audio requests a main callback which then requests the flush.
No heap or mutex is used by this transport. The existing sample-range DSP,
parameter ID/range/default and four-byte state format are unchanged. A native
teardown failure that would leave a live DLL callback is a fixture process
failure, never successful cleanup. These source controls do not establish GUI,
real-time, PCM or host integration qualification before matching native runs.
