# Test Fixtures

Place downloaded sample media and golden references under this directory.

Recommended layout:
- `tests/fixtures/color/` for color golden samples and reference frames
- `tests/fixtures/lut/` for `.cube` LUT files and LUT cache tests
- `tests/fixtures/export/` for export validation samples and metadata references
- `tests/fixtures/sequence/` for nested sequence and timeline interpretation samples

Suggested naming:
- `*_src.*` for source material
- `*_golden.*` for expected reference outputs
- `*_hdr.*` / `*_sdr.*` for dynamic-range variants
- `*_legal.*` / `*_full.*` for range variants
- `*_rec709.*`, `*_rec2020.*`, `*_hlg.*`, `*_pq.*`, `*_log.*` for color-space variants

Keep large files out of the repo history unless they are required for deterministic tests. If a sample is too large, prefer a short manifest entry plus a download script or external reference note.
