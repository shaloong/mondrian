"""Collect read-only native acceptance receipts after the recorded release build.

This collector does not build or execute the product or tests. The caller must
provide the commands and transcript from the actual current artifact build.
"""
from __future__ import annotations

import argparse
import datetime
import ctypes
import hashlib
import json
import uuid
import os
from pathlib import Path
import platform
import subprocess
import time
import tomllib


def run(*command: str, cwd: Path | None = None, encoding: str = "utf-8") -> str:
    return subprocess.check_output(command, cwd=cwd, text=True, encoding=encoding).strip()


def source_files(*directories: str, cwd: Path) -> set[str]:
    """Use one repository path spelling for rg membership, dedup and receipts."""
    return {path.replace("\\", "/") for path in
            run("rg", "--files", "--hidden", "--no-ignore", *directories, cwd=cwd).splitlines()}


def digest(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def windows_active_power_scheme() -> dict:
    """Copy the documented native active-scheme GUID, then release its owner.

    PowerGetActiveScheme returns its Win32 status directly; the GUID allocation
    from a successful call must be released with LocalFree. No shell text or
    system code page participates in this receipt.
    """
    class Guid(ctypes.Structure):
        _fields_ = [("Data1", ctypes.c_uint32), ("Data2", ctypes.c_uint16),
                    ("Data3", ctypes.c_uint16), ("Data4", ctypes.c_ubyte * 8)]

    power = ctypes.WinDLL("PowrProf.dll", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32.dll", use_last_error=True)
    get_active = power.PowerGetActiveScheme
    get_active.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.POINTER(Guid))]
    get_active.restype = ctypes.c_uint32
    free = kernel.LocalFree
    free.argtypes = [ctypes.c_void_p]
    free.restype = ctypes.c_void_p
    active = ctypes.POINTER(Guid)()
    status = get_active(None, ctypes.byref(active))
    if status != 0:
        raise OSError(status, f"PowerGetActiveScheme returned Win32 status {status}")
    if not active:
        raise RuntimeError("PowerGetActiveScheme succeeded without a GUID allocation")
    try:
        # GUID's three integer members use Windows little-endian storage; Data4
        # is the eight-byte sequence defined by guiddef.h, with no text decoding.
        guid = str(uuid.UUID(bytes_le=ctypes.string_at(active, ctypes.sizeof(Guid))))
        return {"source": "PowrProf.dll!PowerGetActiveScheme", "guid": guid}
    finally:
        if free(ctypes.cast(active, ctypes.c_void_p)):
            status = ctypes.get_last_error()
            raise OSError(status, f"LocalFree of active power-scheme GUID failed with Win32 status {status}")


