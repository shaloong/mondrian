# Reference Validation

This system makes media correctness and performance claims reproducible. It does
not make a capability `Verified` merely because a file, decoder, shader, or UI
control exists.

## Contracts

- `tests/validation/corpus-manifest.json` owns immutable fixture IDs, paths,
  provenance, identity strategy, declared media properties, and intended test
  purposes. Fixed files pin bytes globally; generated files pin the recipe and
  are byte-pinned by each run.
- `tests/validation/golden-project.json` defines the five-minute editing and
  export workflow. Schema v2 fixes exact Sequence raster/timing/color/audio
  values, fixture-role purposes, stable built-in delivery preset identities,
  resolved profile/depth/chroma/range/Alpha expectations, and independently
  executable evidence slices. It is the source contract for a generated `.mdp`;
  hand-written project JSON is not accepted as execution evidence.
- `tests/validation/stress-project.json` defines the 30–60 minute workload and
  stability thresholds.
- `tests/validation/windows-alpha-reference.json` defines the reference-machine
  class. A captured machine report is evidence for a run, not a modification of
  the reference profile.
- `tests/validation/playback-reference-gates.json` binds each professional
  playback gate to one fixture, required purposes, exact test entry point,
  environment binding, build profile, and expected structured report profile.

Fields whose value has not been verified must be `unknown` or `null`. Names and
camera folklore are not acceptable evidence for codec or color metadata. Before
a fixture drives a color golden, probe evidence and an independently trusted
reference frame or numeric patch values must be added.

External color references use renderer's versioned `ColorReferenceDescriptor`
contract. Each descriptor separates payload format (PNG, OpenEXR, or numeric
JSON) from pixel encoding (for example sRGB RGBA8, BT.2100 PQ float, or
scene-linear Rec.2020 float), and pins both the source stimulus and payload with
SHA-256. It also records producer/specification version, dimensions, alpha
semantics, reference white, and nominal peak where applicable. Import is
fail-closed before any tolerance comparison. `public_specification` and
`independent_application` are independent evidence;
`mondrian_regression` is intentionally not. This lets a commercial application
export be added later without claiming that Mondrian-generated goldens already
establish subjective parity.

The committed `mondrian-standard-quality-v1` numeric corpus is a separate
objective stimulus contract pinned to the Standard package digest. It covers 22
quality categories and drives the production CPU OCIO sRGB, Rec.709, Display
P3, HLG, and PQ boundaries. Every target gates finite output, normalized
display-signal tolerance of one 12-bit code, alpha preservation, neutral-axis
stability, tone monotonicity, high-saturation hue-boundary continuity, local
negative-channel continuity, and 10-bit ramp transition preservation. The
corpus also pins explicit 10-bit legal/full-range codes. ColorChecker 2005 xyY data comes
from Colour Science 0.4.7 under BSD-3-Clause with attribution in the corpus and
are converted from D50 xyY to linear Rec.2020 test stimuli before rendering.
Synthetic objective gates do not replace independent PNG/OpenEXR reference
frames or subjective review of skin, fabric, LED, neon, flame, and highlights.

## Asset classes

| Class | Repository | PR | Windows nightly/release |
| --- | --- | --- | --- |
| `committed` | Small and redistributable | Required and hash-checked | Required |
| `generated` | Recipe committed; result disposable | Recipe/contract checked | Generated and checked by its scenario |
| `local-restricted` | Never committed | Optional; checked when present | Required and hash-checked |

Generated output is not assumed to be byte-identical across FFmpeg or codec
library versions. The manifest fixes the recipe hash and semantic probe
contract. The adjacent attestation fixes the actual artifact, recipe, tools,
and probe; a Reference Playback Run repeats those hashes in its evidence
bundle. A generated workload fixture marked `color_reference_eligible: false`
cannot satisfy a color golden even when it carries valid CICP tags.

An execution slice is intentionally narrower than the complete Golden Project.
It passes only if its exact required fixture roles, operations, and content have
typed postcondition evidence. Golden v3 rejects unknown fields; requirement IDs
select evidence obligations but cannot substitute for observed author, media,
delivery, or persistence facts.
Every slice report explicitly records `complete_golden_project: false`. A
deterministic Golden Acceptance Plan compiles the top-level fixture, operation,
content, and export sets against all declared slices; the plan is diagnostic
structure, not execution evidence. The complete Golden status remains blocked
until one coordinated Project run observes every top-level requirement and the
release repetition count is satisfied.

Material whose redistribution is prohibited must not be uploaded to CI
artifacts, mirrors, releases, or public fixture bundles. Runtime downloads from
mutable or “latest” URLs are prohibited.

## Tiers and evidence

1. `Pr` validates schemas, stable IDs, hashes, and any locally present assets. It
   is fast and does not turn unavailable restricted media into a false failure.
