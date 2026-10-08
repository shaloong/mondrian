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

The same UHD case now uses the nominal Professional eight-instance/384 MiB
optional native grant, seeks with reversed Source data across generations,
checks both complete unclamped outputs and resident frame charge, then trims
to zero. The one-pixel unfetched probe also changes frame generation before
its required second-call failure, detecting accidental worker restarts.

Choice qualification uses MONDRIAN_OPENFX_SESSION_PROBE_MODE=choice and the ignored integration test choice_filter_preserves_canonical_indices_and_native_pixels. The owned wrapper declares duplicate captions with extreme signed ChoiceOrder ranks; its native Render reads the authored original index through paramGetValueAtTime, as required in Render actions. The test compares per-index Float32 output against expected gains and exercises the same choice through the compiled effect graph. This qualification does not establish StrChoice or historical-time access.

The Choice scenario also checks invalid plugin writes in InstanceChanged,
where current-value access is legal, and verifies preservation of the current
index. A resident worker processes default/0/2/1/default edits without changing
process identity. Full parameter-suite semantics beyond this scenario require
separate qualification.

## String-valued choices

Set `MONDRIAN_OPENFX_SESSION_PROBE_MODE` to `strchoice` or `strchoice_default`
and run the ignored `string_choice_filter_preserves_keys_and_native_pixels`
integration test. The wrapper declares stable enum strings independently of
captions/ranks. Tests compare direct and compiled-graph Float32 pixels and
reuse one worker across edits/default restoration. Invalid plugin writes in
InstanceChanged must retain the current string value. The second mode omits
the default and requires the SDK-defined first enum.

For `strchoice_duplicate`, `strchoice_gap`, `strchoice_missing_enum` and
`strchoice_bad_default`, run the ignored
`malformed_string_choice_is_rejected_before_registration` integration test.
Each scenario must reject both description and effect registration before
any processor instance is authored. These fixtures do not establish historical
time access, dynamic option rebinding or the broader vendor matrix.

The Choice/StrChoice scenarios also declare static Double, Boolean and Integer
controls. They compare current values against past/future fractional-time queries
for all five scalar kinds and require NaN/infinity to return ErrValue without
publishing output values. This qualifies constant non-animatable snapshots;
potentially animated parameters still require separate complete temporal binding.

Static Double calculus is also checked through the actual parameter varargs
suite: zero derivatives; forward, reversed and equal endpoints; finite
endpoints with overflowing differences; zero/subnormal/signed/extreme values;
NaN/infinity admission and preservation of outputs on failure. The Rust matrix
reuses one resident worker and compares pixels, then exercises the compiled
graph. Animatable Double calculus must return MissingHostFeature until a
complete Core-backed temporal binding is implemented.


## String values

Set `MONDRIAN_OPENFX_SESSION_PROBE_MODE=strings` and run the ignored
`string_filter_description_preserves_all_native_modes` integration test. The owned
wrapper declares eight String controls: all six SDK modes, with both existing/new
policies for file and directory paths. Native Render reads exact UTF-8 bytes through
`paramGetValueAtTime` and derives a gain from their lengths, so the oracle compares
actual Float32 output against the authored snapshot rather than a transport echo.

The same worker processes defaults, Unicode/multiline/empty edits, a 64 KiB value,
and default restoration. The plugin probes finite past/future static reads, rejected
nonfinite times, null/oversized setters and preservation of caller output/value.
Rejected host snapshots must leave the worker reusable. Native metadata support
is separate from product editing: Label/RTF controls currently reject effect
registration until their faithful authoring contracts are implemented.


`strings_text` with `string_text_modes_execute_through_authored_compiled_graph`
qualifies SingleLine/MultiLine presentation metadata and authored empty/multiline
values through the compiled effect graph. Successful plugin writes are exercised
in CreateInstance, before SDK change notification becomes active; invalid writes
are exercised inside InstanceChanged without introducing recursive edits.

