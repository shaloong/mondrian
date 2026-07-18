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

Only self-owned, deterministically generated, public-domain, or explicitly
licensed material may enter the canonical manifest. A redistribution-restricted
fixture may remain outside repository history only when its test usage rights
are verified; record that restriction as `prohibited`. Never replace an asset
in place while retaining its fixture ID.

Downloaded or inherited media with unverified rights is not a fixture. Keep it
ignored and use it only with explicitly ignored/manual tests through a local
path. Do not add its filename, hash, metadata, or expected result to the
canonical manifest, and do not use it as release or professional acceptance
evidence.

Run the public/PR gate with:

```powershell
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Pr
```

`Nightly` and `Release` tiers require every restricted asset to be present. A
missing required asset is reported as `blocked`, while a schema, size, or hash
mismatch is `failed`; neither state is a pass.
