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
  export workflow. Contract identity `windows-alpha-golden-v13` uses closed
  schema v4 and fixes exact Hero Sequence raster/timing/color/audio
  values, fixture-role purposes, stable built-in delivery preset identities,
  resolved profile/depth/chroma/range/Alpha expectations, and independently
  executable evidence slices. Every slice declares an acceptance Sequence role;
  global coverage and coverage on the one Hero role are compiled separately.
  It is the source contract for a generated `.mdp`; hand-written project JSON
  is not accepted as execution evidence.
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
typed postcondition evidence. Golden v13 rejects unknown fields; requirement IDs
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
  golden_acceptance_plan_requires_every_obligation_on_the_hero_sequence -j1 -- `
  --nocapture
```

The v11 global ledger is structurally complete: every required fixture,
operation, content item, and export contract is assigned to one of seven
executable slices, and every slice is assigned to the same Hero role. HLG
Main10 and sRGB Alpha remain bound to deterministic project-owned recipes with
run-local artifact attestations, so both the global and Hero ledgers have empty
missing/unbound sets. The test also mutates Color Media to a diagnostic role in
memory and requires those two fixtures to reappear as exact Hero blockers. The
plan does not run a product Interface and always keeps
`complete_golden_project: false`; only the coordinated Hero process described
below can produce complete execution evidence.

Verify the single-Project Headless workflow boundary:

```powershell
cargo test -p mondrian-app --lib `
  golden_product_workflow_binds_hero_and_diagnostic_sequences_explicitly `
  -j1
```

This ordinary-CI test creates one production Project, crosses a fresh open,
binds one Hero slice without creating a Sequence, then uses an intentionally
mutated in-memory contract to exercise diagnostic Sequence creation through the
product authoring Interface. It returns to Hero through the product switching
action and performs durable save/close/reopen. The private binding locks Hero
ID, complete settings, and Project Color Environment; Project/path and both
Sequences survive while only Session identity changes. The canonical v11
contract does not assign any acceptance slice to that diagnostic role. This is
infrastructure evidence, not a Golden operation, and does not alter the ledger.

After the canonical PCM, AAC, H.264, HLG, and Alpha fixtures are available,
verify that every stage composes without report union:

```powershell
$env:MONDRIAN_GOLDEN_FIXTURE_ROOT='target/validation/golden-fixtures'
cargo test -p mondrian-app --lib `
  golden_all_stages_share_one_hero_sequence `
  -j1 -- --ignored --nocapture --test-threads=1
```

The gate runs Foundation Audio, Visual Authoring, Editorial/Transport,
Generated Delivery, Proxy/Relink, Recovery/Nesting, and Color Media on the same initial
7,500-frame Hero Sequence and crosses every durable reopen. Before Recovery it requires one
unchanged Project ID/path and exactly one Sequence; Recovery must retain that
Hero primary and add exactly one nested child. Color Media must keep the total
at exactly two Sequences while adding two adjacent Tracks in `500..525`, with
HLG below Alpha, exact HLG source mapping, explicit still hold, and
original-source reference execution. Visual must leave the complete Hero audio
projection unchanged.
Editorial binds only two complete pristine audio Tracks, leaves the exact
Foundation Track-owned audio anchor and complete visual projection unchanged,
and authors the AAC workflow through typed product outcomes for Drop,
Overwrite, targeted Split, Ripple Delete, and multi-Track Insert. It then
resolves Track Targeting and Sync-Lock through ordinary session Actions without
advancing durable author state. Lift removes only the targeted inserted Clip
without closing program time; Extract trims the targeted primary Track while
closing exactly five frames on both the primary and the untargeted but
Sync-Locked secondary Track. Both operations consume exact half-open In/Out
ranges and prove complete one-step Undo/Redo author roundtrips. Delivery
reuses the Foundation PCM placement instead of importing a duplicate, uses the
nonzero `150..175` Work Area, and preserves all earlier Track-owned authoring
while it authors Trim, Transform, and Opacity.
Proxy/Relink adds one dedicated video Track with adjacent CFR `200..350` and
VFR `350..500` Clips. Both imports and proxies execute through production
workers. The CFR source proves Proxy→Original→Proxy plus asynchronous offline
Relink and replacement proxy generation without changing preceding anchors.
Each Clip then executes exact `1/2`, `-1/2`, and reverse hold through normal
product Actions. Preview and Export must retain the same complete covering or
strict-predecessor source target. The CFR oracle verifies physical frame 25/49;
the VFR oracle instead verifies exact requested/selected/duration PTS and a
60 ms predecessor immediately before a 20 ms covering interval. Headless
presents reverse and hold from proxies, while immutable snapshots export each
held frame from originals through H.264/AAC. Ordinary reimport/decode must be
both absolutely and proportionally closer to the expected Program than
adjacent-covering and wrong-direction counterfactuals. This rejects nominal
average-rate sampling, metadata-only retime, Preview/Export disagreement,
wrong proxy/original authority, dropped boundary kind, and proxy delivery.
Recovery must then preserve every earlier scoped anchor while its exact
`175..200` parent placement, nested child, Autosave recovery, and covering
manual reopen remain identical.
Color Media and the final reopen must preserve those earlier anchors as well as
its own exact Track/Clip/Asset placement hashes.

