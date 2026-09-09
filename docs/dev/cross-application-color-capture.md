# Cross-Application Color Capture

This runbook acquires the local-restricted Blender, DaVinci Resolve, and Adobe
Premiere Pro artifacts consumed by the sealed cross-application qualification.
It is a capture procedure, not a second color implementation. The checked-in
stimulus manifest and a reviewed runtime profile are the authority.

## Common capture contract

Create a new profile edition whenever any application build, native Project,
preset, Adapter, OCIO config, stimulus, decoder, metric, or tolerance changes.
Do not use version ranges, `latest`, default settings, automatic resize/crop,
automatic color detection without a recorded override, lossy EXR compression,
or screenshots as pixel evidence. Preserve one run identity across all
artifacts; separate runs cannot be merged.

For every artifact retain and hash:

- exact application version/build and executable or signed installation inventory;
- OS, GPU, and driver inventory;
- application-native Project and every referenced preset/config;
- complete actual input, working, display/output, range, tone-map, gamut-map,
  Alpha, raster, cadence, and frame settings dump;
- acquisition Adapter source/package and exact version;
- encoded payload plus independent decoder/channel/sample/metadata dump;
- operator attestation and any UI screenshots used only to support that attestation.

The decoder must not apply ICC, gamma, chromaticity, premultiplication,
orientation, resize, or range conversion. Tags and pixels are separate evidence.
Generic OpenEXR is not automatically an ACES Image Container. Fix part/tile,
data/display windows, channel names/order, sample representation, chromaticities,
and lossless compression explicitly.

## Blender Adapter

Use an exact supported Blender build with `--background`, `--factory-startup`,
and a reviewed Python capture script. The script must load the native `.blend`,
verify every requested display/view/colorspace exists, set scene view/display/
look/exposure/gamma and output format/bit depth explicitly, render the exact
frame, then serialize the observed settings plus `bpy.app.version_string` and
build identity. Pin the external OCIO config bytes through `OCIO`; a missing
view is a terminal failure and cannot fall back to another view.

Scene-linear EXR and display-transformed output are separate lanes. Blender's
Windows file-output evidence does not qualify its Viewer, OS HDR, or physical
display behavior.

Official references:

