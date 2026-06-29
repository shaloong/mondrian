# Export Spec

Exports are configured through `ExportConfig`.

## Preset

`ExportPreset` contains:

- name
- container
- video codec config
- audio codec config
- optional output resolution

Implemented containers include MP4, MOV, MKV, GIF, MXF, and WebM. Implemented video configs include H.264, H.265, AV1, ProRes, and GIF. Implemented audio configs include AAC, PCM, and MP3.

## Inputs

`ExportInput` supports:

- `File`: direct transcode path
- `Timeline`: timeline render then encode path

Timeline export carries sequence, sequence collection, asset paths, asset color spaces, range, and project color management.

## Range

`TimelineExportRange`:

- `SequenceInOut`
- `EntireSequence`
- `WorkArea { start_frame, end_frame_exclusive }`

## Semantics

Export must use the same timeline render interpretation as preview. Export may differ in scheduling, cache lifetime, encoding format, and final color/output transform.