After a distinct durable reopen, Delivery renders an independent Program
reference, exports both contracted H.264 High/AAC and HEVC Main10/AAC outputs,
and reimports each through the product media worker. It compares selected
decoded pixels against the Program reference and an independent Rec.709
Transform/Opacity oracle. It also renders reference PCM through the production
audio Program Runtime, decodes each AAC stream through the bounded production
`AudioSourceReader`, checks channel RMS/peak tolerances and cross-codec
agreement, and proves A/V start/end boundaries from exact stream-local
`start_pts`, `duration_ts`, and `time_base`.

Pointer-drag and settled seeks must produce exact stage-local Playback Evidence.
Each seek and Play must also make a current output usable through the production
Preview Runtime, consume the exact Frame Presentation Ticket, and record whether
the output used real Headless GPU execution or the allowed final CPU Raster
fallback. Basic Title currently exercises the CPU path; it is not reported as
GPU. Play leaves priming only after Runtime-derived preroll and advances under
the Synthetic Clock Master. This focused composition test remains partial
execution evidence and cannot replace the top-level supervisor, long CPAL, GPU,
cancellation, memory, or A/V drift gates or report
`complete_golden_project: true`.

With production FFmpeg encoders available, run the complete Golden Project
gate:

```powershell
pwsh -File scripts/validation/invoke-complete-golden-project-gate.ps1
# Or select a prepared corpus explicitly:
pwsh -File scripts/validation/invoke-complete-golden-project-gate.ps1 `
  -FixtureRoot target/validation/golden-fixtures
