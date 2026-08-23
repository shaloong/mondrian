# Color Fixtures

Put professional color samples and golden frames here.

Use this folder for:

- Rec.709 / sRGB SDR references
- Rec.2020 / HLG / PQ HDR references
- Log / wide-gamut source clips and expected frames
- Legal vs full range comparison material
- Golden PNG/EXR frames used by tolerance tests

Recommended structure:

- `source/` for input clips
- `golden/` for expected frames
- `metadata/` for companion JSON/YAML notes

When adding new cases, prefer one source sample per issue family and keep a matching golden frame next to it.

`metadata/mondrian-standard-quality-corpus-v1.json` is the committed, versioned
objective stimulus contract for Mondrian Standard v1. It pins the Standard
package digest and covers 22 quality categories with generated float recipes,
including neutral/near-black/over-white ramps, negative and extended gamut,
high-saturation hue boundaries, SDR/HDR mixes, alpha edges, spatial impulses,
10-bit ramps, and legal/full-range code contracts. Its ColorChecker 2005 CIE xyY
data is redistributed from Colour Science 0.4.7 under BSD-3-Clause and converted
from D50 xyY to linear Rec.2020 test stimuli before rendering; attribution and
license links are embedded in the strict JSON contract.

This corpus is not a claim of visual equivalence to commercial applications.
Independent application exports must use the external reference descriptor and
retain their own producer version, stimulus hash, payload hash, output encoding,
reference white, and nominal peak.
