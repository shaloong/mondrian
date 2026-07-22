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
  export workflow. It is the source contract for a generated `.mdp`; hand-written
  project JSON is not accepted as execution evidence.
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
```

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

The manifest contains the committed Standard numeric color corpus and two
project-generated M0 workload recipes: 1812 seconds of 4K25 HEVC Main10
Long-GOP Rec.709 code-pattern video, and 1835 seconds of 48 kHz stereo AAC.
They make the professional playback run reproducible without importing local
downloads or asserting false color correctness. The clean `c484c47` run
`20260722T065141Z-local-windows-dev-01-f4fc3eff` completed the full Video+Audio
plan on the qualified 16 GiB Windows reference machine and was classified
`passed-baseline`. Its generated artifacts and evidence bundle remain
intentionally disposable under `target`; the repository commits the recipes,
contracts, and this reproducible result record rather than a multi-gigabyte
machine-specific bundle. Golden/Stress still lack verified HLG/PQ, Rec.709
H.264, sRGB alpha, PCM/WAV, camera Log, VFR, multichannel, damaged-media, and
independent image references, so `Nightly/Release -Scope All` correctly remains
blocked.