`strings_paths` declares only the two text controls and four file/directory
controls. To qualify native paths through the application, set
`MONDRIAN_OPENFX_PRODUCT_BINARY` to the current `mondrian.exe`,
`MONDRIAN_OPENFX_SESSION_PROBE_BINARY` to the built SessionProbe binary and
`MONDRIAN_OPENFX_SESSION_PROBE_MODE=strings_paths`, then run:

```powershell
cargo test -p mondrian-app --all-features --lib native_path_resource_draft_cancel_undo_save_reopen_preserves_output_pixels -- --ignored --nocapture
```

This checks external-only resource contracts and their UTF-8 bounds, native
open-picker service routing, cancellation, typed new paths, one-step Undo/Redo,
clearing to the empty SDK String, Unicode `.mdp` save/reopen and real
compiled-graph Float32 pixels.
The picker service is simulated; physical OS dialogs and network-path response
time require separate qualification. No selected file or parent directory is
created by selection, cancellation or the owned render probe.
Existing input bytes must remain unchanged; a save-dialog sentinel detects
accidental routing to an overwrite-oriented picker. Localized input hints state
whether a path must exist or may be new.

Changed paths with an existence policy are inspected by a separate product
process before the author transaction. The path acceptance above uses the actual
current product helper for this step, followed by the actual SDK render worker.
The UI remains cancellable while inspection is pending. Stale, timed-out and
cancelled results cannot commit a draft.

To qualify the inspection adapter separately, point
`MONDRIAN_RESOURCE_INSPECTION_HELPER` at the current product executable and run:

```powershell
cargo test -p mondrian-app --all-features --lib actual_product_helper_preserves_native_paths_and_reports_local_objects -- --ignored --nocapture
cargo test -p mondrian-app --all-features --lib resource_inspection_actual_host_commits_only_after_the_owned_helper_finishes -- --ignored --nocapture
```

The helper must be rebuilt with the resource inspection entrypoint; older product
binaries do not implement this mode. Supervisor fault tests use
`MONDRIAN_RESOURCE_INSPECTION_TEST_PYTHON` pointing to Python and run the ignored
`supervised_process_cancellation_timeout_and_retry_reap_the_owned_child` and
`failed_helper_responses_are_bounded_and_leave_the_service_reusable` tests.
These exercise owned-process termination, bounded responses and retry. They do
not qualify physical network stalls or every platform's process termination.

### Constant and sampled parameter time queries

The owned compound probe also supports `compound_constant_animatable` and
`compound_frame_sample`. The former changes only the descriptor's animation
capability and requires off-frame constant reads, zero derivatives and signed
integrals. The latter requires current-frame values to remain readable while
unprovided off-frame values and calculus fail without changing output pointers.
Run the ignored `native_animatable_constant_parameters_support_off_time_queries`
and `native_frame_sample_parameters_reject_unprovided_curve_queries` App library
tests with `MONDRIAN_OPENFX_SESSION_PROBE_BINARY` set to the rebuilt owned bundle.
These qualify explicit binding authority; they do not qualify animated curve
snapshots, native key enumeration or arbitrary third-party plugins.

The integration targets `product_worker_preserves_animatable_constant_query_authority`
and `product_worker_preserves_current_frame_query_authority` exercise those same
SDK assertions through the actual product executable and resident protocol.
Set the corresponding probe mode before each target. They send all six compound
bindings and verify exact output and worker reuse across two renders at frame zero.
They do not qualify seeking with animated snapshots.

Both query-authority modes also assert SDK key count, indexed lookup and search
semantics. Constant bindings report zero keys, BadIndex for every indexed lookup,
and Failed/-1 for searches in either direction. Sample-only bindings retain output
sentinels and report MissingHostFeature. Nonfinite searches reject with Value and
retain the index sentinel. These are empty/snapshot-absent contracts, not animated
key enumeration qualification.


### Complete-selection interpolation history probe