```

The script validates the contract and fixtures, builds the feature-gated
`mondrian-golden` executable once, and requests three runs. Before executing
heavy stages, the Rust planner requires every fixture, operation, content item,
and export to be assigned to the Hero Sequence role. All seven stages share the
five-minute primary Sequence; Recovery owns one auxiliary nested child rather
than another primary, and Color Media retains that exact two-Sequence project
shape. There are no remaining global or Hero ledger gaps.

The Rust coordinator accepts only the exact
declared slice-report set, requires every slice to retain
`complete_golden_project: false`, and requires all stages to
report the same primary `SequenceId`. It waits for proxy work to quiesce,
performs one final durable reopen, verifies the exact full-duration Hero content
boundary, and compares scoped author anchors, every Sequence snapshot, Project
identity, relink source/proxy intent, Color Media identities, and reimported
`H264High`/`HevcMain10` assets. It alone may
write a complete report. The PowerShell supervisor independently verifies the
Hero identity and may then classify three distinct run/Project identities as a
consecutive pass under `target/validation/runs/`.

The reproducible local run
`20260725T234232Z-complete-golden-42ccff21` passed the preceding v10 contract
`3/3`: each pass used a distinct run and Project identity, all seven primary
Sequence IDs resolved to that pass's Hero, and the final project contained
exactly Hero plus one nested child. The aggregate report SHA-256 is
`4edaeb0404fad68b813f5592a0533b86788414b3f3142ac88be04e46fb5ca8aa`.
The current v11 contract adds fixed-corpus constant-retime decode, presentation,
export, and counterfactual reimport evidence, so the v10 result is now
historical and cannot satisfy the current three-run release obligation. The
preceding v9 run `20260725T172841Z-complete-golden-d631639d` likewise remains
historical composition evidence. No Golden run claims qualified
release-machine performance, long-duration device/A/V evidence, fault
injection, or an independent absolute HLG/PQ/Log reference.

The official v11 supervisor run
`20260726T075954Z-complete-golden-30ffb812` passed `3/3` against the isolated
Golden fixture root. All passes used distinct run and Project identities, all
seven primary Sequence IDs resolved to each pass's one Hero, and every final
Project contained exactly Hero plus one nested child. The aggregate report
SHA-256 is
`e596fc2756eada710d84a0873dcd5e13f49c7d33ee35b8c959e6e130465ece4f`.
Each process emitted and durably published its complete passing report in
roughly 57–58 seconds, then required the documented five-second process-detach
recovery; the supervisor recorded `forced_after_terminal_report: true` without
changing semantic status. This current evidence supersedes v10 for the Golden
release repetition obligation only and is historical after v12.

The v12 supervisor run
`20260805T163443Z-complete-golden-58886bac` passed `3/3` against the repository
fixture root after `Pr/All` reference-asset validation. It was rebuilt from the
current source with incremental compilation disabled in 467.818 s. The three
processes used distinct run and Project identities, exited naturally without
terminal-report recovery, and published complete reports in 67.656 s, 64.158 s,
and 64.860 s. Each report binds all seven stages to its one Hero Sequence and
retains exactly Hero plus the strong-reference nested child after final durable
reopen. The aggregate report SHA-256 is
`20fa59a873180594c927ec1566f4bef225a610eaf6310da0c655e271c0a16633`;
the ordered pass-report hashes are
`5d5e77bbaa0e5956b316d2a418e25a6347a3bec2b2c52f55e175ca2d4645bd72`,
`89c15d2b3dd42380b310b8919b0e5f3ae581de39ec7417a981b74fb8bdfb559b`,
and `aa2da824abe8ca2385a4c31b9af304b7e1847d6a0735070baeffcd2e88b05877`.
This closes only the versioned complete-Golden repetition obligation; it does
not claim qualified-machine throughput, long-duration device/A/V, memory, or
independent absolute color-reference evidence.

The current v13 supervisor run
`20260806T110402Z-complete-golden-c69b0bd8` passed `3/3` after `Pr/All`
reference-asset validation. The non-incremental single-job product build took
355.780 s. Its three processes used distinct run and Project identities,
exited naturally, and published complete reports in 73.776 s, 68.908 s, and
69.328 s. Every report binds all seven stages to one Hero Sequence, retains
exactly Hero plus the strong-reference nested child, and includes Visual report
schema v9 with the ordered Primary/LUT/Gaussian Blur/Sharpen stack. The
aggregate report SHA-256 is
`aadefedb08429bd7320204a6ea55e33862f9aa2948effe9732f1cf705af5bf3d`;
the ordered pass-report hashes are
`1ebedb7133ac92b02bdb0ec0582375239d22b5a53295005a91d2ec84d6ed2ff2`,
`4501cd79525dbe9706d2aec5e962a936e87e653e9dc443d9d1f1090335eaac49`,
and `e20eb9985c7c12f07ca3884461825cc8917784ce4bdb92a0183f7094c222f5cd`.
This supersedes v12 for the complete-Golden repetition obligation only; it
still does not claim independent Blur/Sharpen reference quality, qualified
machine throughput, long-duration device/A/V, memory, or absolute color
reference evidence.

Heavy GPU/media work intentionally runs on a dedicated process main lifetime,
not a libtest worker. The terminal JSON report is the semantic completion
boundary. After a short natural-exit grace period the supervisor may terminate
a child blocked by a third-party Windows graphics-hook DLL during process
detach; missing, malformed, or failing reports still fail closed, and process
cleanup cannot turn a failure into a pass.

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
pwsh -File scripts/validation/generate-golden-editorial-video.ps1 -Force
pwsh -File scripts/validation/generate-golden-color-reference-media.ps1 -Force
```

Generated files and attestations must come from the same fixture root selected
by `MONDRIAN_GOLDEN_FIXTURE_ROOT`; a stale ignored cache under another root is
not repaired or accepted implicitly. The Reference Playback recipe emits AAC
with an explicit Stereo channel layout. Without `-Force`, it verifies the
existing recipe hash, artifact hash, size, and name and refuses reuse on any
mismatch; it never rewrites an old artifact's provenance for a new recipe.

The Golden generator currently creates a 305-second, 48 kHz stereo PCM S16LE
stream in a MOV container with analytically different left/right signals. MOV
is intentional because it preserves the declared Front-Left/Front-Right layout;
two channels without a Stereo layout cannot qualify. It is sufficient to make
gain, pan, fades, channel swaps, import, and persistence observable; it is not a
color or acoustic-loopback reference. A non-`-Force` run only reuses an artifact
whose existing attestation matches the current recipe and artifact hash; it
never rewrites an old artifact's provenance.

