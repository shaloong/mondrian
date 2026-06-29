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

## Unknown Media

When detected media color space is missing, `MissingColorMetadataPolicy` resolves it as:

- Assume Rec.709
- Assume sequence working space
- Reject media

Clip-level `MediaInterpretation.color_space_override` takes precedence over detected metadata.

## Display and Export

Display transforms belong at preview presentation. Export transforms belong at export encoding/tagging. Do not bake display transforms into timeline source data.

## HDR/SDR

HDR output spaces include Rec.2100 PQ/HLG. Tone mapping is required when scene/HDR working data targets SDR output. HDR metadata can only be preserved for HDR output spaces.
