# Setup

## Rust

Use Rust 1.92 or newer, matching workspace `rust-version`.

```bash
rustup update
rustup default stable
```

## Native Dependencies

Mondrian depends on:

- FFmpeg libraries via `ffmpeg-next`
- wgpu-supported GPU drivers
- system windowing APIs through winit/wgpu
- cpal audio backend
- OCIO runtime support through `ocio-rs`

On Windows, install a toolchain capable of building native Rust crates and use
`vcpkg install "ffmpeg[zlib]:x64-windows" --recurse`. The `zlib` feature is
required by FFmpeg's PNG and EXR decoders; a default `ffmpeg:x64-windows`
install is not a supported Mondrian runtime. Ensure those development libraries
are discoverable by the build.

## Optional Tools

- `cargo-nextest` for faster test runs
- GPU debugging tools for the active backend
- OCIO config files for color-management testing

## Run

```bash
cargo run -p mondrian-app
```
