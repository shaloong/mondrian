# Canonical CLAP ABI

The sole `clap-sys` package is generated from the full immutable official CLAP
1.2.10 include tree, commit `195b42a004144fab0b3cf95e9c067187d15365b7`.
`src/raw.rs` is actual bindgen 0.72.1 output. Header modules reexport this same
declaration set; they contain no alternate FFI structs or hand-sized ABI aliases.

The header-local `CLAP_VERSION`, `CLAP_COLOR_TRANSPARENT`, and compatibility
predicate have no exported C symbol. Rust value projections are checked against
the independently compiled official C++ header oracle. Existing maintainer
raw table/POD Send/Sync markers retain their caller-enforced lifetime and thread
contracts; they do not grant permission to call plugin code on arbitrary threads.

Generated `clap_xwnd` uses `core::ffi::c_ulong`, including LP64 platforms. Native
ABI qualification is required separately on Windows, macOS and Linux. An actual
Windows generation is not a native macOS/Linux execution receipt.

Regeneration uses the separate `generator` package and its exact lockfile:
`cargo run --manifest-path vendor/clap-sys/generator/Cargo.toml -- vendor/clap-sys/official OUTPUT TARGET`.
Record libclang/compiler versions, command, source checks, output hash and log.
No formatter or manual FFI edits are part of this generation. If a repository
formatter changes shipped bytes, record that formatting delta and new shipped
hash separately from the preserved generator output hash.

Official header/license and generator input receipts are in this package.
The offline archive cache used for generation is not shipped. Runtime has no
dependencies. Root and Clack use this one path package; normal Cargo metadata
updates root and fixture lockfiles. Do not hand-edit locks or retain Git aliases.

This upgrades ABI inputs. It does not implement every optional host extension,
vendor interoperability, editor workflow, recovery or realtime performance gate.
