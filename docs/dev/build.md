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

## macOS FFmpeg Runtime

macOS builds use the shared FFmpeg CLI and libraries produced by
`scripts/build-macos-ffmpeg.sh`. CI and release packaging use this same builder;
an unmodified package-manager FFmpeg is insufficient for the independent
ProRes 422 CPU/VideoToolbox pixel agreement gate.

The builder verifies the FFmpeg 8.1.2 archive SHA-256 and applies one small
decoder patch. Conventional ProRes reconstruction retains twelve-bit samples
and clips them to 0..4095, as permitted by
[RDD 36:2015 §7.5.1](https://pub.smpte.org/doc/rdd36/20151026-pub/rdd36-2015.pdf).
This pixel-code domain does **not** change limited-range color metadata or
interpretation. ProRes 4444/XQ already use twelve-bit reconstruction; their
conventional color-plane clipping uses the same code domain. ProRes RAW Bayer
clipping and alpha unpacking remain unchanged. The native adapter independently
validates the actual sixteen-bit CVPixelBuffer surface and observed hardware
decode; the probe's software reconstruction format does not define that surface.

The shared runtime includes `libopenjpeg` for the SMPTE DCP picture encoder.
That production recipe requires XYZ twelve-bit JPEG 2000 with `cinema2k` and
`2k_24`; FFmpeg’s built-in JPEG 2000 encoder is not a substitute. Supply the
OpenJPEG development package alongside the codec dependencies listed in CI.

Provide development dependencies through `PKG_CONFIG_PATH`, `CFLAGS` and
`LDFLAGS`. The builder installs no packages and requires a macOS host, absolute
work/install paths, and one or two build workers. For example, after installing
the dependencies listed in the macOS CI step:

```bash
ffmpeg_work=$(mktemp -d /tmp/mondrian-ffmpeg.XXXXXX)
ffmpeg_prefix="$ffmpeg_work/runtime"
bash scripts/build-macos-ffmpeg.sh "$ffmpeg_work" "$ffmpeg_prefix" 2
export FFMPEG_DIR="$ffmpeg_prefix"
export PATH="$ffmpeg_prefix/bin:$PATH"
export PKG_CONFIG_PATH="$ffmpeg_prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export RUSTFLAGS="-Lnative=$ffmpeg_prefix/lib${RUSTFLAGS:+ $RUSTFLAGS}"
cargo build --locked -p mondrian-app -j 2
```

Keg-only dependencies need their pkg-config directories and link-time runtime
search paths, as configured in CI. Do not substitute libraries through DYLD
variables. The installed `share/mondrian-ffmpeg-source` directory contains the
verified source archive, patch, builder, configuration, license files and hash
manifest; release packaging retains it as `FFMPEG_SOURCE`. Existing recursive
private-dylib closure and `--verify-runtime` gates remain required after bundling.
This build recipe alone is not a physical decoder or HDR qualification result.

The Darwin and Linux entrypoints share `scripts/ffmpeg/build-unix-runtime.sh`:
the same FFmpeg version, ProRes twelve-bit reconstruction and RGBA64 signed-chroma
patches, codec dependencies and source package. Platform branches select their
compiler defaults, native frameworks and library search policy.

### Private app packaging

The FFmpeg builder reserves Mach-O header space for dependency relocation.
Use the release workflow's `scripts/package-macos-app.sh` to stage the app,
sibling FFmpeg tools, bundle metadata and their union dylib closure. It requires
`dylibbundler` on `PATH`, a new destination and explicit dependency search
directories when nested libraries cannot resolve their own `@rpath` entries:

```bash
bash scripts/package-macos-app.sh target/debug/mondrian \
  /tmp/mondrian-package/Mondrian.app 0.2.0 \
  "$(brew --prefix)/lib" "$(brew --prefix openssl@3)/lib" "$(brew --prefix zlib)/lib"
```

Unresolved libraries abort without an interactive prompt. Every bundled image
must have a valid ad-hoc signature and every non-system dependency must resolve
inside `Contents/Frameworks`. The final `--verify-runtime` runs with a clean
environment and packaged sibling tools. A local development bundle passing
these checks does not establish release provenance, notarization, codec hardware
qualification or commercial performance. Release packaging separately retains
licenses, source and immutable qualification provenance.

## Linux FFmpeg Runtime

On Ubuntu 24, install the development dependencies before invoking the builder:

```bash
sudo apt-get install --no-install-recommends \
  nasm libx264-dev libx265-dev libaom-dev libdav1d-dev \
  libopenjp2-7-dev libharfbuzz-dev libass-dev \
  libfribidi-dev libgraphite2-dev libunibreak-dev
```

Use an absolute development prefix for the matching CLI and shared libraries.
The Linux entrypoint defaults to the official signed-tag GitHub source mirror;
`MONDRIAN_FFMPEG_SOURCE_KIND=release` selects the ffmpeg.org archive instead.
Both sources are pinned and use the same precision patches.

```bash
ffmpeg_work=$(mktemp -d /tmp/mondrian-ffmpeg.XXXXXX)
ffmpeg_prefix="$ffmpeg_work/runtime"
bash scripts/build-linux-ffmpeg.sh "$ffmpeg_work" "$ffmpeg_prefix" 2
export FFMPEG_DIR="$ffmpeg_prefix"
export PATH="$ffmpeg_prefix/bin:$PATH"
export PKG_CONFIG_PATH="$ffmpeg_prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export RUSTFLAGS="-Lnative=$ffmpeg_prefix/lib${RUSTFLAGS:+ $RUSTFLAGS}"
export LD_LIBRARY_PATH="$ffmpeg_prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
cargo build --locked -p mondrian-app -j 2
```

Keep the SDK library directory first in Rust's native search paths: transitive
dependencies such as ALSA can otherwise introduce a system directory before it,
linking older FFmpeg libraries against newer generated headers. `PKG_CONFIG_PATH`
alone selects metadata, not the final linker precedence. If using
`CARGO_ENCODED_RUSTFLAGS`, prepend `-Lnative=<prefix>/lib` there instead of
`RUSTFLAGS`; Cargo gives the encoded variable precedence. The runtime initialization
gate checks all six linked library versions against the generated bindings before
using FFmpeg struct layouts. A mismatched ABI or older runtime fails explicitly.

The installed `share/mondrian-ffmpeg-source` preserves the source archive,
configuration, licenses and the builder/patch directory layout for rebuilding.
Development builds still require separate actual codec, host, device and packaged
runtime qualification.

## Profiles

Dev profile uses light optimization for media/render debugging. Release enables LTO thin, high optimization, and symbol stripping.

## Docs-Only Changes

Docs-only branches do not need functional cargo tests unless they modify code examples that are compiled by doc tests.
