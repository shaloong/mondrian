# Static DXC distribution provenance

## Selection and licenses

Windows MSVC x86_64 enables wgpu's `static-dxc` feature. The normal production
`GpuContext` retains `Dx12Compiler::Auto`; the compiler is linked into the
executable. It does not require a developer SDK DLL or a hidden SDK `PATH`.
This selection covers this target only, and does not establish GPU correctness,
performance, or support on another architecture or backend.

The Rust wrapper is `mach-dxcompiler-rs` **0.1.6+2026.09.16-48d5a66.1**, MIT,
registry checksum `699d5b85d279d172f6ea742c6c47342fdd7c388872cea2e137e2c6721ef2e6d6`.
Its published source records commit
`77b9a2f68c146d1cf45e52ed7e7ac2457be875d5`.
See [wrapper source](https://github.com/DouglasDwyer/mach-dxcompiler-rs/tree/77b9a2f68c146d1cf45e52ed7e7ac2457be875d5).

The linked native archive belongs to the immutable
[Mach release 2026.09.16+48d5a66.1](https://github.com/DouglasDwyer/mach-dxcompiler/releases/tag/2026.09.16%2B48d5a66.1),
native source commit `48d5a66964a1514bb0ff2b553ce77abed45c2d09`.
The Mach build layer offers MIT or Apache-2.0; this distribution retains the
MIT alternative and its root license delegation. This does **not** relicense
the embedded DXC/LLVM source as MIT. The pinned
[build.zig](https://github.com/DouglasDwyer/mach-dxcompiler/blob/48d5a66964a1514bb0ff2b553ce77abed45c2d09/build.zig)
selects `hexops/DirectXShaderCompiler` commit
`4190bb0c90d374c6b4d0b0f2c7b45b604eda24b6`. Its primary license is the
University of Illinois/NCSA Open Source License, with the additional notices
in its complete `LICENSE.TXT` and `ThirdPartyNotices.txt`. Preserve both files
with the binary distribution. This old source revision must not be described
as the modern Apache-2.0 LLVM exception license.
See [pinned DXC license](https://github.com/hexops/DirectXShaderCompiler/blob/4190bb0c90d374c6b4d0b0f2c7b45b604eda24b6/LICENSE.TXT)
and [third-party notices](https://github.com/hexops/DirectXShaderCompiler/blob/4190bb0c90d374c6b4d0b0f2c7b45b604eda24b6/ThirdPartyNotices.txt).

`lhash` **1.1.0** is a build dependency used by the wrapper's SHA-256 archive
verification. It is not a runtime processor or shader. Its license is BSL-1.0,
registry checksum `744a4c881f502e98c2241d2e5f50040ac73b30194d64452bb6260393b53f0dc9`,
published source commit `403834764cadd9c47928c5e70219fec1e1b10857`.
The registry's package include list omits `LICENSE`; the retained notice is the
exact [upstream license at that commit](https://github.com/DoumanAsh/lhash/blob/403834764cadd9c47928c5e70219fec1e1b10857/LICENSE).
BSL-1.0 permits distribution, and exempts solely machine-executable object code
from its notice condition. We retain its full notice anyway. `deny.toml`
already permits MIT and BSL-1.0; the Rust metadata audit alone does not audit the
native compiler archive's separate license.

The archive build uses `-Dfrom_source -Doptimize=ReleaseFast`; its SPIR-V build
option defaults to false. Mach's MSVC WRL substitute is marked public domain
in its [pinned header](https://github.com/DouglasDwyer/mach-dxcompiler/blob/48d5a66964a1514bb0ff2b553ce77abed45c2d09/msvc/wrl/client.h).
The complete DXC notice includes build/test source notices, including Autoconf
GPL text with its generated-output exception. Keeping that complete notice
does not establish that Autoconf or test executables are linked into Mondrian.
The source selection and archive build recipe are recorded; a reproducible
binary build or a complete per-object linker census is a separate evidence
claim.

## Fixed native artifact

For the default dynamic MSVC CRT, the selected artifact is
`x86_64-windows-msvc_ReleaseFast_Dynamic_lib.tar.gz`, release asset **568856523**,
size **26,115,841 bytes**, expected SHA-256
`2db15ab14e6eee9b9931d75d1690b4f7d81cad446fecccfe4e36e218dee25cce`.
The release API's asset digest and the wrapper's `targets.rs` agree.
The `Dynamic` suffix describes MSVC CRT linkage; `machdxcompiler` itself is
linked as a static library. A Rust `+crt-static` build selects the different
`x86_64-windows-msvc_ReleaseFast_lib.tar.gz` artifact, size 26,261,113 bytes,
SHA-256 `60bdfab56d50679ef071489432fd716e5c2c73c7ec08c33a29d0c910ddca1b04`.
These receipts must not be interchanged.

The wrapper first checks the immutable-release flag, downloads the selected
archive, verifies SHA-256, extracts it and links `machdxcompiler`. Its current
curl command uses `--location` and has no connect/whole-operation timeout,
retry policy or HTTP `--fail` flag. A slow incomplete download therefore
remains a build operation, not a native shader or numeric test failure.
Only completed-file verification and the actual Cargo/build artifact receipt
prove which native archive reached an executable.

## Notices shipped with the application

The Windows, Linux and macOS release staging recipes already copy the entire
`LICENSES` directory. Retain the following verbatim license texts there:

| File | Source scope |
| --- | --- |
| `MACH-DXCOMPILER-RS-MIT.txt` | Published Rust wrapper MIT text |
| `MACH-DXCOMPILER-LICENSE.txt` | Native Mach root license delegation |
| `MACH-DXCOMPILER-MIT.txt` | Selected native Mach MIT alternative |
| `DXC-LLVM-LICENSE.txt` | Complete pinned DXC/LLVM license and bundled notices |
| `DXC-THIRD-PARTY-NOTICES.txt` | Complete pinned DXC third-party notices |
| `LHASH-BSL-1.0.txt` | Exact pinned upstream build-dependency license |

License/source review does not qualify shader compilation, hardware output,
first-use costs, steady execution or the NLE's frame budget. Those results
belong to the corresponding runtime/build receipts.
