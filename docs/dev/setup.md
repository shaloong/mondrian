# Setup

## Rust

Use Rust 1.97.1 or newer, matching workspace `rust-version`. The workspace uses
Rust edition 2024 and Cargo resolver 3; crate manifests inherit both policies
from the workspace rather than selecting editions independently.

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
`vcpkg install "ffmpeg[zlib,ffmpeg,ffprobe,gpl,x264,x265,aom]:x64-windows"
--recurse --overlay-ports=vcpkg-overlay`. This is the product profile: `zlib`
closes PNG/OpenEXR decode, `ffmpeg`/`ffprobe` provide supervised CLI adapters,
and the explicit encoder features match Export's current software backends.
Release verification also requires the resulting CLI runtime to expose DNxHR
LB/SQ/HQ/HQX/444 with `yuv422p10le`/`gbrp10le`, libx264's
`avcintra-class` with `yuv422p10le`, and the `rawvideo`, `v210`, and `r210`
encoders. Encoder-name presence is not enough: the runtime gate inspects the
encoder help contracts, while Export tests complete real professional
encode/re-probe rows. A build that lacks x264 10-bit 4:2:2 must fail release
verification rather than silently dropping AVC-Intra support.
The repository's `vcpkg-overlay` port builds `x265` with `HIGH_BIT_DEPTH=ON`;
without it the stock port ships an 8-bit-only encoder and HEVC Main10 exports
silently downgrade to 8-bit. A default `ffmpeg:x64-windows` install is not a
supported Mondrian runtime. Ensure those development libraries and tools are
discoverable by the build. The vcpkg
install also provisions `pkgconf`; the bundled OCIO build invokes a
`pkg-config` executable to resolve its own install metadata, so either set
`PKG_CONFIG` to `tools\pkgconf\pkgconf.exe` or place a `pkg-config.exe` copy
on `PATH`.

On Debian/Ubuntu, install the same native surface used by CI:

```bash
sudo apt-get install -y --no-install-recommends \
  ffmpeg libavcodec-dev libavformat-dev libavfilter-dev \
  libavdevice-dev libswscale-dev libswresample-dev \
  libasound2-dev libdbus-1-dev libxcb1-dev pkg-config
```

`libdbus-1-dev` and `libxcb1-dev` are required because screen capture for
the eyedropper (`xcap`) uses the XDG portal D-Bus API and XCB on Linux; they
are functional dependencies, not optional conveniences.

On macOS, use `brew install ffmpeg pkg-config`.

## Optional Tools

- `cargo-nextest` for faster test runs
- GPU debugging tools for the active backend
- OCIO config files for color-management testing

## Run

```bash
cargo run -p mondrian-app
```
