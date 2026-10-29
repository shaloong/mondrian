# OpenFX catalog child and publication ownership

The catalog resolves the packaged product worker through the existing required
worker resolver. `MONDRIAN_OPENFX_WORKER_PATH` is an explicit override: an absent
configured file or relative configured path is an error. Cargo libtest and example executables resolve the
product sibling in the profile directory; their own executable does not implement
the product's discovery/describe dispatch. Qualified execution retains its existing
approved helper binding.

One catalog worker owns each direct discovery or description child. Its catalog's
single cancellation flag is checked before native launch, during 64 KiB identity
hash reads, during child polling, and before returning prepared metadata. An
accepted cancellation, native deadline, or observation error kills and waits for
the direct child. A guard also attempts settlement if a checkpoint panics. A
failed OS termination is reported explicitly; a still-running child is not
claimed settled and is not waited on indefinitely after a refused kill. These
contracts do not attest plugin-created descendants or OS/process failure cleanup.

Catalog destruction signals cancellation, closes its request sender to wake an
idle receiver, and joins its worker. This prevents a worker's supervision deadline
from disappearing when the App/libtest owner exits. Foreign calls execute only in
the isolated child and do not delay shutdown until their 10/30 second deadlines.
Disk I/O, process termination and OS scheduling have no hard latency guarantee;
this is not a real-time or bounded UI shutdown qualification.

The worker prepares immutable `EffectDefinition` values without modifying the
global effect registry. Only `AppState::poll_openfx_catalog`, while owning the
live catalog, publishes those definitions. Dropping unpolled results discards
them. Definition publication and successful machine-local install persistence
remain separate from project authoring; no second parameter or author state is
introduced. The existing public direct registration API remains available for
explicit callers.

## Evidence and remaining qualification

On 2026-10-11 a fresh standalone Rust control entered a real direct child before
requesting cancellation. The previous child-polling behavior returned Deadline
instead of owner cancellation: 1 passing control and 1 failing control. The
saved baseline is `.scratch/ofx-supervision-red.rs`; its executable and log are
beside it. This reproduces the missing cancellation seam, independently of the
historical r581/r582 App child receipts.

The repaired standalone supervisor passed four controls: cancellation,
checkpoint panic, deadline, and successful/nonzero native exit. All started
children were observed settled through their actual `Child` owners. Two ignored
test functions are executable child fixtures invoked by the passing controls.
The matching `.scratch/ofx-supervision-green-v2.log` records the final narrow run
after independent review corrected the failed-kill boundary.

Catalog idle shutdown, discard-before-poll, live-owner publication, canceled
discovery before launch, chunked-hash cancellation and canceled definition
preparation have additional crate regression tests. Their presence is not an
execution claim. App/Cargo integration and actual OpenFX discovery/describe
qualification is separately recorded in
`.scratch/plugin-host-sprint/ofx-native-execution-r3/summary.json`: all 15 selected
groups passed using fresh App/helper artifacts and official Basic plus the owned
SessionProbe. The two actual AppUiHost Drop controls entered blocking native
discovery/describe, retained process/thread handles and required both to signal
exit with no registry/preferences publication. First-run Drop observations were
19/22 ms; these are finite observations, not a latency guarantee. The frozen
source/artifact receipts and independent review define the exact scope. Ordinary
App regressions and original startup/restoration/relink workflows remain separate
gates. No full third-party plugin,
native GPU, platform-wide compatibility, or performance claim follows from the
standalone supervisor controls.
