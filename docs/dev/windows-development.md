# Windows development dependencies

Use PowerShell 7 and the repository-pinned Rust toolchain. Install Visual Studio
2022 with Desktop development with C++, an x64 MSVC toolset, C++ CMake tools (CMake and Ninja), and Windows SDK.
LLVM supplies `libclang.dll` for bindgen; MSVC remains the native C/C++ compiler.
The Vulkan SDK supplies validation/development tools; its installation does not
qualify the display, GPU driver, or HDR output.

Install the vcpkg tag declared by `VCPKG_REF` in `.github/workflows/ci.yml`
(currently `2026.07.29`) in a local directory. Bootstrap with `-disableMetrics`.
Do not replace the Visual Studio-managed vcpkg copy. From the checkout, use:

```powershell
$env:VCPKG_ROOT = Join-Path $env:SystemDrive 'vcpkg' # Or your installation directory.
$env:VCPKG_MAX_CONCURRENCY = '2'
& "$env:VCPKG_ROOT/vcpkg.exe" install `
  'ffmpeg[zlib,ffmpeg,ffprobe,gpl,x264,x265,aom,nvcodec]:x64-windows' `
  pkgconf:x64-windows --recurse --overlay-ports=vcpkg-overlay
```

This is the existing CI product profile, including the GPL codec components;
it is not an LGPL-only distribution profile. The `nvcodec` feature supplies
FFmpeg's NVIDIA codec headers and enables runtime loading of the installed
NVIDIA driver; it does not require the CUDA SDK and does not change Mondrian's
primary Windows decode/render path from D3D12. Keep vcpkg's installed package
copyright notices. LLVM uses Apache-2.0 with LLVM Exceptions. This setup does
not redistribute system fonts or change the title font policy.

For each new terminal, activate the environment in the current process:

```powershell
. ./scripts/enter-windows-development.ps1
cargo test -p mondrian-media --lib probe_mapping_ --locked -j 2
```

The script defaults to the system drive's `vcpkg` directory and the standard
LLVM installation, or accepts `-VcpkgRoot` and `-LibClangDirectory`. It imports
the actual MSVC x64 environment via `vswhere`, selects that compiler for Ninja
(`CC`, `CXX`, and `ASM`), verifies headers/import libraries,
and binds pkg-config, FFmpeg/FFprobe and DLL lookup to the same x64-windows
installation. It checks FFmpeg 8's libavcodec major before reporting readiness.
It changes only the current process environment, not the system/user PATH or
an unrelated FFmpeg installation. Git Bash precedes WSL for native build tools.

Build outputs default to local application data under `Mondrian/target-windows`,
including when sources are on an SMB checkout. Existing `CARGO_TARGET_DIR` or
`-TargetDirectory` overrides that default. Do not share this directory with
simultaneous builds. Logs, package caches, native tool installations, machine
paths and build outputs must not be committed.