- [Blender color management](https://docs.blender.org/manual/en/4.5/render/color_management.html)
- [Blender command-line arguments](https://docs.blender.org/manual/en/4.5/advanced/command_line/arguments.html)
- [Blender output properties](https://docs.blender.org/manual/en/4.5/render/output/properties/output.html)
- [ImageFormatSettings](https://docs.blender.org/api/current/bpy.types.ImageFormatSettings.html)

## DaVinci Resolve Adapter

Use the scripting README distributed with the installed Resolve build as the
runtime authority and hash it into evidence. The Adapter may use Python/Lua and
Resolve's `-nogui` host, but Resolve must be running and licensed for the
selected workflow. Freeze a reviewed Project/DRP and named render preset.
Record `GetVersionString()`, the complete Project/Timeline setting snapshots,
render settings, preset identity, job range, and every Boolean result from the
scripting Interface. A false or unavailable setting is terminal.

Use an exact Mark In/Out single-frame job or image-sequence range; current
playhead/still state is not frame identity. Color-space/gamma carrier tags and
the pixel transform are separate evidence. `Same as Project` is acceptable only
when the Project output setting was independently captured.

Official references:

- [DaVinci Resolve support and developer packages](https://www.blackmagicdesign.com/support)
- [DaVinci Resolve Colorist Guide](https://documents.blackmagicdesign.com/UserManuals/DaVinci-Resolve-20-Colorist-Guide.pdf)
- [DaVinci Resolve supported codecs](https://documents.blackmagicdesign.com/SupportNotes/DaVinci_Resolve_20_Supported_Codec_List.pdf)

## Adobe Premiere Pro Adapter

Premiere capture is UI-hosted. The public UXP Interface can open/import Projects,
export a Sequence frame, and drive encoder presets, but it does not expose a
complete strongly typed setter/getter surface for Sequence working/output color,
tone mapping, gamut compression, and input overrides. Use a signed `.prproj`,
Sequence preset, and `.epr`; an operator must verify and attest the Color Setup,
Working/Output Space, input override/Preserve RGB, tone-map, gamut-compression,
maximum-bit-depth, and linear-composite controls for every run.

Treat `exportSequenceFrame` and Media Encoder/AME export as separate lanes;
neither is evidence for the other's precision, tag, or color behavior. Do not
invoke `PProHeadless.exe` or invent an undocumented command-line contract.

Official references:

- [Premiere UXP changelog](https://developer.adobe.com/premiere-pro/uxp/changelog/)
- [Exporter Interface](https://developer.adobe.com/premiere-pro/uxp/ppro-reference/classes/exporter)
- [EncoderManager Interface](https://developer.adobe.com/premiere-pro/uxp/ppro-reference/classes/encodermanager)
- [ProjectColorSettings Interface](https://developer.adobe.com/premiere-pro/uxp/ppro-reference/classes/projectcolorsettings)
- [Premiere color management](https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/about-color-management.html)

## Qualification and claim boundary

After review, store the complete bundle below the prepared runner's ignored
`tests/fixtures/large/cross-application/` root and invoke the sealed supervisor
documented in `reference-validation.md`. A missing application/case is
`incomplete`; unsupported is recorded as capability evidence but cannot satisfy
a required profile row. Numeric failure is `failed` even when other application
artifacts are missing.

The final report proves exact file-output parity for the declared cases only.
It does not qualify vendor-native creative tone mapping, Viewer/ICC/HDR display,
reference monitors, GPU/driver/platform matrices, SDI, Camera RAW, third-party
effects, fonts/motion graphics, or interchange structure.

## Local Blender/Premiere scope and executable Blender capture

The user-selected local matrix uses
`tests/validation/cross-application-color-blender-premiere-qualification.json`
and its separately hashed stimulus. Runtime profile schema 2 must explicitly
set `producer_scope: blender_and_premiere`; it cannot omit Resolve from a
legacy full-matrix profile. Report schema 2 retains the scope.

`invoke-blender-color-capture.ps1` launches the pinned Blender executable with
background/factory startup, script autoexecution disabled, and a bounded
wall-clock deadline. The parent retains read-only file leases on the request,
script, native project, dependencies, and OCIO config/LUTs before startup. The
Python adapter verifies exact build, rejects missing/unknown request fields,
loads the native project, rejects unfrozen external resources, reads back the
requested raster/cadence/view/format settings, and renders the requested frames.
It retains actual per-artifact hashes and settings on successful and failed
acquisition. The launcher records native return and input lease release.

Request schema 1 fields are `run_id`, `expected_version`, `expected_build`,
`project`, `project_sha256`, `ocio`, `ocio_sha256`, `dependencies` (an array of
`path`/`sha256` objects), and bounded `cases`. Each case specifies `case_id`,
`frame_index`, `width`, `height`, `rate_numerator`, `rate_denominator`,
`display`, `view`, `look`, `exposure`, `gamma`, `format`, `depth`, and
`linear_output_space`. The supported exact capture formats are PNG/RGBA8
and lossless ZIP OpenEXR/RGBA Float32. Display PNG uses null linear output
space; EXR explicitly selects the Blender output linear colorspace.

A local adapter smoke loaded an actual `.blend` and rendered both PNG and
Float32 Rec.2020 EXR with Blender 5.1.1 build b70da489d7f4, with clean native
exit. This verifies the acquisition adapter only. The smoke's default project
is not the analytic qualification stimulus and is not a passing COL-045 corpus.
Premiere 24.0.0 build 58 requires its CEP/ExtendScript UI-hosted capture route;
the later UXP API must not be assumed present in this installed version.

## Local native capture findings, 2026-09-06

The local Blender/Premiere attempt produced actual native files and a diagnostic
inventory, not a qualified cross-application corpus. The inventory is
`.scratch/endurance-batch/cross-app-local/local-native-capture-manifest-v2.json` and
binds all 120 input hashes, both executable identities, native projects, OCIO,
Blender request/launcher/capture receipts, Premiere saved native readback, actual
output hashes and independent diagnostics. It does not infer output frame 119
from an existing input or a successful frame-17 export.

Blender 5.1.1 build `b70da489d7f4` uses the corrected `frame_offset = -1` for the
zero-numbered input sequence and file `color_management = OVERRIDE`, including
the file's own display/view settings. `blender-capture-v2` independently identifies
frames 0 and 17; the actual Float32 ZIP EXR header is `lin_rec2020_scene`. Its RGB
matches the Rec.2020 premultiplied source to 1.91e-6. Native EXR output remains
premultiplied and the compositor discards hidden RGB at alpha zero. This is
recorded in `native_output_contract`, not relabeled as the required straight
coverage. The prior frame+1/Rec.709 capture is retained as failed evidence.

Premiere Pro 24.0.0 build 58 was driven through its native UI. The saved
`premiere-native-readback-v2.json` matches the current native project's SHA256.
Its selected sequence has UID `79cb2d83-6177-4c92-9d28-9731c42ab156`, with video
track group ObjectID 79. Sequence maximum-depth settings are retained on the
Sequence object, not inferred from unrelated VideoSettings objects. The native
UI observed 64x64, 24000/1001 cadence (10594584000 ticks per frame), Rec.709,
auto tone mapping disabled, maximum depth enabled, linear compositing enabled,
and explicit straight alpha override. The EXR source's color controls reported
Not color managed and did not permit assigning Linear Rec.2020. Private project
readback is diagnostic evidence only; no private project fields were edited.

Independent FFmpeg Float32 EXR and Pillow PNG decoding of
`premiere-sdr-frame17-v2.png` proves the following:

- All eight repeated identity markers select frame 17; alpha is exactly
  0/64/128/255, as in Blender's PNG.
- Every RGB code equals rounded/clipped per-channel `source_linear^(1/2.4)`.
  All 12288 RGB samples match that diagnostic hypothesis with zero code error.
  The Rec.2020-to-Rec.709 matrix and target sRGB transfer were not applied.
- Premiere retains 1736 nonzero RGB samples at zero alpha, whereas Blender's
  compositor output retains none. The two failures must not be conflated.
- The SDR row therefore **fails** the target sRGB contract despite correct
  native frame/alpha capture. The paired PQ row is admission-time **NotRun**:
  the installed EXR importer cannot establish the required source Rec.2020
  interpretation. Frame 119 has no captured producer output and remains NotRun.

Adobe's [24.3 beta announcement by an Adobe employee](https://community.adobe.com/announcements-732/now-released-override-media-color-space-now-available-for-all-media-formats-except-raw-formats-313719)
explains that previously unmanaged formats disabled both source override and
input LUT controls, and that broader override support was introduced after the
installed 24.0 build. This supports the observed version limitation; it does not
prove exact Linear Rec.2020 EXR support in any later build. Current official
[clip color management instructions](https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/color-management-using-clip-modify.html)
use Lumetri Color > Settings > Source Clip or Modify Clip > Color. Adobe also
[qualifies still-format interoperability consistency](https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/premiere-after-effects-color-management-compatibility.html),
including EXR; Dynamic Link is not an automatic qualification workaround.

The executable next step is to use an approved, exactly pinned native build that
actually exposes a source override for this EXR, verify that **Linear Rec.2020**
exists (a gamma-encoded Rec.2020 option is insufficient), and freeze a new native
project/config edition. Verify source, working and output transforms plus alpha
before admitting the paired SDR/PQ run; then reacquire and independently decode
frames 0/17/119. If the exact input or alpha contract is unavailable, retain that
capability failure. This attempt did not upgrade applications, change plugin
security, insert an unreviewed conversion LUT, rewrite native project internals,
normalize output images, or fabricate the missing frame-119 evidence.


## Ordinary Mondrian native capture, 2026-09-06

`mondrian-native-03/mondrian-capture.json` and the separate
`mondrian-native-03-independent.json` under the local batch directory bind all
120 source EXRs, the saved ordinary project, native executable, manifests and
three actual exports. Repeating image markers independently identify frames
0, 17 and 119. An archived producer executable retains the same SHA for review.

Frame-0 Float32 Rec.2020 EXR preserves all nonzero-alpha source samples exactly,
including -0.125, 16.0 and coverage 0.25/0.5/1.0. Fully transparent RGB becomes
zero through the existing canonical source-over composite; the independent
report retains this difference from the source's hidden RGB. Frame-17 SDR RGB
is exact for opaque pixels and differs by at most one code for nonzero partial
coverage. A separate native GPU diagnostic localizes midpoint alpha 127 (CPU 128)
to the Float32-to-UNORM render-target conversion: all 4096 Float32 alpha samples
are bit-exact; all 1024 midpoint pixels differ only after UNORM8 storage.
These are measured diagnostics, not a silently relaxed comparison threshold.

The legal PQ path exports opaque UInt16 TIFF and independently checks its
actual tags, strips, precision and normalized Float32 JSON against native sample
codes. This is not native Float32 output or proof of the requested authored
203-nit reference white. The missing Premiere PQ artifact remains NotRun.

The isolated report `gpu-alpha-native-01.json` records the actual RTX 3050 Laptop
GPU, DX12 backend and driver 32.0.15.9159, plus every source/CPU/GPU sample.
[Microsoft's conversion rules](https://learn.microsoft.com/en-us/windows/win32/direct3d10/d3d10-graphics-programming-guide-resources-data-conversion)
explicitly permit float-to-UNORM integer-side tolerance for D3D versions including
12. This explains why a midpoint byte is not a bit-exact portable GPU contract;
it does not relax this campaign's comparison threshold or certify other drivers.
The diagnostic bypasses upstream composition and retains `qualified: false`.
No production color transform or output resource format was changed to conceal
this observed interoperability difference.
