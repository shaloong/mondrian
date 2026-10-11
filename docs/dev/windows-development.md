# Windows development dependencies

Use PowerShell 7 and the repository-pinned Rust toolchain. Install Visual Studio
2022 version 17.11 or later with Desktop development with C++, an x64 MSVC
toolset, C++ CMake tools (CMake and Ninja), Windows SDK, and **C++ ATL for latest
v143 build tools (x86 & x64)**. ATL is an optional Visual Studio component;
installing the Desktop C++ workload alone does not establish its availability.
Its installer ID is `Microsoft.VisualStudio.Component.VC.ATL`. If a specific
older toolset is deliberately selected, install its matching ATL component;
for v14.44 this is `Microsoft.VisualStudio.Component.VC.14.44.17.14.ATL`.
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

## Windows x64 shader compiler prerequisite

The Windows MSVC x64 renderer enables wgpu's `static-dxc` feature. The pinned
native compiler archive contains explicit `/DEFAULTLIB:atls.lib` directives;
the actual active MSVC toolset must supply that library. Visual Studio Installer
**Modify → Individual components** installs ATL into that toolset. Reactivate the
environment afterwards. A C++ redistributable runtime does not supply this
development library, and an unrelated toolset's copy is not a substitute.

The existing activation script checks
`VCToolsInstallDir/atlmfc/lib/x64/atls.lib`, the exact activated x64 `cl.exe`, and
the ATL directory in `LIB` before reporting readiness. It rejects Visual Studio
older than 17.11 and an activated toolset older than v14.41. Windows CI and packaging
use the same entry point in its MSVC-only mode:

```powershell
. ./scripts/enter-windows-development.ps1 -MsvcOnly
```

This mode activates and validates the compiler environment without requiring
FFmpeg or LLVM to be installed first. It imports only the named MSVC/SDK variables,
preserving the caller's existing vcpkg, FFmpeg and LLVM selections. It changes only the current process;
workflow steps explicitly persist its finite set of required variables through
`GITHUB_ENV`. Missing ATL is a build prerequisite failure. It is not a renderer
pixel, performance, or hardware qualification result. The dependency's own
build script separately checks its minimum MSVC compiler version.

Sources: [Visual Studio 2022 component IDs](https://learn.microsoft.com/en-us/visualstudio/install/workload-component-id-vs-build-tools?view=vs-2022#desktop-development-with-c),
[wgpu compiler selection](https://docs.rs/wgpu/30.0.0/wgpu/enum.Dx12Compiler.html),
and [pinned native compiler provenance](static-dxc-provenance.md).
