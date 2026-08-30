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
