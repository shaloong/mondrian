# Official OpenFX Basic Gain qualification fixture

This project builds the official Basic Gain example from OpenFX commit
`e40728885390ec16276d11e00025de9b4282060c` as an external `.ofx.bundle`.
The checkout and built binary stay outside the product and Git tree. The
example's upstream BSD-3-Clause license and source remain with its checkout.
The fixture has no Conan dependency; it compiles only `Examples/Basic/basic.cpp`
and uses the official headers and `Info.plist`.

```sh
git clone https://github.com/AcademySoftwareFoundation/openfx.git /path/to/openfx
git -C /path/to/openfx checkout e40728885390ec16276d11e00025de9b4282060c
cmake -S tests/openfx_reference -B /path/to/build \
  -DOPENFX_SOURCE_DIR=/path/to/openfx
cmake --build /path/to/build --config Release
```

On Windows, point `MONDRIAN_OPENFX_REFERENCE_BINARY` to
`/path/to/build/Basic.ofx.bundle/Contents/Win64/Basic.ofx`; on macOS use
`Contents/MacOS/Basic.ofx`; on Linux use
`Contents/Linux-x86-64/Basic.ofx`. Then run:

```sh
cargo test -p mondrian-app --test openfx_host \
  official_basic_filter_renders_authored_float32_pixels_in_child -- --ignored --nocapture
```

The test verifies pinned discovery, parameter description, native Float32
rendering, authored per-component gains on a later frame with a non-square
pixel aspect ratio, preservation of negative/HDR/alpha values, and equality
between direct host and compiled effect-graph output.
The result qualifies this example and this host subset only; other vendors,
contexts, pixel formats, and controls require separate evidence.

Native reuse is qualified separately in a supervised test child:

```sh
cargo test -p mondrian-app --all-features --lib \
  official_basic_native_session_reuses_authored_frames_in_supervised_child \
  -- --ignored --nocapture
```

This checks repeated native session creation/destruction, same-frame parameter
edits, seeks, changing input/PAR, omitted-control defaults and poisoned-handle
retirement. It does not replace the product worker or cross-vendor matrix.

The build also creates a separate `SessionProbe.ofx.bundle`. Its small owned
wrapper includes the untouched official example from the external checkout;
upstream notices and license remain attached. This fixture checks render-action
flags and change-action grouping, advertises required/optional sequential or
temporal contracts, and can deliberately return success without producing a
new output. Neither fixture is packaged with Mondrian.

Set `MONDRIAN_OPENFX_SESSION_PROBE_BINARY` to its platform binary and run:

```sh
cargo test -p mondrian-app --all-features --lib \
  native_session_enforces_independent_frame_and_action_contracts \
  -- --ignored --nocapture
```

Seven supervised scenarios qualify normal and optional sequential execution,
required sequential and temporal rejection, instance-level admission changes,
runtime contract changes, and stale-output rejection. The probe independently
rejects any begin/render/end action falsely claiming sequential execution or
any parameter-change action outside a nonempty begin/end group.

Resident process/mapping reuse is checked by the `official_basic_resident_sessions_reuse_workers_and_isolate_frames`
integration test. Preview/Export test consumers keep different stable child
identities across edits/seeks and changing inputs; cancellation of one consumer
cannot retire the other. A separate native hang scenario uses the probe bundle:

```sh
# Set MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang in the test command environment.
cargo test -p mondrian-app --all-features --test openfx_host \
  resident_worker_cancellation_retires_and_reaps_a_hung_native_render \
  -- --ignored --nocapture
```

The latter interrupts a five-second native render and confirms the retired
session cannot be reused. It qualifies supervised cancellation for the session
API. Authored Effect cancellation and the controlled host-frame charge now
use the same execution-owned factory seam; physical native heaps and persistent
production residency remain separate qualification.

The normal and optional SessionProbe scenarios additionally hold two Source
image handles, release the first, then read and release the second before the
official Basic implementation fetches Source again. This checks action-scoped
input ownership and prevents image release from freeing session pixel storage.

Authored native factory qualifications (run separately because probe mode is
chosen by the test command environment):

```sh
# MONDRIAN_OPENFX_SESSION_PROBE_MODE=unfetched
cargo test -p mondrian-app --all-features --test openfx_host \
  authored_native_factory_reuses_instances_and_rejects_stale_output \
  -- --ignored --nocapture

# MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang
cargo test -p mondrian-app --all-features --test openfx_host \
  authored_native_factory_observes_mixed_execution_deadline \
  -- --ignored --nocapture
```

The reuse test also denies a one-byte-short frame grant before worker creation,
then retries successfully without quarantining the definition. A new generation
must get a fresh native instance after stale-output failure. The deadline test
executes the authored Filter through the prepared CPU-prefix route, rather than
calling the host session directly. Neither qualification requires a GPU device.
When running all ignored integration tests in `hang` mode, skip the
`authored_native_factory_reuses_instances_and_rejects_stale_output` test and
run it separately in `unfetched` mode.

The ordinary Float32 Session and Timeline compositor also carry the same owner:

```sh
# MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang
cargo test -p mondrian-app --all-features --test openfx_host \
  authored_native_factory_observes_scalar_and_timeline_owner_stops \
  -- --ignored --nocapture
```

This interrupts the five-second native action through the scalar Session with
an owner deadline, then through the Timeline compositor with the owner token.
Both produce an exact stopped result without quarantining the definition.

The lifetime probe now requires independent overlapping Source and Output image
handles, releases one and reads the other, then lets Basic fetch again. Native
handles borrow the action's read-only Source/writable Output mappings. No image
wrapper or native frame allocation is retained between actions. Output clears
on its first fetch; an unfetched frame cannot publish an old raster. Factory
admission now counts three host rasters, plus executor staging; the one-pixel
boundary is 79/80 bytes, with 48 bytes charged to host storage.

The same probe forbids Source fetches in Create/Destroy, BeginInstanceChanged
and Begin/EndSequenceRender, and exercises legal user-edited InstanceChanged
Source fetches. These checks cover actual action availability rather than only
pointer refresh. The `leak_source` fault probe deliberately leaves one Source
handle unreleased at Render return: the host rejects output and poisons the
session so a second render cannot run. The authored UHD test uses the official Basic bundle:

```sh
cargo test -p mondrian-app --all-features --test openfx_host \
  authored_uhd_filter_fits_professional_working_grant -- --ignored --nocapture
```

It rejects a one-byte-short 663,552,000-byte working grant, then renders the
complete unclamped frame under 768 MiB, including distinct corner samples.