def windows_machine() -> dict:
    import winreg
    result = {"cpu": [], "total_physical_memory_bytes": None, "limitations": [],
              "os_version": platform.platform(), "windows_version": platform.win32_ver()}
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor") as processor_key:
            index = 0
            while True:
                try:
                    name = winreg.EnumKey(processor_key, index)
                except OSError as error:
                    if error.winerror == 259:  # ERROR_NO_MORE_ITEMS
                        break
                    raise
                index += 1
                with winreg.OpenKey(processor_key, name) as cpu_key:
                    cpu = {"registry_logical_processor_index": name}
                    for value, field in (("ProcessorNameString", "name"), ("VendorIdentifier", "vendor"),
                                         ("Identifier", "architecture_identifier"), ("~MHz", "registry_nominal_frequency_mhz")):
                        try:
                            cpu[field] = winreg.QueryValueEx(cpu_key, value)[0]
                        except OSError as error:
                            cpu[field] = None
                            result["limitations"].append(f"CPU {name} {value} unavailable: {error}")
                    result["cpu"].append(cpu)
        result["registry_logical_processor_count"] = len(result["cpu"])
    except OSError as error:
        result["limitations"].append(f"Read-only CPU registry unavailable: {error}")
    # Logical CPU registry entries do not establish physical-core topology.
    result["physical_core_count"] = None
    result["limitations"].append("Physical-core topology is not supplied by the read-only CPU registry receipt.")
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") as os_key:
            result["windows_registry"] = {}
            for name in ("ProductName", "DisplayVersion", "CurrentBuildNumber", "UBR"):
                try:
                    result["windows_registry"][name] = winreg.QueryValueEx(os_key, name)[0]
                except OSError as error:
                    result["windows_registry"][name] = None
                    result["limitations"].append(f"OS registry {name} unavailable: {error}")
    except OSError as error:
        result["limitations"].append(f"Read-only OS registry unavailable: {error}")
    try:
        class MemoryStatusEx(ctypes.Structure):
            _fields_ = [("dwLength", ctypes.c_uint32), ("dwMemoryLoad", ctypes.c_uint32),
                        ("ullTotalPhys", ctypes.c_uint64), ("ullAvailPhys", ctypes.c_uint64),
                        ("ullTotalPageFile", ctypes.c_uint64), ("ullAvailPageFile", ctypes.c_uint64),
                        ("ullTotalVirtual", ctypes.c_uint64), ("ullAvailVirtual", ctypes.c_uint64),
                        ("ullAvailExtendedVirtual", ctypes.c_uint64)]
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        memory_api = kernel.GlobalMemoryStatusEx
        memory_api.argtypes = [ctypes.POINTER(MemoryStatusEx)]
        memory_api.restype = ctypes.c_int
        memory = MemoryStatusEx()
        memory.dwLength = ctypes.sizeof(memory)
        if not memory_api(ctypes.byref(memory)):
            raise ctypes.WinError(ctypes.get_last_error())
        result["total_physical_memory_bytes"] = memory.ullTotalPhys
        result["available_physical_memory_bytes_at_collection"] = memory.ullAvailPhys
        frequency_api = kernel.QueryPerformanceFrequency
        frequency_api.argtypes = [ctypes.POINTER(ctypes.c_int64)]
        frequency_api.restype = ctypes.c_int
        frequency = ctypes.c_int64()
        if not frequency_api(ctypes.byref(frequency)):
            raise ctypes.WinError(ctypes.get_last_error())
        result["timer_frequency_hz"] = frequency.value
    except (OSError, AttributeError) as error:
        result["limitations"].append(f"Documented memory/timer API unavailable: {error}")
    try:
        result["power_plan"] = windows_active_power_scheme()
    except (OSError, AttributeError, RuntimeError, ValueError) as error:
        result["power_plan"] = None
        result["limitations"].append(f"Read-only active power-scheme GUID unavailable (no retry): {error}")
    return result


