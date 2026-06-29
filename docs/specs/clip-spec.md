# Clip Spec

A clip is a timeline instance that references an asset or nested sequence.

## Required Fields

- `id: ClipId`
- `kind: ClipKind`
- `asset_id: AssetId`
- optional `nested_sequence_id`
- `interpretation: MediaInterpretation`
- `position`
- `duration`
- `source_in`
- `source_out`
- `transform`
- `speed`
- `effects`
- `masks`
- optional `linked_clip`
- `is_disabled`
- optional `blend_mode`
- optional `label`
- optional `solid_color`

## Kinds

- `Media`: file-backed media.
- `AdjustmentLayer`: applies effects to lower accumulated image.
- `NestedSequence`: references another sequence.
- `SolidColor`: generated color source.

## Timing

Timeline range is `[position, position + duration)`. `timeline_to_source_time()` subtracts clip position, applies speed mapping, then adds `source_in`.

## Built-In Properties

Built-in transform, speed, blend mode, and solid color properties are not removable through property mutations. UI may hide or reset them but cannot delete them.

## Link Semantics

`linked_clip` keeps audio/video clips synchronized. Link state must not imply both clips share effects, transform, masks, or selection state.
