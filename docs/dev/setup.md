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

On Windows, install a toolchain capable of building native Rust crates and ensure FFmpeg development libraries are discoverable by the build.

## Optional Tools

- `cargo-nextest` for faster test runs
- GPU debugging tools for the active backend
- OCIO config files for color-management testing

## Run

```bash
cargo run -p mondrian-app
```
