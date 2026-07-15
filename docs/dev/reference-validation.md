# Reference Validation

This system makes media correctness and performance claims reproducible. It does
not make a capability `Verified` merely because a file, decoder, shader, or UI
control exists.

## Contracts

- `tests/validation/corpus-manifest.json` owns immutable fixture IDs, paths,
  hashes, provenance, declared media properties, and intended test purposes.
- `tests/validation/golden-project.json` defines the five-minute editing and
  export workflow. It is the source contract for a generated `.mdp`; hand-written
  project JSON is not accepted as execution evidence.
- `tests/validation/stress-project.json` defines the 30–60 minute workload and
  stability thresholds.
- `tests/validation/windows-alpha-reference.json` defines the reference-machine
  class. A captured machine report is evidence for a run, not a modification of
  the reference profile.

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

## Asset classes

| Class | Repository | PR | Windows nightly/release |
| --- | --- | --- | --- |
| `committed` | Small and redistributable | Required and hash-checked | Required |
| `generated` | Recipe committed; result disposable | Recipe/contract checked | Generated and checked by its scenario |
| `local-restricted` | Never committed | Optional; checked when present | Required and hash-checked |

`redistribution: unverified` is deliberately conservative. Such media must not be
uploaded to CI artifacts, mirrors, releases, or public fixture bundles. Runtime
downloads from mutable or “latest” URLs are prohibited.

## Tiers and evidence

1. `Pr` validates schemas, stable IDs, hashes, and any locally present assets. It
   is fast and does not turn unavailable restricted media into a false failure.
2. `Nightly` requires the full local corpus on the dedicated Windows runner and
   runs real decode, seek, display/capability, Golden Project, and stress cases.
3. `Release` has the same asset strictness and additionally requires three
   consecutive Golden Project passes plus export/reimport and recovery evidence.

Run manifest validation:

```powershell
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Pr
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Nightly
```

Capture the reference machine before a performance or release run:

```powershell
pwsh -File scripts/validation/capture-windows-reference.ps1
```

Reports belong under `target/validation/` and must record the Git revision and
dirty state. A result from a dirty tree can diagnose a problem but cannot replace
a release baseline.

## Baseline governance

- Changing bytes creates a new fixture ID and hash; do not silently update an old
  entry.
- A changed golden image, tolerance, performance threshold, or expected fallback
  requires a review note explaining the semantic reason and independent evidence.
- Unsupported input must produce the expected blocker and diagnostic. Skipping,
  silently converting precision, or accepting visibly wrong color is a failure.
- HDR-to-SDR fallback is allowed only where the scenario declares it and the
  capability report records the actual path.

## Current coverage status

The manifest now inventories the existing restricted color corpus without
inventing unknown metadata. It establishes the reproducible gate and project
contracts, but it does not claim that the Golden Project has executed. Missing
Rec.709 H.264, verified HLG, sRGB alpha, PCM/WAV, AAC, reference frames, and
licensed provenance remain explicit corpus acquisition tasks; the Windows
nightly/release workflow must remain blocked until those roles are fulfilled.
