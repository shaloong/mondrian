---
status: accepted
---

# Carry image sampling with CPU working values

CPU value/cache propagation, native OpenFX sampling and demand-driven unit-scale
media materialization implemented. Integrated native runtime qualification pending.

## Problem

CPU Timeline effects run on source pixels before the placement transform. Source
SAR currently survives only in that later affine; the OpenFX adapter instead
fills image PAR from the owner project's PAR. A local adapter override cannot
preserve geometry through owned/view conversion, nesting or persistent cache
restore. Project geometry from ADR 0013 remains a separate owner contract.

## Decision

- Carry original resolved sample aspect ratio and positive X/Y render scales
  with the CPU working image value, then Effects borrowed/owned values. Keep
  checked, reduced rational scales; do not add an implicit square/unit default
  or an unknown-value fallback. Reject invalid serialized values.
- At a proven full-raster resampling boundary, derive scales once from actual
  payload extent divided by the original reference extent. Source interpretation
  comes from `ResolvedPictureGeometry`, including placement overrides. Neither
  proxy-container SAR nor a Reduced divisor proves the resulting sampling.
- Preserve the actual `f32` scale used by Basic Title rasterization exactly;
  do not recompute an ideal ratio or change existing layout precision. Integer
  reference extents alone cannot describe its cropped, fractional author span.
- Sampling describes density, not origin, crop, orientation or provenance.
  Existing title/ROI/placement wrappers retain those responsibilities. Do not
  identify unit scale with proof of an uncropped full image.
- Bind sampling when first creating a CPU working value, including eager numeric
  DataTexture preparation, before sharing its lazy cache. Color/alpha operations
  preserve it. A compositor explicitly produces its target canvas sampling;
  nested canvases use their own owner. NoImage needs an explicit output target.
- Keep the existing processor bank, graph execution and cache. Image sampling
  participates in image-cache identity and output admission, not immutable native
  owner identity. Project changes still replace the owner; dynamic image PAR can
  reuse it. The existing resident creates the native instance lazily on first
  Render, after receiving the actual frame. Derive Create/clip-preference sampling
  there from that value, or NoImage's explicit output staging without a Source;
  do not add an initial-sampling field to factory preparation or its identity.
- Extend the current persistent cache identity envelope and artifact together.
  Publish and restore compare the payload's sampling with the requested contract.
  Reject old artifacts and recompute; never restore missing metadata as square.

## Migration and verification

This extends CPU value contracts across Core, Effects, Renderer, Preview, Export
and the existing render cache. It does not change authored project files, time,
audio routing or GPU formats. Freeze concrete APIs before splitting writers;
integrate complete producer/consumer slices with no placeholder defaults.

First fix Full decode silently resizing odd rasters. Then verify PAR/scale
propagation through color, alpha, Effects, nested output and both cache publishers;
test cache mismatches and old-artifact refusal. Exercise real unequal source and
project PAR, owner reuse and NoImage. Current OpenFX scale1 admission must be
matched by demand-driven full-source decode; merely rejecting proxy/Reduced at
the consumer would regress playback. GPU/window/temporal support remains separate.

Windows CPU controls pass: Effects 340, affected Renderer 205, render cache 19,
affected App 688 and Export 3 (including two actual FFmpeg controls). These cover
value propagation and sampling mismatch refusal; they do not qualify native
unequal-PAR rendering, GPU interoperability or other platforms.

The shared provider requirement and pre-processing scale admission now pass 352
ordinary Effects controls and strict checks. Native v11 carries actual sampling;
Unit media uses the original Full CPU source and complete Clip graph before the
existing spatial compositor. Unit temporal source materialization remains an
explicit unsupported boundary. Legal Unit title crops, authored nested canvases
and full accumulators retain their sampling; Reduced inputs require their own
correct producer before native execution can be admitted.
