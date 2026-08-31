# Platform / Driver / Display Qualification

This qualification closes the commercial support matrix across exact Windows /
DX12, macOS / Metal, and Linux / Vulkan environments. It is physical-hardware
evidence. Hosted CI compilation, an offscreen GPU render, EDID capability, or a
successful developer smoke cannot qualify a row.

## Evidence model

The checked-in policy is
`tests/validation/platform-driver-display-matrix.json`. A release-specific
runtime profile explicitly enumerates its supported rows. It does not derive an
unbounded Cartesian product or use labels such as `latest`.

One row is one physical machine, clean source revision, exact product package
and actually executed runtime image,
active Renderer adapter, OS/driver/compositor state, display path, ICC/HDR state,
and unique run. Its platform probe, GPU color, and physical Viewer evidence must
execute serially. Capture the exact environment before and after the row; any
identity drift invalidates it. Never combine reports across a restart, driver or
ICC change, different display, different run ID, or different machine report.

The complete matrix may aggregate independently sealed rows from multiple
machines. Every row must bind the same canonical runtime profile, source SHA,
release-candidate identity, and build-manifest SHA. Each row additionally binds
the exact target-specific distribution package, runtime image, and build-provenance SHA;
Windows, macOS, and Linux package bytes are expected to differ. Missing rows or
evidence produce `incomplete`, never a skip or pass. An executed failure
dominates missing evidence.

The normalized evidence deliberately excludes hardware serial numbers and raw
ICC/EDID payloads. It carries opaque display-path identity plus payload hashes.
Normalization is not a trust boundary: every lane also seals the original
owner source files, and the verifier deterministically replays those files into
the normalized result. Restricted ICC/EDID payloads can remain separate source
entries, but their roles, byte lengths, and SHA-256 values stay in the row and
matrix closure.

Every owner lane report uses the normalized schema-1 envelope consumed by
`verify-platform-driver-display-lane.ps1`: `kind`, `owner`, `verifier_id`,
`status`, canonical profile hash, raw-evidence hash, environment hash, source,
machine, row-run, release-candidate, build-manifest, build-provenance, and
target-artifact bindings plus an owner payload. The platform payload proves the
exact native probe set and display/inventory identity; the GPU payload proves a
hardware adapter, exact backend/vendor/device and platform-correct driver stack,
all gates executed/passed, and zero skips; the Viewer payload binds the same
display identity and repeats the exact scenario closure with Ready health,
valid contract, external-texture
presentation, zero readback, scenario-correct carrier reuse, operator pass, no capability skip,
and full output-contract/attestation hashes. Legacy COL-008/COL-010 summaries
that lack this envelope are inputs to an owner capture Adapter, not matrix
receipts by themselves.

Each row-manifest lane has a schema-1 `source_evidence` object with a fixed
owner `source_verifier_id`, a unique capture ID, full row/release/artifact
bindings, and finite entries `{ role, path, sha256, byte_length, format }`.
Roles match `^[a-z0-9][a-z0-9._-]{0,95}$`; paths are relative, non-link, and
unique across the row. The manifest also contains separate schema-1 environment
snapshots captured before and after execution. Source replay requires:

- one authority-challenged, Rust-produced native transcript for every required
  scenario/API pair. `mondrian-platform-display-probe-source` invokes the same
  `mondrian-platform` Adapter used by the product and records the raw ICC or
  HDR/EDR API return, exact target, sequence, producer image, process, row, and
  challenge. Managed ICC captures both ICC and HDR state. The verifier derives
  SDR/P3/PQ/ICC admission from those fields; authored `qualified` booleans are
  not source evidence;
- the GPU profile/supervisor/build logs plus stdout, stderr, and a uniform
  test-emitted measurement JSON and machine-readable report for every required
  gate. The profile bytes must equal the checked-in
  `platform-gpu-color-gates.json`, Cargo must report exactly one passing test,
  and every finite measurement must satisfy the checked-in comparison limit.
  Each test process hashes its own executable and echoes the pre-issued capture
  challenge plus row/source/runtime/supervisor identity; the session records
  exact argv, start/end, and exit;
- the operator observation plus product JSONL and stimulus definition for every
  profile-required Viewer scenario.

