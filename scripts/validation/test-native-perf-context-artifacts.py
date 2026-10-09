"""Artifact-group collector oracle, with no native/build/performance execution.

All source and binary-byte inputs below are controlled receipt-format fixtures.
Read-only tool/machine queries are mocked; no Cargo subprocess is launched.
"""
from __future__ import annotations

import argparse
import contextlib
import ctypes
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

COLLECTOR = Path(__file__).with_name("collect-native-perf-context.py")
SCRATCH = Path(__file__).resolve().parents[2] / ".scratch" / "native-perf-collector-tests"

SOURCES = ["Cargo.toml", "Cargo.lock", "crates/mondrian-audio/Cargo.toml", "vendor/vst3-host/Cargo.toml",
    "scripts/validation/regenerate-vst3-sdk.py",
    "crates/mondrian-audio/src/processor_isolation/tests/native_performance.rs",
    "crates/mondrian-app/build.rs", "crates/mondrian-app/src/openfx_host.rs", "crates/mondrian-app/Cargo.toml",
    "crates/mondrian-effects/Cargo.toml", "crates/mondrian-renderer/Cargo.toml",
    "crates/mondrian-app/src/openfx_adapter.rs", "crates/mondrian-app/src/openfx_effect.rs",
    "crates/mondrian-app/src/openfx_host/curves.rs", "crates/mondrian-app/src/openfx_host/resident.rs",
    "crates/mondrian-app/tests/openfx_host.rs", "tests/openfx_reference/session_probe.cpp",
    "tests/openfx_reference/CMakeLists.txt", "tests/openfx_reference/README.md"]
