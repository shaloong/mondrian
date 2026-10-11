# Small Adjustment Mix GPU cost diagnostic

This is a bounded diagnostic for the current finite Adjustment batch. It is
not a realtime, HD/UHD, release-host or complete NLE performance gate. Its
ignored test uses the production `GpuContext`, compositor, compiled point
plan, public record functions and actual queue submission/completion. It
requires a discrete or integrated hardware GPU and the Float32 working route;
it never substitutes a software adapter, skipped test or empty submission.

## Workloads and timing

Run **16x16 first**. Read its full report before explicitly requesting 64x64.
There is no automatic HD/UHD expansion. Stop after a slow, failed or hung
small cohort; a supervisor killing the host cannot promise GPU preemption.

For one compositor, record ordinary Normal Over before any Mix; assert that
the lazy Mix pipeline is still absent. Time the production lazy initializer's
first module/pipeline creation separately, then first canonical inline Mix
dispatch. The same owner subsequently records common inline Mix, an exact
finite HDR Coverage cancellation case, and the same ordinary Over cohort
again. The common inline path materializes its actual compiled ColorAdjust
Exposure result. The adversarial two-texture path uses the already-qualified
fixed rational `RGB=-0.25, A=2^-74` oracle (and its sign reversal). These are
distinct actual routes, not a direct-texture substitute for inline effects.

Each cohort has 2 excluded warmup calls and 8 retained samples. Real input
textures change sign/scale each call, and their materialization/completion
finishes before timing. The timer includes the public record call, its output
allocation, queue submission and proved GPU completion. Encoder creation,
input preparation, graph compilation, output readback/map, independent pixel
checks and completed resource destruction are excluded and named in the
report. Readback verifies every pixel after each timer. This cost excludes
App/IPC/full-NLE scheduling and must not be compared directly with their
whole-call frame budget.

Reports retain all samples, nearest-rank p50/p95/max, known texture counts and
bytes, adapter/backend information and actual host profile. Texture bytes
do not measure driver allocations, shader register spills or total RSS.
First use is cold for this compositor only; no cleared driver-cache claim is
made. Ordinary before/after costs isolate whether this owner incurs a change
after first Mix. A ratio alone does not prove no regression across drivers.

The current finite batch uses the existing **test debug** host artifact. On
Windows MSVC x86_64 the static native compiler archive remains upstream
`ReleaseFast`; this is separate from Rust host optimization. The GPU module
uses that supported default compiler. Do not rebuild `--release` solely to
run this small gate or describe a debug-host elapsed time as release-host
throughput. No arbitrary PASS threshold is added; preserve the measurements
and inspect cost before selecting further work.

## Root execution recipe

Apply the complete correctness/resource/opacity tail and this fixture, then
format and build one matching renderer library test artifact. No application
bin or diagnostic companions are needed:

```powershell
cargo test -p mondrian-renderer --all-features --lib gpu_adjustment_mix_small_cost_qualification --no-run --message-format=json
```

Select the `compiler-artifact` whose target name is `mondrian_renderer`, kind
`lib`, profile test is true and executable is non-null. Preserve the exact
build log, source snapshot, selected executable hash and the native compiler
archive/library receipt. Source hashes that remain stable during execution
alone do not prove that an arbitrary old executable was built from them.

Run that exact test executable with a new output file and direct owned log
files; no PowerShell `*>` inherited-pipe wrapper is required:

```powershell
$env:MONDRIAN_GPU_MIX_COST_SIZE = '16'
$env:MONDRIAN_GPU_MIX_COST_OUTPUT = 'E:/dev/mondrian/target/perf/gpu-mix-16.json'
& $testExecutable --exact gpu_compositor::tests::gpu_adjustment_mix_small_cost_qualification --ignored --nocapture --test-threads=1
```

The report file uses `create_new`. A second execution must select a distinct
new file and explicitly set size 64. The source-only scratch runner supplied
with the proposal accepts `--worktree`, `--test-executable`, `--output-directory`
and `--size 16|64`, opens owned stdout/stderr files, supervises the one process
for 300 seconds, verifies exactly one executed test and rehashes the selected
sources/executable. It never downloads, builds or expands dimensions. GPU
submission and mapping waits each have a finite 10-second bound. Those are
failure cleanup bounds, not throughput acceptance limits or a revised frame
budget. Failures preserve logs and prevent later dispatches in the test.

No source-only fixture, ignored test declaration, static compiler download or
passed pixel gate is a measured performance result. A report becomes actual
evidence only with the matching-source/native build and executed-test receipt.
