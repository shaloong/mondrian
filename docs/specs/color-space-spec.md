# Color Space Spec

Supported color spaces currently include:

- Rec.709
- Rec.2100 HLG
- Rec.2100 PQ
- sRGB
- Rec.2020
- Display P3 (P3-D65 primaries, sRGB transfer)
- Apple Log
- S-Log3
- ARRI LogC4

## Engine

`ColorEngine` is either MondrianStandard or OCIO. OCIO requires a loaded config.
MondrianStandard loads Mondrian's embedded `mondrian_default_ocio_v1` config and
must fail closed if that asset cannot parse or cannot produce the requested
OCIO processor.
Explicit OCIO sources must not fall back to another source. In particular,
`OcioConfigSource::Environment` means the `OCIO` environment variable itself;
if it is unset or points to a missing file, the selected source is invalid.

The embedded Standard config is also exposed through
`mondrian_default_ocio_contract()`. That contract is the authoritative list of
Mondrian `ColorSpace` to OCIO name mappings plus the default and supported
display/view pairs for product UI and renderer integration.

## Pipeline

Color conversion uses three distinct contracts rather than one untyped
source/working/output struct:

- `RenderInputTransform`: encoded source identity to linear working identity.
- Effects/compositing: linear pixels carrying `WorkingColorSpace` only.
- `RenderOutputColorBoundary`: working identity to encoded display or export
  identity, with display/view tone mapping allowed only here.

CPU and GPU OCIO processor requests use `OcioColorSpaceIdentity`, so an encoded
`ColorSpace` cannot alias a `WorkingColorSpace` in processor caches or APIs.

## Media Interpretation

Clip-level media interpretation may override detected color space. Missing metadata policy is owned by sequence color management.

## Display vs Export

Display transform is for preview output only. Export transform and tags are export-path responsibilities. Neither should modify source media or clip data.