2. `Nightly` requires the selected scope's local corpus on the dedicated Windows
   runner and runs real decode, seek, display/capability, Golden Project, and
   stress cases. `-Scope Playback` closes only the M0 Video/Audio playback plan;
   it does not waive missing Golden or Stress roles for an `All` run.
3. `Release` has the same asset strictness and additionally requires three
   consecutive Golden Project passes plus export/reimport and recovery evidence.

Run manifest validation:

```powershell
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Pr
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Nightly -Scope Playback
```

Inspect the current top-level Golden coverage ledger without generating media:

```powershell
cargo test -p mondrian-app --lib `
  golden_acceptance_plan_reports_current_top_level_blockers -j1 -- `
  --nocapture
```

The ledger deliberately remains blocked. The current slices do not plan AAC
audio, HLG Main10/Rec.709 H.264/sRGB Alpha picture roles; Play, accurate Seek,
Scrub, Insert, Overwrite, Ripple, Split, Proxy switch, and offline Relink;
primary color correction and LUT. The three picture roles also remain unbound
to qualifying fixtures. Export contracts are already assigned, but that alone
is not a complete product workflow.

Preflight a candidate reference machine before generating large media. The
machine ID is an operator-owned stable label, not a serial number or an
automatically harvested hardware identifier:

```powershell
pwsh -File scripts/validation/capture-windows-reference.ps1 -MachineId edit-bay-a
pwsh -File scripts/validation/validate-windows-reference.ps1 `
  -RequiredMemoryClass standard-playback `
  -RequireBaselineEligibility
```

The Windows Alpha memory contract has three explicit classes: 8 GiB is the
minimum supported memory class, 16 GiB is the standard playback/reference class,
and 32 GiB is recommended for large professional projects. Minimum-class systems
must remain correct, bounded, and capable of explicit proxy or reduced-quality
fallback, but are not required to satisfy native 4K Main10 real-time thresholds.
The M0 Video+Audio baseline requires the standard class; an 8 GiB machine can
run it only as an explicitly unqualified diagnostic. Qualification
uses the summed memory-module capacity, while the report also records OS-visible
memory, so firmware or integrated-GPU reservation does not falsely reject a
nominal memory class or hide memory actually available to the process.

Generate the disposable canonical workload media when needed:

```powershell
pwsh -File scripts/validation/generate-reference-playback-media.ps1 -Profile All -Force
pwsh -File scripts/validation/generate-golden-project-media.ps1 -Force
```

The Golden generator currently creates a 305-second, 48 kHz stereo PCM S16LE
stream in a MOV container with analytically different left/right signals. MOV
is intentional because it preserves the declared Front-Left/Front-Right layout;
two channels without a Stereo layout cannot qualify. It is sufficient to make
gain, pan, fades, channel swaps, import, and persistence observable; it is not a
color or acoustic-loopback reference. A non-`-Force` run only reuses an artifact
whose existing attestation matches the current recipe and artifact hash; it
never rewrites an old artifact's provenance.

Run the first Headless Golden execution slice:

```powershell
pwsh -File scripts/validation/invoke-golden-foundation-gate.ps1 `
  -RegenerateGeneratedFixture
```

The bounded runner invokes only `mondrian-app --lib` and records a structured
report for canonical fixture/attestation identity, semantic project open, PCM
import, exact five-minute placement, typed Clip gain/pan/fades, four Undo plus
four Redo steps, durable save, and fresh reopen. The report records exact
Session/Generation/Sequence Revision transitions, complete Sequence settings,
native Stereo layout, persistence request identity, archive identity, and
reopened typed author values rather than prose assertions. Its envelope always records
`complete_golden_project: false`; a passing foundation slice cannot be reported
as the M1 Golden exit gate.

Run the generated-picture delivery slice during development after placing the
generated PCM fixture under an untracked fixture root:

```powershell
pwsh -File scripts/validation/generate-golden-project-media.ps1 `
  -OutputRoot target/validation/golden-fixtures/large
$env:MONDRIAN_GOLDEN_FIXTURE_ROOT = "target/validation/golden-fixtures"
cargo test -p mondrian-app --lib `
  golden_project_generated_delivery_roundtrip_gate -j1 -- `
  --ignored --nocapture --test-threads=1
```

This slice executes a 25-frame work area rather than pretending that generated
color is a color reference. It proves ordinary Solid Color and PCM authoring,
exact Trim, Transform and Opacity, H.264 High 8-bit and HEVC Main10 10-bit
delivery, MP4 mux identity, Rec.709 CICP/range, absence of static HDR metadata,
48 kHz Stereo AAC, production queue terminal evidence, and reimported typed
codec/profile/pixel-format facts. It still reports a partial Golden slice:
missing real color-reference roles, playback, nesting, transitions, recovery,
and three complete consecutive runs remain open.

Run the fixture-free visual-authoring slice on Windows:

```powershell
cargo test -p mondrian-app --lib `
  golden_project_visual_authoring_roundtrip_gate -j1 -- `
  --ignored --nocapture --test-threads=1
```

