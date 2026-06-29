# Color Space Spec

Supported color spaces currently include:

- Rec.709
- Rec.2100 HLG
- Rec.2100 PQ
- sRGB
- Rec.2020
- DCI-P3
- Apple Log
- S-Log3
- ARRI LogC4

## Engine

`ColorEngine` is either MondrianSmart or OCIO. OCIO requires a loaded config; MondrianSmart is always available.

## Pipeline

Color conversion is represented by `ColorPipeline`:

- input
- working
- output
- tone_map
- engine

`ColorTransformPlan` expands this to management engine, transfer decode, primary conversion, optional tone map, and output conversion.

## Media Interpretation

Clip-level media interpretation may override detected color space. Missing metadata policy is owned by sequence color management.

## Display vs Export

Display transform is for preview output only. Export transform and tags are export-path responsibilities. Neither should modify source media or clip data.