`curve_group_interpolation_authoring` extends the same owned fixture family as
`curve_group_authoring` (time) and `curve_group_value_authoring` (value). Keys stay
at frames 0/6/12 with values 1/2.5/3, beginning as Linear. One stable-ID group
request applies Auto or Continuous. Every SDK key/value query must retain those
exact coordinates. The fixture samples frame 3 for output, independently checks
the derivative by symmetric value queries, and integrates with Simpson sections
split at exact keys and the clamped tail. No quadrature tolerance is relaxed.
Continuous constraint changes need not change otherwise collinear geometry.

After rebuilding the current product and SessionProbe fixture, set the product
and fixture binary variables described above and the mode to
`curve_group_interpolation_authoring`, then explicitly run:

```powershell
cargo test -p mondrian-app --all-features --lib native_effect_group_interpolation_undo_save_reopen_preserves_queries_and_output -- --ignored --test-threads=1
```

The test also checks complete-set refusal, one author generation/Undo, saved
undone state retaining live Redo, and Unicode archives reopened in fresh sessions
with empty history and identical shared-graph native pixels. Time/value sibling
probes must still pass after fixture changes. This fixture mode describes the
intended acceptance; it is not itself a receipt of a successful run.

### Joint time/value history probe

`curve_group_transform_authoring` admits only the original 0/6/12-frame,
1/2/3-value snapshot or the jointly translated 0/12/18-frame, 1/2.5/3.5-value
snapshot. A partial axis update fails the SDK fixture. The existing independent
finite-difference derivative and split Simpson integral checks apply unchanged.
The App test `native_effect_group_transform_undo_save_reopen_preserves_queries_and_output`
uses one canonical TransformMany request for Auto and Continuous constraints,
checks collision refusal without value mutation, and verifies native pixels,
one Undo, saved live Redo and fresh Unicode reopen. Explicitly rebuild product
and fixture and execute this ignored test before claiming qualification.

### Declared-default reset history probe

`curve_reset_authoring` accepts the original constrained three-key snapshot or
an unkeyed declared scale default of 1. SDK queries verify zero keys, unchanged
out-of-range key-time outputs, failed-search index -1 in all three directions,
constant off-frame values, zero derivative and exact integral. Curve SDK writes
remain rejected; a reset constant accepts unchanged instance-local initialization
without authoring the Project. Native output samples frame 3, so reset must
change actual pixels.
The App test `native_effect_parameter_reset_undo_save_reopen_preserves_queries_and_output`
uses the existing parameter write action for Auto and Continuous, rejects locked
and stale targets and redundant reset, restores complete controls/IDs with one
Undo, saves the undone state retaining live Redo, and reopens both Unicode
archives in fresh sessions. Rebuild product and fixture and explicitly execute
the ignored test before claiming qualification; the four group sibling probes
must remain green.


The CPU admission/geometry probe modes are `host_capabilities`,
`canonical_progressive`, `cpu_unsupported_descriptor`, `cpu_unsupported_create`,
`cpu_unsupported_change`, `bootstrap_required`, `bootstrap_skip`, and
`bootstrap_error`. They inspect actual host/native action properties and enforce
bootstrap ordering, progressive fields, non-square canonical project coordinates
and refusal before CPU rendering. Coordinate-sensitive asymmetric HDR/alpha
pixels are compared with an independent reference through resident and compiled
graph execution. The untouched official Basic has no OfxSetHost export and
continues to qualify optional-export absence independently. These probes do not
claim the full GPU/context/vendor/performance matrix.

## Mixed API binary admission

`mixed_api` exposes unknown API entries around image API v2 and the actual Filter
at original export index 2. Unsupported callbacks abort if reached. Run
`product_worker_skips_unsupported_apis_and_preserves_original_export_indices`
with this mode to qualify discovery, native description, repeated resident
teardown, same-frame edits/seeks and the sole compiled graph against the
independent gain oracle. `unsupported_api` exposes only unsupported entries;
`product_discovery_admits_an_empty_supported_api_intersection` qualifies a
successful empty discovery and refusal to describe a nonexistent supported Filter.