def machine() -> dict:
    system = platform.system()
    common = {"system": system,
              "os_version": platform.platform(), "architecture": platform.machine(),
              "logical_processors": os.cpu_count(), "processor": platform.processor()}
    clock = time.get_clock_info("perf_counter")
    common["collector_clock_info"] = {"implementation": clock.implementation,
                                      "resolution_seconds": clock.resolution,
                                      "monotonic": clock.monotonic}
    if system == "Windows":
        common.update(windows_machine())
    elif system == "Darwin":
        common["os_version"] = run("sw_vers")
        common["cpu"] = [{"name": run("sysctl", "-n", "machdep.cpu.brand_string"),
                          "logical_processors": int(run("sysctl", "-n", "hw.logicalcpu")),
                          "physical_cores": int(run("sysctl", "-n", "hw.physicalcpu"))}]
        common["total_physical_memory_bytes"] = int(run("sysctl", "-n", "hw.memsize"))
        common["power_plan"] = run("pmset", "-g", "custom")
    elif system == "Linux":
        cpu_text = Path("/proc/cpuinfo").read_text(encoding="utf-8")
        common["cpu"] = [{"proc_cpuinfo": cpu_text}]
        memory = Path("/proc/meminfo").read_text(encoding="utf-8")
        memory_kib = next(int(line.split()[1]) for line in memory.splitlines() if line.startswith("MemTotal:"))
        common["total_physical_memory_bytes"] = memory_kib * 1024
        governors = sorted(Path("/sys/devices/system/cpu").glob("cpu[0-9]*/cpufreq/scaling_governor"))
        common["power_plan"] = {str(path): path.read_text(encoding="utf-8").strip() for path in governors}
    else:
        raise RuntimeError(f"Machine collector not implemented for {system}; supply an explicit reviewed receipt")
    common.setdefault("limitations", [])
    common["cpu_identity_and_ram_available"] = any(cpu.get("name") or cpu.get("architecture_identifier") or cpu.get("proc_cpuinfo")
        for cpu in common.get("cpu", [])) and bool(common.get("total_physical_memory_bytes"))
    return common


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--worktree", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--build-transcript", type=Path, required=True)
    parser.add_argument("--build-command", action="append", required=True)
    parser.add_argument("--clap-worker", type=Path)
    parser.add_argument("--vst3-worker", type=Path)
    parser.add_argument("--clap-fixture", type=Path)
    parser.add_argument("--vst3-fixture", type=Path)
    parser.add_argument("--audio-test-executable", type=Path)
    parser.add_argument("--openfx-worker", type=Path)
    parser.add_argument("--openfx-reference", type=Path)
    parser.add_argument("--openfx-test-executable", type=Path)
    parser.add_argument("--source", type=Path, action="append", default=[])
    parser.add_argument("--external-source", type=Path, action="append", default=[])
    args = parser.parse_args()
    worktree = args.worktree.resolve(strict=True)
    git = ("git", "-c", f"safe.directory={worktree}", "-C", str(worktree))
    lock = tomllib.loads((worktree / "Cargo.lock").read_text(encoding="utf-8"))
    sdk_packages = [entry for entry in lock["package"] if entry["name"] in
                    {"clack-common", "clack-host", "clack-plugin", "clack-extensions", "clap-sys", "vst3-host", "vst3"}]
    source_dirs = ("crates/mondrian-audio/src", "crates/mondrian-core/src", "crates/mondrian-timeline/src", "vendor/vst3-host/src", "vendor/vst3-sdk", "vendor/clack", "vendor/clap-sys", "vendor/crossbeam-queue")
    sources = source_files(*source_dirs, cwd=worktree)
    audio_artifacts = (args.clap_worker, args.vst3_worker, args.clap_fixture, args.vst3_fixture, args.audio_test_executable)
    openfx_artifacts = (args.openfx_worker, args.openfx_reference, args.openfx_test_executable)
    if any(audio_artifacts) and not all(audio_artifacts):
        raise RuntimeError("Supply audio worker/fixture/test-executable paths together")
    if any(openfx_artifacts) and not all(openfx_artifacts):
        raise RuntimeError("Supply OpenFX worker/reference/test-executable paths together")
    if not any(audio_artifacts) and not any(openfx_artifacts):
        raise RuntimeError("Select at least one native artifact group")
    if args.openfx_worker:
        sources.update(source_files("crates/mondrian-effects/src", "crates/mondrian-renderer/src", cwd=worktree))
        sources.update(source_files("crates/mondrian-app/native/openfx/host",
                                    "crates/mondrian-app/native/openfx/vendor", cwd=worktree))
        sources.update({"crates/mondrian-app/build.rs", "crates/mondrian-app/src/openfx_host.rs",
                        "crates/mondrian-app/Cargo.toml", "crates/mondrian-effects/Cargo.toml",
                        "crates/mondrian-renderer/Cargo.toml", "crates/mondrian-app/src/openfx_adapter.rs",
                        "crates/mondrian-app/src/openfx_effect.rs", "crates/mondrian-app/src/openfx_host/curves.rs",
                        "crates/mondrian-app/src/openfx_host/resident.rs", "crates/mondrian-app/tests/openfx_host.rs",
                        "tests/openfx_reference/session_probe.cpp", "tests/openfx_reference/CMakeLists.txt",
                        "tests/openfx_reference/README.md"})
    for extra in args.source:
        resolved = (extra if extra.is_absolute() else worktree / extra).resolve(strict=True)
        if not resolved.is_relative_to(worktree):
            raise RuntimeError("Extra source receipts must remain inside the worktree")
        sources.add(resolved.relative_to(worktree).as_posix())
    sources.update({"Cargo.toml", "Cargo.lock", "crates/mondrian-audio/Cargo.toml", "vendor/vst3-host/Cargo.toml", "scripts/validation/regenerate-vst3-sdk.py"})
    for name in ("clap-sidechain", "vst3-sidechain"):
        for leaf in ("Cargo.toml", "Cargo.lock", "src/lib.rs"):
            sources.add(f"crates/mondrian-audio/tests/fixtures/{name}/{leaf}")
    required_harness = "crates/mondrian-audio/src/processor_isolation/tests/native_performance.rs"
    if args.clap_worker and required_harness not in sources:
        raise RuntimeError("Apply the harness patch before recording current sources")
    receipts = [{"path": path.replace("\\", "/"), "sha256": digest(worktree / path)}
                for path in sorted(sources)]
    transcript = args.build_transcript.resolve(strict=True)
    build = {"profile": "release", "commands": args.build_command,
             "transcript": {"path": str(transcript), "sha256": digest(transcript)},
             "rustc_verbose": run("rustc", "--version", "--verbose", cwd=worktree),
             "cargo_version": run("cargo", "--version", cwd=worktree),
             "git_commit": run(*git, "rev-parse", "HEAD"),
             "git_status": run(*git, "status", "--porcelain=v1"),
             "tracked_diff_normalized_utf8_sha256": hashlib.sha256(
                 run(*git, "diff", "--binary", "HEAD").encode("utf-8")).hexdigest(),
             "cargo_lock_sha256": digest(worktree / "Cargo.lock"), "sdk_locked_packages": sdk_packages,
             "environment": {key: os.environ.get(key) for key in
                 ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS",
                  "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CARGO_PROFILE_RELEASE_OPT_LEVEL",
                  "CC", "CXX", "CFLAGS", "CXXFLAGS")}}
    # Path dependencies have no Git source in Cargo.lock. Preserve explicit
    # immutable SDK/generator identity alongside hashes of every shipped file.
    canonical_sdk = worktree / "vendor/vst3-sdk"
    provenance_path = canonical_sdk / "PROVENANCE.json"
    canonical_provenance = json.loads(provenance_path.read_text(encoding="utf-8"))
    for shipped in canonical_provenance["shipped_files"]:
        shipped_path = Path(shipped["path"])
        if shipped_path.is_absolute() or ".." in shipped_path.parts:
            raise RuntimeError("Canonical VST3 provenance contains an invalid relative path")
        if digest(canonical_sdk / shipped_path) != shipped["sha256"]:
            raise RuntimeError(f"Canonical VST3 source changed: {shipped_path}")
    build["vst3_sdk_provenance"] = {"path":"vendor/vst3-sdk/PROVENANCE.json",
                                    "sha256":digest(provenance_path),
                                    "manifest":canonical_provenance}
    # Path Clack packages likewise require explicit immutable source receipts.
    clack_sdk = worktree / "vendor/clack"
    clack_provenance_path = clack_sdk / "PROVENANCE.json"
    clack_provenance = json.loads(clack_provenance_path.read_text(encoding="utf-8"))
    for shipped in clack_provenance["shipped_files"]:
        shipped_path = Path(shipped["path"])
        if shipped_path.is_absolute() or ".." in shipped_path.parts:
            raise RuntimeError("Canonical Clack provenance contains an invalid relative path")
        if digest(clack_sdk / shipped_path) != shipped["sha256"]:
            raise RuntimeError(f"Canonical Clack source changed: {shipped_path}")
    build["clack_sdk_provenance"] = {"path": "vendor/clack/PROVENANCE.json",
                                     "sha256": digest(clack_provenance_path),
                                     "manifest": clack_provenance}
    # The sole official header-generated CLAP path package has no Git lock ID.
    clap_sdk = worktree / "vendor/clap-sys"
    clap_provenance_path = clap_sdk / "PROVENANCE.json"
    clap_provenance = json.loads(clap_provenance_path.read_text(encoding="utf-8"))
    for shipped in clap_provenance["shipped_files"]:
        shipped_path = Path(shipped["path"])
        if shipped_path.is_absolute() or ".." in shipped_path.parts:
            raise RuntimeError("Canonical CLAP provenance contains an invalid relative path")
        if digest(clap_sdk / shipped_path) != shipped["sha256"]:
            raise RuntimeError(f"Canonical CLAP source changed: {shipped_path}")
    build["clap_sdk_provenance"] = {"path": "vendor/clap-sys/PROVENANCE.json",
                                    "sha256": digest(clap_provenance_path),
                                    "manifest": clap_provenance}
    artifacts = [{"role": role, "path": str(path.resolve(strict=True)),
                  "sha256": digest(path.resolve(strict=True)), "bytes": path.stat().st_size}
                 for role, path in (("clap_worker", args.clap_worker), ("vst3_worker", args.vst3_worker),
                                    ("clap_fixture", args.clap_fixture), ("vst3_fixture", args.vst3_fixture),
                                    ("audio_test_executable", args.audio_test_executable)) if path]
    if args.openfx_worker:
        for role, path in (("openfx_worker", args.openfx_worker), ("openfx_reference", args.openfx_reference),
                           ("openfx_test_executable", args.openfx_test_executable)):
            resolved = path.resolve(strict=True)
            artifacts.append({"role": role, "path": str(resolved), "sha256": digest(resolved), "bytes": resolved.stat().st_size})
    report = {"schema_version": 1, "recorded_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "machine": machine(), "build": build, "artifacts": artifacts, "source_receipts": receipts,
              "external_source_receipts": [{"path": str(path.resolve(strict=True)), "sha256": digest(path.resolve(strict=True))}
                                           for path in args.external_source],
              "provenance": "Build commands/transcript are supplied from the actual completed build; collector does not rebuild."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8", newline="\n") as destination:
        json.dump(report, destination, ensure_ascii=False, indent=2)
        destination.write("\n")
    print(f"Native performance context: {args.output.resolve()} SHA256={digest(args.output)}")


if __name__ == "__main__":
    main()
