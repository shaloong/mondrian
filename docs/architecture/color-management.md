# Color Management

Mondrian supports a built-in color engine and optional OCIO.

## Engines

- `ColorEngine::MondrianSmart`: pure-Rust built-in conversion path.
- `ColorEngine::Ocio`: delegates transforms to an OCIO config when loaded.

If OCIO is selected but no config is available, current conversion code warns and falls back to MondrianSmart for conversion. UI should surface availability clearly.

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
