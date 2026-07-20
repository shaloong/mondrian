# Asset Model

Assets are project-library records. Timeline clips reference assets by `AssetId`.

## Asset Kinds

Current persisted `AssetKind` discriminants are:

- `Video`
- `Audio`
- `AdjustmentLayer`
- `SolidColor`

This list describes model identities only. Import/probe, decode, relink,
thumbnail, proxy, timeline, preview and export capability must be reported and
verified separately for each real codec/container or generated kind.

Planned/spec-level kinds:

- `NestedSequenceAsset`: a reusable sequence reference entry if needed by future UI/library workflows.
- `OfflineAsset`: missing media placeholder retaining identity and relink metadata.
- `ProxyAsset`: derived proxy/cache representation, not a replacement for the source asset.

## File Assets

File assets store canonical paths and `MediaInfo` metadata from FFmpeg probing. Relink must validate media kind compatibility.

## Generated Assets

Generated assets use synthetic paths:

- `mondrian://adjustment-layer/<asset_id>`
- `mondrian://solid-color/<asset_id>`

Adjustment layer and solid color parameter state belongs to the clip instance, not the reusable asset record.

## Design Rule

Prefer virtual asset records over optional `asset_id` branches in timeline clips. A clip should normally have an `AssetId`; kind-specific generated data can be represented by the asset kind and clip instance fields.