## Negotiated output alpha

The owned `output_alpha` wrapper defines a static Choice that negotiates Straight,
Premultiplied and Opaque output through GetClipPreferences. Source metadata must
stay Straight; Output clip and fetched image metadata must match the preference.
The resident/compiled-graph test exercises same-frame representation edits,
seeks, repeated session teardown, negative/HDR RGB, zero coverage and positive
tiny alpha against an independent pixel oracle. Raw Straight signed/over-one
alpha is preserved. Separate failure modes require refusal of invalid metadata,
nonfinite output, finite division overflow, zero-alpha emission and Premultiplied alpha outside the
canonical normalized-coverage domain; this last domain is a product adapter
limitation, not a normative OFX Float restriction. Any error retires the worker
without exposing staging output. These tests are not a performance receipt.

## Actual resident CPU performance gate

`official_basic_production_resident_steady_frame_performance_gate` is an ignored
release-only test against the actual product resident worker and untouched
official Basic bundle. It requires explicit dimensions, FPS numerator/denominator,
per-effect frame-budget percentage, excluded warmup and sample count. Its public
render timer includes validation, fingerprints, copies, native work, IPC and
returned raster allocation; setup/shutdown are separately recorded. Every frame
has an independent asymmetric HDR/negative/alpha gain oracle outside the timer,
and a changing source pixel prevents stale-output success. Record raw samples,
p50/p95/p99/max and require zero stage-budget misses and zero render/oracle errors.

Use the sole `scripts/validation/collect-native-perf-context.py` schema-v1 receipt
with the actual OpenFX worker/reference/test-executable triple, completed release
build commands/transcript and optional external official Basic source receipt.
Set `MONDRIAN_NATIVE_PERF_CONTEXT`, `MONDRIAN_PERF_WORKTREE`,
`MONDRIAN_OPENFX_REFERENCE_BINARY`, `MONDRIAN_OPENFX_PERF_WIDTH`,
`MONDRIAN_OPENFX_PERF_HEIGHT`, `MONDRIAN_OPENFX_PERF_FPS_NUM`,
`MONDRIAN_OPENFX_PERF_FPS_DEN`, `MONDRIAN_OPENFX_PERF_STAGE_BUDGET_PERCENT`,
`MONDRIAN_OPENFX_PERF_FRAMES`, `MONDRIAN_OPENFX_PERF_WARMUP` and a unique
`MONDRIAN_PERF_OUTPUT`. Do not set any SessionProbe mode. Current source hashes,
Cargo.lock/build transcript and selected artifact hashes must match the receipt.

This is a hot-buffer public-host baseline, not uninterrupted transport throughput:
the complete correctness oracle touches both buffers between samples. Full NLE
performance acceptance also needs paced decode/effects/composite/present, sustained
memory/CPU and seek/cancellation latency on declared hardware. A 100% stage share
reserves no frame budget for other pipeline stages. No measurement is established
by adding this ignored test, a debug build or a changed budget after a failure.

## Early image memory and unknown long API qualification

`bootstrap_memory` calls the actual exposed ImageEffect suite during OfxSetHost,
allocates host-owned image memory with a null instance handle, locks/writes/checks/
unlocks/frees it, then follows the normal bootstrap count/get sequence. The source
test repeats fresh inspection/teardown in a supervised child; public integration
also checks native resident and sole compiled graph parity. `mixed_api_long`
exports an unsupported printable API name longer than 63 bytes at index 0, an
unsupported ImageEffect API version at 1 and the real Filter at original index 2.
All unsupported callbacks abort if reached. The fixture has distinct identifiers.
Source inspection and final public native/graph tests must remain GREEN after
the original API bound/native-header rejection RED. UTF-8 API naming is not
asserted as a normative requirement by this fixture.