This slice creates an exact adjacent generated edit, adds the product-default
Cross Dissolve, creates and edits a Basic Title, and authors two-key Hold,
Linear, and Bezier curves through production App Interfaces. Every edit,
Undo, and Redo must advance the installed Authoring Session by exactly one
Author Generation and Sequence Revision. The gate then performs production
durable save/close/fresh-open and repeats Headless Preview execution. Preview
uses the recursive timeline executor, real system-font rasterizer, and
float-linear CPU compositor; its evaluated title and Cross Dissolve coefficient
must equal the Export render plan, and its raster signature and pixel hash must
be unchanged after reopen. The exact named Windows font is a real dependency,
so missing or changed font data fails closed. These generated pixels prove
regression parity only: they do not replace an independent application or
specification reference, real-media handle coverage, the complete keyframe
editing UI, or the three-run top-level Golden exit gate.

Run the complete release-profile M0 playback plan. This preflights the machine
before expensive generation, validates the Playback corpus, runs the 30-minute
Video and CPAL A/V gates, and creates one evidence bundle:

```powershell
pwsh -File scripts/validation/invoke-playback-reference-gates.ps1 `
  -MachineId edit-bay-a `
  -RegenerateGeneratedFixtures
```

Each plan-v3 gate has an external 45-minute process deadline. Timeout kills the
complete Cargo/test descendant tree and fails the run even if a partial report
exists. The Video gate additionally writes a flushed
`video-decode-progress.jsonl` artifact beside its report; its last record
identifies the most recently observed media call without granting recovery
authority. Evidence records the
configured deadline, elapsed wall time, timeout outcome, journal presence, and
journal SHA-256.

The Video gate first builds the packaged `mondrian` executable under the same
release profile, records the bounded build log plus executable hash, and passes
its absolute path through `MONDRIAN_PREVIEW_DEMUX_WORKER_PATH`. Exact-Still
format work therefore exercises the same hidden helper dispatch shipped to
users; a stale developer binary, `PATH` lookup, or test-harness executable
cannot silently satisfy the process-isolation gate.

Use `-Gate Video` or `-Gate Audio` for a partial diagnostic. Use
`-AllowDirtyDiagnostic` only when results are intentionally non-baseline. A
machine below the reference performance class may use
`-AllowUnqualifiedDiagnostic`, but only when every failed qualification code is
explicitly listed by the versioned playback plan as diagnostic-waivable. The
current plan permits only `machine.memory-class`; memory below the 8 GiB support
floor, missing GPU identity, unsupported OS
or architecture, wrong toolchain, missing FFmpeg/FFprobe, invalid Git identity,
and every other execution prerequisite still stop the run. This switch always
disqualifies the result from becoming a baseline, even if the machine happened
to satisfy the profile.
Reports belong under `target/validation/runs/`. Baseline eligibility requires
the complete two-gate set, a qualified machine, a clean tree, the same Git
revision before and after the run, attested artifacts, zero process failures,
and passing structured profiles. A loose cargo log is not acceptance evidence.

## Baseline governance

- Changing a fixed artifact's bytes creates a new fixture ID and hash. Changing
  a generated recipe creates a new recipe hash and normally a new fixture ID;
  concrete generated bytes remain run-local evidence.
- A changed golden image, tolerance, performance threshold, or expected fallback
  requires a review note explaining the semantic reason and independent evidence.
- Unsupported input must produce the expected blocker and diagnostic. Skipping,
  silently converting precision, or accepting visibly wrong color is a failure.
- HDR-to-SDR fallback is allowed only where the scenario declares it and the
  capability report records the actual path.

## Current coverage status

The manifest contains the committed Standard numeric color corpus, two
project-generated M0 workload recipes (1812 seconds of 4K25 HEVC Main10
Long-GOP Rec.709 code-pattern video and 1835 seconds of 48 kHz stereo AAC), plus
the 305-second Golden PCM authoring fixture.
They make the professional playback run reproducible without importing local
downloads or asserting false color correctness. The clean `c484c47` run
`20260722T065141Z-local-windows-dev-01-f4fc3eff` completed the full Video+Audio
plan on the qualified 16 GiB Windows reference machine and was classified
`passed-baseline`. Its generated artifacts and evidence bundle remain
intentionally disposable under `target`; the repository commits the recipes,
contracts, and this reproducible result record rather than a multi-gigabyte
machine-specific bundle. The Golden v3 contract resolves its PCM and AAC
fixture identities, but only PCM is assigned to an executable slice.
`foundation-audio-authoring-v1` has a real Headless product-workflow gate rather
than a declaration-only check. HLG Main10 picture, Rec.709 H.264
picture, and sRGB Alpha still roles remain deliberately null until qualifying
fixtures and appropriate independent color evidence exist. Stress coverage
also still lacks 4K60 and broader Log/VFR/multichannel/damaged-media fixtures,
so `Nightly/Release -Scope All` correctly remains blocked.
