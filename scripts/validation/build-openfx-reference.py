#!/usr/bin/env python3
"""Build the official BSD-3-Clause Basic example for native host tests only."""

import argparse
import json
from pathlib import Path
import platform
import shutil
import subprocess
import urllib.request


SDK_REVISION = "e40728885390ec16276d11e00025de9b4282060c"
REFERENCE_FILES = (
    "Examples/Basic/basic.cpp",
    "Examples/include/ofxUtilities.H",
    "LICENSE.md",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument(
        "--source-root",
        type=Path,
        help="Use an existing SDK source directory; its revision is caller supplied.",
    )
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    receipt = output / "reference.json"
    receipt.unlink(missing_ok=True)
    if args.source_root is None:
        source = output / "source"
        for relative in REFERENCE_FILES:
            url = (
                "https://raw.githubusercontent.com/AcademySoftwareFoundation/"
                f"openfx/{SDK_REVISION}/{relative}"
            )
            with urllib.request.urlopen(url, timeout=30) as response:
                data = response.read(1024 * 1024 + 1)
            if not data or len(data) > 1024 * 1024:
                raise RuntimeError(f"Unexpected SDK reference file size: {relative}")
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        origin = {"revision": SDK_REVISION, "kind": "official pinned source"}
    else:
        source = args.source_root.resolve(strict=True)
        origin = {"kind": "caller-provided source", "path": str(source)}
    for relative in REFERENCE_FILES:
        if not (source / relative).is_file():
            raise FileNotFoundError(source / relative)
    # Keep the upstream license with the lab artifact. No plugin source or binary
    # is added to the product, and caller-provided source is never modified.
    shutil.copy2(source / "LICENSE.md", output / "LICENSE.md")
    include = (
        Path(__file__).resolve().parents[2]
        / "crates/mondrian-app/native/openfx/vendor/include"
    )
    cpp = source / "Examples/Basic/basic.cpp"
    system = platform.system()
    machine = platform.machine().lower()
    if system == "Darwin":
        architecture = "MacOS"
    else:
        architecture = {
            ("Windows", "amd64"): "Win64",
            ("Windows", "x86_64"): "Win64",
            ("Windows", "x86"): "Win32",
            ("Linux", "x86_64"): "Linux-x86-64",
            ("Linux", "i686"): "Linux-x86",
            ("Linux", "i386"): "Linux-x86",
        }.get((system, machine))
        if architecture is None:
            raise RuntimeError(f"Unsupported OFX bundle architecture: {system}/{machine}")
    bundle = output / "Basic.ofx.bundle"
    native_directory = bundle / "Contents" / architecture
    native_directory.mkdir(parents=True, exist_ok=True)
    plugin = native_directory / "Basic.ofx"
    if system == "Windows":
        compiler = shutil.which("cl")
        command = [
            compiler, "/nologo", "/std:c++17", "/EHsc", "/utf-8", "/O2",
            "/DNDEBUG", "/MD", "/LD", f"/I{include}", str(cpp),
            "/link", f"/OUT:{plugin}", f"/IMPLIB:{output / 'Basic.lib'}",
        ]
    elif system in ("Linux", "Darwin"):
        compiler = shutil.which("c++")
        command = [
            compiler, "-std=c++17", "-O3", "-DNDEBUG", "-fPIC",
            "-bundle" if system == "Darwin" else "-shared",
            f"-I{include}", str(cpp), "-o", str(plugin),
        ]
    else:
        raise RuntimeError(f"Unsupported native reference platform: {system}")
    if compiler is None:
        raise RuntimeError("Activate the native C++ compiler before building the reference")
    subprocess.run(command, cwd=output, check=True, timeout=120)
    if not plugin.is_file() or plugin.stat().st_size == 0:
        raise RuntimeError("Native reference compiler did not produce a plugin")
    result = {
        "binary": str(plugin),
        "bundle": str(bundle),
        "source": origin,
        "headers": "repository-vendored OpenFX headers; whitespace normalized",
        "platform": system,
        "compiler": compiler,
        "license": "BSD-3-Clause; LICENSE.md retained beside the lab binary",
        "scope": "Reference build only; host compatibility requires the native tests",
    }
    receipt.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
