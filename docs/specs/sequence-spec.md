# Sequence Spec

A sequence is an editorial timeline with video/audio tracks and render settings.

## Identity

- `id: SequenceId`
- `name: String`
- `role: Editorial | NestedComposition`

## Settings

`SequenceSettings` includes:

- editing mode preset
- resolution
- frame rate
- pixel aspect ratio
- field order
- video display format
- audio sample rate/channels/layout/display format
- start timecode frame
- preview render settings
- working color space
- auto tone-map flag
- action/title safe margins
- sequence color management

## Validation

Sequence settings must validate supported frame rates, resolution bounds, audio sample rates/layout consistency, non-negative start timecode, preview scale, and HDR/ACES constraints.

## Defaults

Default sequence:

- FHD
- 25 fps
- square pixels
- progressive
- stereo 48 kHz
- Rec.709 working space
- V1-V3 and A1-A3

## Render Cache

`SequencePreviewSettings` controls preview format, resolution scale, and cache enablement. Cache policy must not alter timeline semantics.
