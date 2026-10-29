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

Windows CPU controls pass: Effects 352, affected Renderer 207, render cache 19,
affected App 694 and Export 4 (including two actual FFmpeg controls). The App
selection and existing exclusions are unchanged. These cover value propagation,
Unit original-source requests through transitions/lookahead, odd-raster decode,
effects/masks and sampling refusal; they do not qualify native unequal-PAR
Media Preview rendering, GPU interoperability or other platforms.

The shared provider requirement and pre-processing scale admission now pass 352
ordinary Effects controls and strict checks. Native v11 carries actual sampling;
Unit media uses the original Full CPU source and complete Clip graph before the
existing spatial compositor. Finite temporal Media binds its original source
domain as described in ADR 0016; native temporal access remains unsupported.
Legal Unit title crops, authored nested canvases
and full accumulators retain their sampling; Reduced inputs require their own
correct producer before native execution can be admitted.

CPU affine composition tests physical raster coverage before converting to texel
centers, then clamps interpolation taps independently. GPU image coverage uses
that same physical support `[0, size)`, retaining its existing Nearest sampling.
The recorder's damage encloses complete transformed image cells, intersected
with conservative crop bounds whose upper closure survives reflection. Pixel
sampling coordinates, point effects and GPU resource ownership are unchanged.
Persistent cache identity stays v6 and artifact layout v3; private viewer key v5
invalidates former GPU coverage. GPU outputs do not publish persistent Working
artifacts, so this correction needs no second persistent cache invalidation.

Windows RTX3050/DX12 libtest passes all twelve literal Float32 border cases,
including both fragment paths, scaling, translation, a half-open diamond and
Nearest two-texel control. Renderer 246, App 694, Export 4 and cache 19 affected
ordinary controls pass, including literal reflected/rotated crop centers.
Independent source and actual-result review retain the original execution and
cleanup budgets. These results qualify the tested compositor paths; native OFX
GPU interoperability, the normal Product's new shader, realtime performance and
other platforms require their own verification.

Decoded storage and source transfer are independent. Physical FLOAT reaches the
supported planar-F32 reader or an explicit refusal before integer paths. Linear
integer RGB uses the existing native-depth reader and float resize; one RGB
contract constructor retains matrix, range and alpha while deriving data,
linear or encoded interpretation. YUVA12 preserves alpha; other alpha-bearing
YUV layouts are explicitly unsupported. The optional RGBA8 decoder declines
linear and physical-float inputs. Windows default+validation Preview 219 and
the affected Renderer 207, cache 19, App 694, Export 4 and ordinary OpenFX 114
controls pass with independent review. These ordinary suites do not qualify
GPU, realtime performance or other platforms.

A separate owned Windows gate passes original 3x5 PPM decode, Clip PAR4/3 and
Unit native processing before CPU spatial composition to 2x2 and 3x2 Preview
rasters. Every pixel and sampling value, one retained native owner, advancing
invocations, unchanged author state and consuming cleanup pass. This fixed-frame
bridge does not qualify full Timeline editing, Export or other plugins.
