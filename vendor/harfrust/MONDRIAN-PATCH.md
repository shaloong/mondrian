# Harfrust 0.5.2 compatibility patch

This directory contains the registry source of `harfrust` 0.5.2 under its MIT
license. `Cargo.toml`, `Cargo.toml.orig`, `README.md`, and the Rust source come
from that release; `LICENSE` retains the upstream copyright and permission.
The unused upstream patch artifact `src/hb/p` is omitted; it is not Rust source
and is not referenced by the crate. The original registry archive checksum is
`9da2e5ae821f6e96664977bf974d6d6a2d6682f9ccee23e62ec1d134246845f9`.

The only source changes backport upstream
[PR #397](https://github.com/harfbuzz/harfrust/pull/397), commit
`c262b3f2d7d2f6e1a90a532e061075d3d8017c4e`. The skipping iterator's glyph-data
counter and matching callbacks use `u32` throughout, rather than overflowing
`u16` on long lines using legacy `kern`/AAT `kerx` tables. The backport adapts
context to the older source without changing its public API or dependencies.
Changed source files:

- `src/hb/ot_layout_gsubgpos.rs`
- `src/hb/ot/contextual.rs`
- `src/hb/ot/gsub/ligature.rs`

The workspace patches this release because `cosmic-text` 0.19 requires Harfrust
0.5, while the upstream fix first shipped in 0.11. Remove this directory and
the workspace override when the text stack admits a release with the fix.
Do not modify the global Cargo registry or suppress overflow checks.

Regression coverage uses the application's real text shaping and measurement
path: `text_metrics::tests::text_byte_budget_does_not_truncate_or_change_measurement`
in `mondrian-ui-widgets`, plus the text renderer's long-line coverage.