for fixture in ("clap-sidechain", "vst3-sidechain"):
    SOURCES.extend(f"crates/mondrian-audio/tests/fixtures/{fixture}/{leaf}" for leaf in ("Cargo.toml", "Cargo.lock", "src/lib.rs"))


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class ArtifactGroupTests(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="receipt-oracle-", dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for source in SOURCES:
            path = self.root / source
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("controlled collector source-byte fixture only\n", encoding="utf-8")
        (self.root / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "clack-host"\nversion = "0.1.0"\n', encoding="utf-8")
        sdk = self.root / "vendor/vst3-sdk"
        sdk.mkdir(parents=True, exist_ok=True)
        (sdk / "receipt-oracle.txt").write_text("controlled collector digest fixture; no SDK implementation\n", encoding="utf-8")
        (sdk / "PROVENANCE.json").write_text(json.dumps({"scope": "collector-format oracle only", "shipped_files": [
            {"path": "receipt-oracle.txt", "sha256": digest(sdk / "receipt-oracle.txt")}
        ]}), encoding="utf-8")
        self.transcript = self.root / "build-fixture.txt"
        self.transcript.write_text("controlled transcript fixture; no compiler was run\n", encoding="utf-8")
        self.artifacts = {}
        for role in ("clap_worker", "vst3_worker", "clap_fixture", "vst3_fixture", "audio_test_executable",
            "openfx_worker", "openfx_reference", "openfx_test_executable"):
            path = self.root / (role + ".byte-fixture")
            path.write_bytes(("not executable; collector artifact oracle: " + role).encode("utf-8"))
            self.artifacts[role] = path
        spec = importlib.util.spec_from_file_location("collector_under_test", COLLECTOR)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    def collect(self, flags):
        output = self.root / "context.json"
        argv = [str(COLLECTOR), "--worktree", str(self.root), "--output", str(output),
            "--build-transcript", str(self.transcript), "--build-command", "controlled collector fixture only"]
        for role in flags:
            argv.extend(["--" + role.replace("_", "-"), str(self.artifacts[role])])

        def tool_query(*command, **_):
            if command[0] == "rg":
                return getattr(self, "rg_source_paths", "crates/mondrian-audio/src/processor_isolation/tests/native_performance.rs")
            if command[0] == "rustc":
                return "release: collector-only mocked query"
            if command[0] == "cargo":
                return "collector-only mocked Cargo version query; no process executed"
            if command[0] == "git":
                return "0" * 40 if command[-2:] == ("rev-parse", "HEAD") else ""
            raise AssertionError(f"unexpected real tool query: {command}")

        machine = {"os_version": "collector-only fixture", "cpu": [{"name": "controlled receipt fixture"}],
            "total_physical_memory_bytes": 1, "cpu_identity_and_ram_available": True, "limitations": []}
        with mock.patch.object(sys, "argv", argv), mock.patch.object(self.module, "run", side_effect=tool_query), \
            mock.patch.object(self.module, "machine", return_value=machine), contextlib.redirect_stdout(io.StringIO()), \
            contextlib.redirect_stderr(io.StringIO()):
            self.module.main()
        return json.loads(output.read_text(encoding="utf-8"))

    def test_audio_receipt_has_exact_test_executable_identity(self):
        context = self.collect(["clap_worker", "vst3_worker", "clap_fixture", "vst3_fixture", "audio_test_executable"])
        selected = [artifact for artifact in context["artifacts"] if artifact["role"] == "audio_test_executable"]
        self.assertEqual(len(selected), 1)
        artifact = selected[0]
        self.assertEqual(Path(artifact["path"]).resolve(), self.artifacts["audio_test_executable"].resolve())
        self.assertEqual(artifact["sha256"], digest(self.artifacts["audio_test_executable"]))
        self.assertEqual(artifact["bytes"], self.artifacts["audio_test_executable"].stat().st_size)

    def test_audio_receipt_accepts_windows_rg_path_separators(self):
        self.rg_source_paths = r"crates/mondrian-audio/src\processor_isolation\tests\native_performance.rs"
        context = self.collect(["clap_worker", "vst3_worker", "clap_fixture", "vst3_fixture", "audio_test_executable"])
        paths = [entry["path"] for entry in context["source_receipts"]]
        self.assertIn("crates/mondrian-audio/src/processor_isolation/tests/native_performance.rs", paths)
        self.assertTrue(all("\\" not in path for path in paths))

    def test_openfx_receipt_deduplicates_mixed_enumerated_source_separators(self):
        self.rg_source_paths = "crates/mondrian-app/build.rs\n" + r"crates\mondrian-app\build.rs"
        context = self.collect(["openfx_worker", "openfx_reference", "openfx_test_executable"])
        paths = [entry["path"] for entry in context["source_receipts"]]
        self.assertEqual(len(paths), len(set(paths)), "each actual source must have exactly one canonical receipt")
        self.assertEqual(paths.count("crates/mondrian-app/build.rs"), 1)

    def test_audio_workers_without_test_executable_cannot_publish_a_receipt(self):
        with self.assertRaisesRegex(RuntimeError, "test-executable"):
            self.collect(["clap_worker", "vst3_worker", "clap_fixture", "vst3_fixture"])
        self.assertFalse((self.root / "context.json").exists())

    def test_orphan_audio_test_executable_is_not_a_complete_audio_group(self):
        with self.assertRaisesRegex(RuntimeError, "test-executable"):
            self.collect(["audio_test_executable"])
        self.assertFalse((self.root / "context.json").exists())

    def test_openfx_only_group_does_not_require_audio_executable(self):
        context = self.collect(["openfx_worker", "openfx_reference", "openfx_test_executable"])
        self.assertEqual({artifact["role"] for artifact in context["artifacts"]},
            {"openfx_worker", "openfx_reference", "openfx_test_executable"})


class WindowsPowerSchemeTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("power_collector_under_test", COLLECTOR)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.get_active = mock.Mock()
        self.free = mock.Mock(return_value=None)
        self.power = mock.Mock(PowerGetActiveScheme=self.get_active)
        self.kernel = mock.Mock(LocalFree=self.free)
        self.allocations = []
        self.expected_address = None
        self.get_active.side_effect = self.write_guid
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(self.module.ctypes, "WinDLL", side_effect=self.load_dll, create=True).start()
        mock.patch.object(self.module.ctypes, "get_last_error", return_value=6, create=True).start()

    def load_dll(self, name, *, use_last_error):
        self.assertTrue(use_last_error)
        self.assertIn(name, {"PowrProf.dll", "kernel32.dll"})
        return self.power if name == "PowrProf.dll" else self.kernel

    def write_guid(self, reserved, output):
        self.assertIsNone(reserved)
        pointer_type = self.get_active.argtypes[1]
        guid_type = pointer_type._type_._type_
        self.assertEqual(ctypes.sizeof(guid_type), 16)
        self.assertEqual([getattr(guid_type, name).offset for name in ("Data1", "Data2", "Data3", "Data4")], [0, 4, 6, 8])
        payload = guid_type.from_buffer_copy(bytes.fromhex("78563412bc9af0de0102030405060708"))
        self.allocations.append(payload)  # The oracle owns its fake allocation.
        self.expected_address = ctypes.addressof(payload)
        ctypes.cast(output, pointer_type)[0] = ctypes.pointer(payload)
        return 0

    def assert_one_release(self):
        self.free.assert_called_once()
        self.assertEqual(self.free.call_args.args[0].value, self.expected_address)
        self.assertEqual(self.free.argtypes, [ctypes.c_void_p])
        self.assertIs(self.free.restype, ctypes.c_void_p)

    def test_guid_has_exact_native_layout_and_one_pointer_width_release(self):
        receipt = self.module.windows_active_power_scheme()
        self.assertEqual(receipt, {"source": "PowrProf.dll!PowerGetActiveScheme",
            "guid": "12345678-9abc-def0-0102-030405060708"})
        self.assertIs(self.get_active.restype, ctypes.c_uint32)
        self.assertIs(self.get_active.argtypes[0], ctypes.c_void_p)
        self.get_active.assert_called_once()
        self.assert_one_release()

    def test_nonzero_direct_status_has_no_guid_read_or_retry(self):
        self.get_active.side_effect = lambda _reserved, _output: 5
        with mock.patch.object(self.module.ctypes, "string_at") as read:
            with self.assertRaisesRegex(OSError, "Win32 status 5") as failure:
                self.module.windows_active_power_scheme()
            self.assertEqual(failure.exception.errno, 5)
            read.assert_not_called()
        self.get_active.assert_called_once()
        self.free.assert_not_called()

    def test_null_success_is_explicitly_unavailable_without_dereference(self):
        self.get_active.side_effect = lambda _reserved, _output: 0
        with mock.patch.object(self.module.ctypes, "string_at") as read:
            with self.assertRaisesRegex(RuntimeError, "without a GUID allocation"):
                self.module.windows_active_power_scheme()
            read.assert_not_called()
        self.free.assert_not_called()

    def test_guid_copy_failure_still_releases_allocation_once(self):
        with mock.patch.object(self.module.uuid, "UUID", side_effect=ValueError("controlled GUID copy failure")):
            with self.assertRaisesRegex(ValueError, "controlled GUID copy failure"):
                self.module.windows_active_power_scheme()
        self.assert_one_release()

    def test_failed_release_does_not_publish_success_or_retry(self):
        self.free.side_effect = lambda handle: handle.value
        with self.assertRaisesRegex(OSError, "LocalFree.*Win32 status 6") as failure:
            self.module.windows_active_power_scheme()
        self.assertEqual(failure.exception.errno, 6)
        self.assert_one_release()

    def test_dll_unavailable_is_propagated_without_shell_fallback(self):
        with mock.patch.object(self.module.ctypes, "WinDLL", side_effect=OSError("controlled DLL unavailable")), \
            mock.patch.object(self.module, "run") as shell:
            with self.assertRaisesRegex(OSError, "controlled DLL unavailable"):
                self.module.windows_active_power_scheme()
            shell.assert_not_called()
        self.free.assert_not_called()

    def machine_with_power(self, *, failure=None):
        registry = mock.Mock()
        registry.OpenKey.side_effect = OSError("controlled registry unavailable")
        kernel = mock.Mock()
        kernel.GlobalMemoryStatusEx.return_value = 0
        receipt = {"source": "PowrProf.dll!PowerGetActiveScheme", "guid": "12345678-9abc-def0-0102-030405060708"}
        with mock.patch.dict(sys.modules, {"winreg": registry}), \
            mock.patch.object(self.module.platform, "platform", return_value="controlled Windows identity"), \
            mock.patch.object(self.module.platform, "win32_ver", return_value=("controlled", "", "", "")), \
            mock.patch.object(self.module.ctypes, "WinDLL", return_value=kernel), \
            mock.patch.object(self.module.ctypes, "WinError", return_value=OSError("controlled memory unavailable"), create=True), \
            mock.patch.object(self.module, "windows_active_power_scheme", return_value=receipt, side_effect=failure) as query, \
            mock.patch.object(self.module, "run") as shell:
            result = self.module.windows_machine()
            query.assert_called_once_with()
            shell.assert_not_called()
        return result, receipt

    def test_windows_machine_uses_structured_guid_without_locale_or_shell(self):
        result, receipt = self.machine_with_power()
        self.assertEqual(result["power_plan"], receipt)

    def test_windows_machine_records_native_failure_without_fabricated_guid(self):
        result, _ = self.machine_with_power(failure=OSError(5, "PowerGetActiveScheme returned Win32 status 5"))
        self.assertIsNone(result["power_plan"])
        self.assertTrue(any("Read-only active power-scheme GUID unavailable (no retry)" in item and
            "Win32 status 5" in item for item in result["limitations"]))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--collector", type=Path, default=COLLECTOR)
    parser.add_argument("--scratch-root", type=Path, default=SCRATCH)
    options = parser.parse_args()
    COLLECTOR = options.collector.resolve(strict=True)
    SCRATCH = options.scratch_root.resolve()
    unittest.main(argv=[sys.argv[0]], verbosity=2)