Viewer JSONL binds one supervisor nonce, process instance, PID, monotonic record
sequence, runtime-image hash, active Renderer adapter, exact native display
path, complete canonical Display Output Contract, and ICC processor identity.
The source verifier deserializes the full contract through
`mondrian-core`, recomputes its v2 identity, and derives validation, ICC, and
HDR facts instead of trusting the compact diagnostic projection. Scenario files from
another process, adapter, target, runtime, or run cannot be spliced. On Linux
the runtime hash reads `/proc/self/exe`, which resolves the mapped executable
inode even if the launch path changes. Windows measures the locked executable
file. macOS loaded-image/code-signature provenance remains part of the approved
physical capture authority and must not be inferred from a mutable path alone.
Managed-ICC Viewer evidence also carries the platform ICC bytes. A Core replay
rebuilds the exact LUT from the contract source space and product-reported
rendering intent, then compares complete profile and processor identities
across Platform and Viewer lanes.

## Required rows and native evidence

- Windows uses the active DX12 hardware adapter. ICC evidence first uses the
  active DisplayConfig adapter LUID/source ID with
  `ColorProfileGetDisplayDefault`, including Advanced Color profiles, and only
  then falls back to the WCS device default (`CPT_ICC + CPST_NONE`). Advanced
  Color active state remains independently proven by DisplayConfig.
- macOS uses the active Metal hardware adapter. CoreGraphics supplies ICC and
  display color-space evidence; AppKit supplies EDR state/headroom. A PQ program
  scenario is qualified through the linear extended-range EDR carrier; the row
  must not invent a native PQ link, link bpc, or peak-nit reading. Quartz
  display extents are already physical pixels and must not be rescaled.
- Linux uses the active Vulkan hardware adapter. Wayland
  `color-management-v1` is required for active P3/PQ qualification. X11 can
  qualify managed SDR ICC. DRM/EDID can prove physical capability only; it
  cannot prove compositor HDR is active.

Each supported platform covers the union of SDR sRGB, Display P3, PQ HDR, and
managed ICC, with all three owner-verified evidence lanes on each row: platform
probe, GPU color, and physical Viewer display. One row need not claim scenarios
its native display system cannot prove; for example Linux may use an X11
SDR/ICC row plus a Wayland P3/PQ row. The runtime profile may declare multiple
exact rows per platform for GPU vendors, integrated/discrete adapters, display
types, Wayland and X11, or supported driver releases.

Carrier allocation/reuse is required for Display P3 and HDR scenarios. Direct
SDR sRGB and managed-ICC paths may report `carrier_reuse_observed: false`.

The build manifest is schema 1 and binds one release-candidate ID and source
revision to a finite `artifacts` array. Every entry declares `platform`,
`target_triple`, `package_kind`, relative `path`, `sha256`, an exact
`runtime_image` object with relative `path` and `sha256`, and a
`build_provenance` object with relative `path` and `sha256`. Provenance is schema
1 and repeats source revision, release-candidate ID, target triple, package kind,
product artifact SHA, and runtime-image SHA. Package inputs are immutable regular files (for
example Windows EXE/MSIX, macOS DMG, or Linux AppImage/DEB), not mutable bundle
directories or links.

## Row acquisition

1. Build/package every declared target from one release candidate and sealed
   build manifest. Record each target-specific artifact and build-provenance
   SHA-256.
2. Start from the exact clean source SHA. Assign opaque machine, display, and
   unique row-run identities.
3. Capture the exact OS/build or kernel, architecture, window system,
   compositor, active wgpu adapter/vendor/device/backend, platform-correct
   driver identity, display inventory hash, ICC payload/processor hashes, and
   HDR/EDR state. Every native result must also return the stable output/path
   identity resolved by the same OS enumeration that selected the probe target;
   request-side display labels cannot qualify another monitor.
4. Execute the owner gates serially. Preserve each independently verified lane
   report, normalized raw evidence, and every source-replay entry. Do not run another Cargo/GPU gate
   concurrently on the same checkout, target directory, adapter, or display.
   First run the native producer with the authority-issued single-use challenge:

```powershell
pwsh -File scripts/validation/invoke-platform-display-probe-source.ps1 `
  -RuntimeProfilePath <approved-runtime-profile.json> `
  -CellObservationPath <cell-observation.json> `
  -MachineReportPath <machine-report.json> `
  -CapturePlanPath <platform-capture-plan.json> `
  -AuthorityChallengeManifestPath <pre-issued-platform-challenge.json> `
  -TransitionAcknowledgementDirectory <initially-empty-authority-ack-directory> `
  -ExpectedSourceSha <40-hex-sha> `
  -CaptureId <unique-platform-capture-id> `
  -OutputDirectory <new-platform-source-directory>
