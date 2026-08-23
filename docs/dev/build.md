# Build

## Common Commands

```bash
cargo build
cargo build --release
cargo run -p mondrian-app
```

## CI Gate

Before merging code changes:

```bash
cargo fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Run relevant tests for changed crates. For broad UI/runtime changes, include at least affected UI crates and app UI tests.

## Profiles

Dev profile uses light optimization for media/render debugging. Release enables LTO thin, high optimization, and symbol stripping.

## Docs-Only Changes

Docs-only branches do not need functional cargo tests unless they modify code examples that are compiled by doc tests.
