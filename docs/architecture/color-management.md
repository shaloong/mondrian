# Color Management

Mondrian has one color-management pipeline. Mondrian Standard and Custom OCIO
both resolve color transforms through OCIO config / processor. The bundled
Mondrian default OCIO config is required for Standard mode; missing config or
processor failures must surface as errors instead of falling back to another
color science.

## Engines

- `ColorEngine::MondrianSmart`: productized Standard/Simple policy over the
  Mondrian default OCIO source.
- `ColorEngine::Ocio`: explicit OCIO mode over a selected environment,
  built-in, or path source.

Explicit OCIO mode must load its selected config successfully. It must not
silently fall back to a different color science. Mondrian Standard follows the
same rule for `mondrian_default_ocio_v1`.

## Project and Sequence

`ProjectColorManagement` stores the project-level engine. `SequenceColorManagement` can inherit from the project or override its own engine and policies.

Important fields:

- `workflow`: DisplayReferred, SceneReferred, Aces
- `missing_metadata_policy`
- `nested_processing`
- `output_color_space`
- `video_range`
- `export_bit_depth`
- HDR metadata preservation fields

## Working Space

`SequenceSettings.color_space` is the timeline working color space. Media input transforms resolve from clip/media interpretation into this working space. Output/display transforms convert from working/output context to the destination.

`ColorPipeline` execution is source -> working -> output. The management engine
dispatch must preserve that full chain; it must not collapse a timeline pipeline
to source -> output when a sequence working space is available.

OCIO execution also follows source -> working -> output. The Standard mode UI
can hide OCIO details from normal users, but the backend still routes through
the Mondrian default OCIO source and fails closed when that source is missing.

## Color Encoding Contract

`ColorSpace::encoding()` is the canonical metadata source for color
primaries, transfer characteristic, matrix coefficients, and whether the space
is display SDR, display HDR, or camera log. Conversion code, preview
diagnostics, validation, and export tagging should read this contract instead
of maintaining separate ad hoc mappings.

`ColorSpace::ffmpeg_tags()` is derived from that contract and only returns tags
for standardized delivery/monitoring spaces. Camera-log acquisition spaces such
as Apple Log, S-Log3, and ARRI LogC4 currently do not emit FFmpeg delivery tags,
because writing guessed Rec.709 tags would mislabel the exported media.

Camera-log output is treated as a professional intermediate path. Export
validation rejects consumer delivery codecs for camera-log output and only
allows 10-bit-or-higher MOV/MXF ProRes configurations until richer metadata
carriage is implemented.

## Unknown Media

When detected media color space is missing, `MissingColorMetadataPolicy` resolves it as:

- Assume Rec.709
- Assume sequence working space
- Reject media

Clip-level `MediaInterpretation.color_space_override` takes precedence over detected metadata.

## Display and Export

Display transforms belong at preview presentation. Export transforms belong at export encoding/tagging. Do not bake display transforms into timeline source data.

`SequenceSettings::root_preview_color_context(...)` builds the monitor
presentation context with a caller-provided display/output color space.
`SequenceSettings::root_export_color_context(...)` builds the delivery context
from the sequence output color space. Callers must choose one of these explicit
entry points instead of using a generic root render context.

Preview and export may therefore target different output color spaces while
sharing the same working color space, engine inheritance, workflow,
missing-metadata policy, and nested-processing policy.

## HDR/SDR

HDR output spaces include Rec.2100 PQ/HLG. Tone mapping is required when scene/HDR working data targets SDR output. HDR metadata can only be preserved for HDR output spaces.
