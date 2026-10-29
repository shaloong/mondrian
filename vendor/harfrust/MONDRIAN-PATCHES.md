# HarfRust source provenance

This is the MIT-licensed Cargo registry package `harfrust` 0.5.2, originating from commit `efdae3142ab76a2f1524d72cff9e3dfdc5afd7ca` of https://github.com/harfbuzz/harfrust. The package omitted its license file; `LICENSE` is copied from the exact upstream commit. File hashes are in `PROVENANCE.json`.

The sole production change backports upstream [PR 397](https://github.com/harfbuzz/harfrust/pull/397), merge commit `6e85d3f8a8f54614b6cf4f267f4c8063181070ed`, which is included in upstream 0.11.0. The matched sequence index is widened to `u32` rather than overflowing after 65,535 glyphs in classic kern/AAT kerx runs. Matcher signatures and values are widened losslessly across the same three internal source files. The 0.5.2 contextual helper remains otherwise unchanged.

No input text is shortened, Advanced shaping stays enabled, and overflow checking stays enabled. Public Mondrian TextRenderer and actual numeric Host large Paste regressions exercise the integration. Keeping this compatible source version avoids adapting cosmic-text to an unrelated major HarfRust API migration. Replace the vendor when cosmic-text admits an upstream version containing this fix.
