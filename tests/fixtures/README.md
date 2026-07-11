# Test Fixtures

Fixture identity and expected metadata are governed by
`tests/validation/corpus-manifest.json`. A filename alone is not a test contract.

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

Keep large or redistribution-restricted files out of repository history. Store them
at the manifest path locally and validate their byte length and SHA-256 before use.
Never replace an asset in place while retaining its fixture ID.

Run the public/PR gate with:

```powershell
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Pr
```

`Nightly` and `Release` tiers require every restricted asset to be present. A
missing required asset is reported as `blocked`, while a schema, size, or hash
mismatch is `failed`; neither state is a pass.
