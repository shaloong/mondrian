# Owned VST3 native editor fixture

This MIT/Apache-2.0 fixture uses the workspace's canonical `vendor/vst3-sdk`
bindings. It has its own workspace and does not change the existing sidechain
fixture or its candidate/reprepare modes. Its component class is
`C48B1B724CDE4A91A2C9C4EBB7DD1E83`; its controller class is
`48CE5E916FEC4D4ABDA302D1D70C2DE9`.

The Windows `IPlugView` creates a real child HWND using a fixture-owned window
procedure. It shares the actual loaded controller's state and retained component
handler. Posting Space or a mouse press produces a Gain Begin/Value/End gesture.
Posting G produces one ordered group containing Gain=0.75, Balance=0.25 and an
opaque controller configuration change with a dirty callback. Messages execute
on the actual native window thread. Diagnostic read-only parameters remain
separate from the authored gain/balance/configuration and are not serialized.

The component has one stereo input/output bus, zero latency and no tail. Its
sample-accurate input parameter queues drive Gain and Balance. With both input
channels equal to one, the grouped native edit produces exactly 1.125 on the
left and 0.375 on the right. There are no component-handler calls from DSP.

Fixture failure scenario parameter 2 is a non-automatable selector:

| Normalized value | Scenario |
|---|---|
| 0 | Normal attach, resize and close |
| 0.5 | First `removed()` refuses; the host must retain the live child and never repeat that callback |
| 1 | `removed()` succeeds, then `setFrame(null)` refuses once |

Read-only parameter IDs 100 through 107 expose attach count, removal attempts,
IPlugView focus calls, native window focus messages, resize calls, native input
messages, completed native edits and wrong-thread controller calls. Counts are
normalized by 1024 and saturate at one. Native focus messages and IPlugView
`onFocus` calls are distinct; the first does not prove the host forwards the
second. The real window class is unregistered after its final child is removed
so a loaded DLL's window procedure cannot remain registered after unload.

## Qualification

Build this package only after applying it at its repository-relative path:

```powershell
cargo build --release --manifest-path crates/mondrian-audio/tests/fixtures/vst3-editor/Cargo.toml --target-dir target/native-editor-fixture
$env:MONDRIAN_VST3_EDITOR_FIXTURE=(Resolve-Path target/native-editor-fixture/release/mondrian_vst3_editor_fixture.dll).Path
cargo test --manifest-path vendor/vst3-host/Cargo.toml --no-default-features --test mondrian_owned_editor -- --ignored --nocapture --test-threads=1
```

The matching vendor stage must contain the sole same-instance editor/processing
roles and native attachment state machine. Four ignored tests exercise a real
loaded native IPlugView, Windows message dispatch, ordered callback source,
controller configuration roundtrip, resize constraints, two close failures and
actual separate-thread DSP using the same component/controller. Missing input
artifacts are a test failure, not a successful skip.

The three control-only gates explicitly stop processing and borrow the same
controller. The separate DSP gate starts processing before borrowing both roles;
GUI access does not imply that a processing scope is active.

Every gate uses an actual native test child process. The removal-failure gate
checks that repeated host close remains refused without a second native
`removed()` call, then retires the child without unloading an attached view's
DLL. Only a successful removal followed by failed frame cleanup can retry
`setFrame(null)`. A child panic exits immediately before native owner unwinding.

These tests are source-only until executed. They do not establish product child
IPC/installed-owner/App authoring integration, physical GUI usability, native
assistive technology, real-time scheduling, resource budget or performance
qualification. Those require the subsequent actual product consumer gates.
