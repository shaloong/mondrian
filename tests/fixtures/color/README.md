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