```

Each capture-plan entry has a unique `transition_id` and the exact required
HDR/wide-color/transfer state. After the supervisor publishes that capture's
request it waits for the independent transition authority to create the
matching acknowledgement. The acknowledgement binds the challenge, row,
sequence, required state, UTC time, and previous transcript hash. The native
probe starts only after that fresh acknowledgement; its returned state must
match it. This makes mutually exclusive SDR, P3, and HDR desktop states a
serial physical procedure instead of pretending they coexist.

For example, an HDR capture entry declares
`transition_id: "hdr-pq-ready"` and
`required_state: {"hdr_enabled":true,"wide_color_active":true,"active_transfer_function":"PQ"}`.
The corresponding `<transition_id>.json` acknowledgement is schema 1 and
contains the capture/row/sequence/scenario/probe identities, challenge ID,
challenge-manifest and nonce hashes, transition-authority ID,
`acknowledged_at_utc`, the identical `required_state`, and
`previous_transcript_sha256` (empty only for sequence 1). The acknowledgement
directory must be empty when capture starts; pre-created acknowledgements are
rejected. Summary evidence retains `transition_requested_at_utc`; all transition,
start, and finish timestamps use invariant round-trip UTC and replay must prove
`previous finish <= request <= acknowledgement <= process start <= process finish`.

   The cross-platform GPU source supervisor produces the exact measured source
   set and a source-evidence template:

```powershell
pwsh -File scripts/validation/invoke-platform-gpu-color-source.ps1 `
  -CellObservationPath <cell-observation.json> `
  -MachineReportPath <machine-report.json> `
  -ExpectedSourceSha <40-hex-sha> `
  -AuthorityChallengeManifestPath <pre-issued-gpu-challenge.json> `
  -CaptureId <unique-gpu-capture-id> `
  -OutputDirectory <new-gpu-source-directory>
```
5. Launch the runtime image declared by the build manifest, execute the physical
   Viewer scenarios, and bind its process-image SHA-256, the full Display Output
   Contract SHA-256, and operator attestation to the Viewer report. The app
   computes the process hash once when qualification JSONL output is enabled.
6. Re-capture the environment. Construct the strict
   `PlatformQualificationCellObservation` and artifact manifest.
7. Resolve the current host's separately approved verifier-tools manifest and
   set `MONDRIAN_DISPLAY_CONTRACT_REPLAY_EXECUTABLE` / `_SHA256` and
   `MONDRIAN_DISPLAY_CALIBRATION_REPLAY_EXECUTABLE` / `_SHA256` to its exact
   entries. These values are mandatory; row replay has no Cargo fallback.
8. Seal the row:

```powershell
pwsh -File scripts/validation/invoke-platform-driver-display-row.ps1 `
  -RuntimeProfilePath <runtime-profile.json> `
  -CellObservationPath <cell-observation.json> `
  -ArtifactManifestPath <row-artifacts.json> `
  -BuildManifestPath <cross-target-build-manifest.json> `
  -ExpectedSourceSha <40-hex-sha> `
  -OutputDirectory <new-output-directory>
```

The row supervisor verifies byte hashes, source-role closure, direct owner
source replay, and atomic bindings. It does not claim
the complete matrix is qualified. Its row artifact manifest is schema 1 and
binds the row/source/release/build identity, target artifact metadata, one
machine report, before/after environment snapshots, and exactly one normalized
report/raw-evidence/source-replay set for every required owner lane.

## Matrix resolution

After all rows are collected, construct one strict
`PlatformQualificationCampaign` and a matrix artifact manifest containing
exactly one machine report per row and exactly one report/raw-evidence pair per
required lane. In addition to the two Core replay pairs above, set
`MONDRIAN_PLATFORM_QUALIFICATION_REPLAY_EXECUTABLE` and `_SHA256` from the same
approved tools manifest. Resolve it from the same clean source SHA:

```powershell
pwsh -File scripts/validation/resolve-platform-driver-display-matrix.ps1 `
  -RuntimeProfilePath <runtime-profile.json> `
  -CampaignPath <campaign-envelope.json> `
  -ArtifactManifestPath <matrix-artifacts.json> `
  -BuildManifestPath <cross-target-build-manifest.json> `
  -ExpectedSourceSha <40-hex-sha> `
  -OutputDirectory <new-output-directory>