The editorial-video generator creates an eight-second 1920×1080, 25 fps,
H.264 High 8-bit 4:2:0, limited-range Rec.709 code pattern with no audio. It is
project-authored and redistribution-safe, but explicitly has
`color_reference_eligible: false`: CICP/probe checks qualify codec, source
interpretation, proxy, and relink behavior only. They do not establish color
accuracy.

The color-reference generator creates two redistribution-safe, project-owned
stimuli: one-second 1920×1080 HEVC Main10 HLG patch video with exact
BT.2020/HLG/non-constant-luminance/limited tags, and one 1920×1080 RGBA PNG
with an explicit sRGB chunk and straight-Alpha patches. The recipe and semantic
probe are stable; each FFmpeg build's concrete bytes are attested locally.

Run their product roundtrip slice:

```powershell
pwsh -File scripts/validation/generate-golden-color-reference-media.ps1 `
  -OutputRoot target/validation/golden-fixtures/large -Force
$env:MONDRIAN_GOLDEN_FIXTURE_ROOT = "target/validation/golden-fixtures"
cargo test -p mondrian-app --lib `
  golden_color_media_roundtrip_executes_production_interfaces -j1 -- `
  --ignored --nocapture --test-threads=1
```

The slice requires first-class `StillImage` import, a zero-rate Media hold,
two directly adjacent product-authored Hero video Tracks, exact placement in
`500..525`, and durable reopen. HLG is below Alpha; the HLG Clip retains its
exact source interval, and color-reference execution selects the original source
rather than a proxy. It
then decodes through the production Preview media Adapter, uses the shared
float-linear compositor, checks HLG decoded codes plus neutral/chromatic
invariants, compares sRGB→linear Rec.2020 against an independent analytic
matrix, and proves RGB behind zero Alpha cannot affect the composite. Finally
it exports one Rec.709 H.264 frame through the production queue, strictly
probes and reimports it, and compares sampled Program pixels. This is not an
independent absolute HLG transfer-function oracle and does not close PQ, Log,
or real-media nesting. Its report remains partial even though the complete
coordinator consumes it as one of seven required stages.

Run the Proxy/Original and Offline Relink slice:

```powershell
pwsh -File scripts/validation/generate-golden-editorial-video.ps1 `
  -OutputRoot target/validation/golden-fixtures/large -Force
pwsh -File scripts/validation/generate-golden-vfr-retime-video.ps1 `
  -OutputRoot target/validation/golden-fixtures/large -Force
$env:MONDRIAN_GOLDEN_FIXTURE_ROOT = "target/validation/golden-fixtures"
cargo test -p mondrian-app --lib `
  golden_project_proxy_original_offline_relink_gate -j1 -- `
  --ignored --nocapture --test-threads=1
```

The slice copies the attested CFR fixture into two run-local Relink locations
and the attested VFR fixture into a third source location. The stage imports
both through the production media worker, reuses Hero, creates one dedicated
video Track, and authors adjacent `200..350` CFR and `350..500` VFR placements
through ordinary Timeline Actions. Both import-driven proxies must cross real
worker boundaries and publish fresh artifacts. CFR Product
Actions then switch Preview to original, back to the exact same fresh proxy,
and finally to original again; every switch is exactly one Project author
transaction and does not revise the Sequence. The run-local source is then
made offline. Preview must report a structured unavailable source, the ordinary
Relink Action must complete its bounded two-phase probe/commit operation,
advance only the Asset Library revision, publish its reload event, and retain
the same typed Asset/Clip identities and authored name.
Because proxy identity includes the source path and fingerprint contract, the
old proxy must be ineligible for the replacement path; a second real generation
must publish a distinct fresh proxy. Report schema v5 captures both placement
anchors, Relink operation/generation/terminal evidence, signed CFR/VFR source
targets, physical PTS intervals, proxy/original bindings, Headless completions,
two exports/reimports, and project/asset proxy intent so the following
Recovery stage can prove they survive a new Session and covering reopen. The
report remains a partial slice and never claims independent color-reference or
complete Golden status.

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
as the M1 Golden exit gate. Foundation report schema v4 additionally records
the fixed Project ID/path at each lifecycle checkpoint and runs as a reusable
stage over `GoldenProductWorkflowDriver`.

Generated Delivery deliberately has no isolated-Sequence libtest wrapper.
During development, run the combined Hero command above so its 25-frame Work
Area must coexist with Foundation, Visual, and Editorial authoring. This keeps
its strong codec/profile/pixel-format, Rec.709 signal, AAC, A/V boundary,
Program-pixel, and audio-roundtrip evidence without allowing a standalone
sample Sequence to masquerade as programme integration.

Run the fixture-free visual-authoring slice on Windows:

```powershell
cargo test -p mondrian-app --lib `
  golden_project_visual_authoring_roundtrip_gate -j1 -- `
  --ignored --nocapture --test-threads=1