```

The campaign input is a small envelope carrying campaign ID, source revision,
release-candidate ID, and build-manifest SHA. The resolver consumes every
`sealed-row.json`, reconstructs the strict Rust campaign only from those rows,
verifies exact artifact closure, invokes the separately approved
platform-neutral Matrix replay binary, rejects failures, and writes a deterministic, self-verifying
report plus an independently re-verifiable evidence closure. Run the independent
verifier with trust anchors supplied outside the bundle before release admission:

Before verification, a release/capture authority that is independent of the
bundle publisher approves an exact manifest SHA. Schema 1 binds the authority
ID/time, source/release/build/policy/profile hashes, and one row entry per
cell. Each row binds the row-run and before/after environment hashes; each
capture binds kind, capture ID, source-verifier ID, exact source
`{role, sha256, byte_length}` closure, and an approved `operator_id` for the
Viewer lane (`operator_id` is null for non-Viewer lanes). This is the external
HITL trust boundary: internal hashes prove content integrity, not who observed
the physical display.
Platform/GPU captures additionally bind the challenge ID/manifest SHA,
producer ID/image SHA, and session-transcript SHA. Challenges are issued before
execution and are single-use for one capture ID and row run.

```powershell
pwsh -File scripts/validation/verify-platform-driver-display-matrix.ps1 `
  -BundleDirectory <finished-bundle> `
  -ExpectedSourceSha <40-hex-sha> `
  -ExpectedPolicyPath tests/validation/platform-driver-display-matrix.json `
  -ExpectedRuntimeProfilePath <approved-runtime-profile.json> `
  -ExpectedCaptureAuthorityManifestPath <approved-capture-authority.json> `
  -ExpectedCaptureAuthorityManifestSha256 <64-hex-sha> `
  -ExpectedVerifierToolsManifestPath <approved-verifier-tools.json> `
  -ExpectedVerifierToolsManifestSha256 <64-hex-sha> `
  -ExpectedReleaseCandidateId <release-candidate-id> `
  -ExpectedBuildManifestSha256 <64-hex-sha>
```

The verifier checks the external source/policy/profile/release/build/capture-authority/verifier-tool anchors,
replays every owner lane verifier and original source Adapter over bundled bytes, reruns the exact ignored
Matrix evaluator, and requires the regenerated report bytes to
match the bundled verdict. The Matrix Module owns profile closure, platform-correct
driver identity, exact environment and receipt correlation, scenario semantics,
three-state verdicts, canonical ordering, and report identity. PowerShell owns
only process, filesystem, hash, and clean-checkout orchestration.

The verifier-tools manifest is schema 1 and is approved outside the bundle. It
binds `authority_id`, `approved_at_utc`, `source_revision`,
`release_candidate_id`, and exactly three `{id, path, sha256}` entries:
`display_contract_replay`, `display_calibration_replay`, and
`platform_qualification_replay`. Paths are relative to the manifest directory;
absolute, escaping, linked, missing, duplicated, or hash-mismatched tools fail
before any owner replay.

All closure files are streamed into a private create-only verification
snapshot before owner replay. The externally pinned Git revision is retained as
one link-free archive; the exact owner/source verifier scripts, producer-script
identities, and GPU gate profile are read once from that archive into memory.
Runtime Cargo execution is forbidden. A separately approved schema-1 verifier
tools manifest binds source/release identity and the exact SHA-256 plus relative
path of `display_contract_replay`, `display_calibration_replay`, and
`platform_qualification_replay`. Only those three measured executables may
produce replay results. The complete evidence admission set is rehashed around
owner replay and before success, and the original bundle is rehashed again.

The matrix artifact manifest is schema 1 and contains exactly one
`sealed_rows` entry for every runtime-profile cell. Each entry binds the cell ID
and the relative path plus SHA-256 of `sealed-row.json`, the cell observation,
and the row artifact manifest. The resolver consumes these row seals; it never
accepts a parallel, unsealed campaign cell list. It copies every admitted
authority, machine, lane, raw, source capture, environment snapshot, provenance, target package, and runtime image into
an external create-only bundle. The bundle verifier rejects missing, extra,
linked, oversized, or changed files and does not trust bundle-owned identities.

## Current qualification state

The software contract and deterministic evaluator can be tested on any host.
This workstation cannot produce macOS/Metal, Linux/Vulkan Wayland HDR, or the
required physical P3/PQ/ICC rows. Until the exact real-device campaign is
captured, COL-046 remains hardware HITL and no macOS/Linux commercial display
claim is implied.