```

This slice creates an exact adjacent generated edit, adds the product-default
Cross Dissolve, creates and edits a Basic Title, applies working-space-aware
Primary Color, and binds a generated identity cube to an explicitly authored
Rec.709 processing domain. The same ordered Clip stack then adds finite-kernel
Gaussian Blur and Sharpen through the product Effect Interface. The report
requires one compiled node for each operation in both the Preview and Export
graph, records stable Effect identities and parameters, and proves Sharpen's
canonical default plus authored value through a complete Undo/Redo pair. It
also authors two-key Hold, Linear, and Bezier curves through production App
Interfaces. Every edit,
Undo, and Redo must advance the installed Authoring Session by exactly one
Author Generation and Sequence Revision. The gate then performs production
durable save/close/fresh-open and repeats Headless Preview execution. Preview
uses the recursive timeline executor, real system-font rasterizer, and
float-linear CPU compositor; its evaluated title, Cross Dissolve coefficient,
and compiled effect-graph signature must equal the Export render plan. Effect
IDs/order, Primary/Blur/Sharpen parameters, LUT processing
space/path/content hash/domain, raster signature, and pixel hash must be
unchanged after reopen. The exact named
Windows font is a real dependency, so missing or changed font data fails closed.
These generated pixels and the generated identity LUT prove
regression parity only: they do not replace an independent application or
specification reference, independent Blur/Sharpen visual or numerical
references, real-media handle coverage, the complete keyframe
editing UI, or any other complete-run stage. Visual report schema
v9 records the Project-scoped stage-Sequence creation, Blur/Sharpen author and
execution evidence, and runs over the same
`GoldenProductWorkflowDriver` used by Foundation Audio. Its standalone wrapper
still creates an isolated development run, while the complete coordinator
invokes every reusable stage against one Project.

Run the fixture-free Recovery/Nesting slice:

```powershell
cargo test -p mondrian-app --lib `
  golden_project_recovery_nesting_roundtrip_gate -j1 -- `
  --nocapture --test-threads=1
```

The slice creates a generated Solid Color placement and dispatches the formal
Timeline Precompose Action from the current selection. Precompose must replace
the parent placement, create one `NestedComposition` Sequence, project selected
content to child-local zero, advance Project Generation and the parent Sequence
Revision exactly once, and select the replacement Clip. The Headless Adapter
then runs the ordinary recursive Preview compositor and actual recursive Export
frame renderer; both parent and child must remain float-linear without a legacy
RGBA8 fallback.

The same dirty author state is published through the production autosave
worker. Discovery must validate manifest schema, exact child path, archive
SHA-256, document revision, Project identity, and canonical source identity.
The stage keeps the initial Hero Sequence as its primary identity, adds one
dedicated video Track, trims the generated source to the exact `175..200`
window, and creates only one strongly referenced nested child. The test closes
the original Session and recovers only through the product recovery Action.
Complete Hero parent/child Sequence hashes, typed IDs, Preview pixels, and
Export diagnostics must match; opening recovery must not retire its source.
Only a manual save that covers current author and Asset Library revisions may
atomically publish an empty manifest and remove the recovery archive. A final
fresh reopen must still match all author and execution evidence. This proves a
generated recovery/nesting path, not recovery-conflict UI, disk-full/permission
fault handling, real-media nested color, or complete Golden status.

Run the complete release-profile M0 playback plan. This preflights the machine
before expensive generation, validates the Playback corpus, runs the 30-minute
Video and CPAL A/V gates, and creates one evidence bundle:

```powershell
pwsh -File scripts/validation/invoke-playback-reference-gates.ps1 `
  -MachineId edit-bay-a `
  -RegenerateGeneratedFixtures
```

Each plan-v4 gate first performs a separately bounded release test prebuild and
retains its log, exit status, timeout status, and elapsed time. The external
45-minute process deadline starts only after that prebuild, so a cold compiler
cache cannot consume the gate's normative 30-minute wall-clock observation
window. Execution timeout kills the complete Cargo/test descendant tree and
fails the run even if a partial report exists. The Video gate additionally writes a flushed
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

Realtime observation count and authored Sequence extent are independent.
External development probes reserve bounded startup headroom, capped by the
probed source frame count, so cold decoder/GPU priming cannot consume the
declared observation window. The measured Playback Evidence window begins
after the first exact-current presentation and preroll qualification. Because
that first observation may occur at any phase inside a frame, the conservative
wall-clock lower bound for `N` subsequent frame-boundary crossings is
`(N - 1) * frame_interval`; exact start/end coordinates and Engine displacement
evidence independently prove the full number of crossings. Missed intermediate
frames count once against the fixed opportunity denominator, and a gap crossing
the terminal boundary cannot grow that denominator.

## Current coverage status

The manifest contains the committed Standard numeric color corpus, two
project-generated M0 workload recipes (1812 seconds of 4K25 HEVC Main10
Long-GOP Rec.709 code-pattern video and 1835 seconds of 48 kHz stereo AAC), the
305-second Golden PCM authoring fixture, and short CFR/VFR H.264 High editorial
fixtures used only for proxy/relink/time semantics.
They make the professional playback run reproducible without importing local
downloads or asserting false color correctness. The clean `c484c47` run
`20260722T065141Z-local-windows-dev-01-f4fc3eff` completed the full Video+Audio
plan on the qualified 16 GiB Windows reference machine and was classified
`passed-baseline`. Its generated artifacts and evidence bundle remain
intentionally disposable under `target`; the repository commits the recipes,
contracts, and this reproducible result record rather than a multi-gigabyte
machine-specific bundle.

The clean `0175f35` partial run
`20260805T172741Z-local-windows-16g-3194bed3` reran the current Video gate on
the qualified 16 GiB Windows reference machine without regenerating or
committing the generated fixture. It observed 45,002 timeline frames over
1,800,042,719 microseconds, with 45,003/45,003 native P010 hardware-presented
media layers, zero CPU transfers, Viewer fallbacks, readbacks, or GPU blockers,
and 45,105/45,105 native-import GPU timing samples with no missing, dropped,
duplicate, or orphan evidence. The same process completed 50 warm seeks at
15,392 microseconds p95, 52 accurate seeks at 356,722 microseconds p95, and
100 latest-wins supersessions with zero rejected terminal deliveries or final
Broker residency. Across 1,802 product-process-tree samples, Windows Private
Commit peaked at 1,215,262,720 bytes, settled growth was 17,589,808 bytes, and
the post-stress sample was 942,784,512 bytes. The structured professional
profile passed with no failures and the decode journal was present. The
supervisor correctly classified the run as `passed-diagnostic`, not baseline,
because `-Gate Video` omits the required Audio gate; this result closes the
current-revision Windows long-video slice only.

The Golden v13 contract resolves PCM, AAC, CFR and
VFR Rec.709 H.264, HLG Main10, and sRGB Alpha fixture identities and assigns
all six to executable slices.
`foundation-audio-authoring-v1`, `visual-authoring-roundtrip-v1`,
`editorial-transport-v2`, `generated-delivery-roundtrip-v1`,
`proxy-relink-v1`, `recovery-nesting-v1`, and `color-media-roundtrip-v1`
now share one Hero Sequence. Recovery adds one nested child while Proxy/Relink
retains a real focused Headless product-workflow gate rather than becoming a
declaration-only check. The CFR and VFR fixtures close forward/reverse/hold
source mapping through proxy presentation, original-source production export,
ordinary reimport, physical PTS interval evidence, and adjacent/wrong-direction
counterfactuals; their manifest purposes are explicit and neither is eligible
as a color reference.
Color Media adds real file-backed color/Alpha execution in `500..525`, while
explicitly retaining the independent absolute HLG/PQ/Log gap. The H.264
editorial fixtures are not eligible to close
primary-color or LUT coverage. Stress coverage
also still lacks 4K60 and broader real-device VFR/Log/multichannel/damaged-media fixtures,
so `Nightly/Release -Scope All` correctly remains blocked.

A separate non-ignored Retime Hero seam gate uses the same production
single-Project workflow driver with internally consistent metadata-only
video/audio streams. It proves that one linked exact-rate transaction and one
picture-only hold retain identical source coordinates through Preview and
Export render-plan lowering, audio semantic compile and dense preparation,
Undo/Redo, durable `.mdp` publication, and a fresh Authoring Session reopen.
Because its one-byte source is never decoded or encoded, this gate is not
evidence of picture, PCM, codec, or delivery correctness; those obligations
remain with the fixed-corpus executable slices.
