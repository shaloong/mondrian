"""Reproduce canonical VST3 bindings from independently pinned source bytes.

By default this only prepares a fresh source tree. --run-generator explicitly
invokes the unchanged upstream generator with task-local Cargo home/target.
Native platform ABI qualification remains separate from source generation.
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import zipfile


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def blob(data: bytes) -> str:
    return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()


def download_archive(repository: str, commit: str) -> tuple[zipfile.ZipFile, str]:
    url = f"https://codeload.github.com/{repository}/zip/{commit}"
    request = urllib.request.Request(url, headers={"User-Agent": "Mondrian-VST3-source-reproduction"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                data = response.read(16 * 1024 * 1024 + 1)
            break
        except urllib.error.HTTPError as error:
            if error.code not in (408, 429, 500, 502, 503, 504) or attempt == 2:
                raise RuntimeError(f"Pinned source archive download failed: {url}") from error
        except (http.client.RemoteDisconnected, ConnectionError, TimeoutError) as error:
            if attempt == 2:
                raise RuntimeError(f"Pinned source archive download failed: {url}") from error
        except urllib.error.URLError as error:
            if not isinstance(error.reason, (ConnectionError, TimeoutError)) or attempt == 2:
                raise RuntimeError(f"Pinned source archive download failed: {url}") from error
        time.sleep(attempt + 1)
    if len(data) > 16 * 1024 * 1024:
        raise RuntimeError(f"Unexpected archive extent: {url}")
    return zipfile.ZipFile(io.BytesIO(data)), sha(data)


def read_source(archive: zipfile.ZipFile, repository: str, commit: str,
                relative: str, record: dict) -> bytes:
    path = Path(relative)
    if path.is_absolute() or ".." in path.parts or "\\" in relative:
        raise RuntimeError(f"Invalid pinned source path: {relative}")
    member = f"{repository.split('/')[-1]}-{commit}/{relative}"
    info = archive.getinfo(member)
    if info.is_dir() or info.file_size > 2 * 1024 * 1024:
        raise RuntimeError(f"Unexpected source extent: {relative}")
    data = archive.read(info)
    if sha(data) != record["sha256"] or blob(data) != record["git_blob_sha1"]:
        raise RuntimeError(f"Pinned source identity differs: {relative}")
    return data


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True, help="fresh generation directory")
    parser.add_argument("--worktree", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--target", default="x86_64-pc-windows-gnu")
    parser.add_argument("--libclang", type=Path)
    parser.add_argument("--cxx-include-bin", type=Path, help="compiler bin directory for Clang include discovery")
    parser.add_argument("--run-generator", action="store_true")
    args = parser.parse_args()
    worktree = args.worktree.resolve(strict=True)
    sdk = worktree / "vendor/vst3-sdk"
    source_manifest = sdk / "GENERATION-SOURCES.json"
    sources = json.loads(source_manifest.read_text(encoding="utf-8"))
    provenance = json.loads((sdk / "PROVENANCE.json").read_text(encoding="utf-8"))
    if sha(source_manifest.read_bytes()) != provenance["generation_input_receipt_sha256"]:
        raise RuntimeError("Generation source receipt does not match canonical provenance")
    destination = args.output.resolve()
    if destination.exists():
        raise RuntimeError("Use a fresh generation directory; no existing files are overwritten")
    destination.mkdir(parents=True)
    copied = []
    archives = {}
    archive_receipts = []
    for item in sources["sources"]:
        role, relative = item["role"], item["path"]
        if role == "deterministic-generator":
            continue
        official = role.startswith("official-")
        repository = "steinbergmedia/vst3_pluginterfaces" if official else "coupler-rs/vst3-rs"
        commit = sources["pluginterfaces_commit"] if official else sources["binding_commit"]
        identity = (repository, commit)
        if identity not in archives:
            archive, archive_sha = download_archive(repository, commit)
            archives[identity] = archive
            archive_receipts.append({"repository":repository, "commit":commit,
                                     "archive_sha256":archive_sha})
        data = read_source(archives[identity], repository, commit, relative, item)
        target = destination / "vst3sdk/pluginterfaces" / relative if official else destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        copied.append({"role":role, "path":relative, "sha256":sha(data)})
    generator = destination / "generate/src/main.rs"
    original = generator.read_text(encoding="utf-8")
    needle = "    find_headers_inner(dir, &mut headers)?;\n\n    Ok(headers)"
    if original.count(needle) != 1:
        raise RuntimeError("Original generator shape differs; do not replace ABI definitions")
    generator.write_text(original.replace(needle, "    find_headers_inner(dir, &mut headers)?;\n    headers.sort();\n\n    Ok(headers)", 1), encoding="utf-8", newline="\n")
    expected_delta = next(item for item in sources["sources"] if item["role"] == "deterministic-generator")
    if sha(generator.read_bytes()) != expected_delta["sha256"]:
        raise RuntimeError("Deterministic header-order patch differs from recorded generation")
    lock = (sdk / "GENERATION-Cargo.lock").read_bytes()
    if sha(lock) != provenance["generation"]["cargo_lock_sha256"]:
        raise RuntimeError("Independent generator lock differs from recorded provenance")
    (destination / "Cargo.lock").write_bytes(lock)
    receipt = {"schema":1, "generation_input_sha256":sha(source_manifest.read_bytes()),
               "sdk_commit":sources["sdk_commit"], "pluginterfaces_commit":sources["pluginterfaces_commit"],
               "generator_runtime_commit":sources["binding_commit"], "sources":copied,
               "archives":archive_receipts,
               "target":args.target, "generated":False,
               "qualification":"Generation and native MSVC/macOS C++ ABI qualification are separate gates."}
    if args.run_generator:
        if args.libclang is None:
            raise RuntimeError("--libclang is required when running the original generator")
        environment = os.environ.copy()
        environment["CARGO_HOME"] = str(destination / "cargo-home")
        environment["CARGO_TARGET_DIR"] = str(destination / "target")
        environment["LIBCLANG_PATH"] = str(args.libclang.resolve(strict=True))
        environment["TARGET"] = args.target
        if args.cxx_include_bin:
            compiler_bin = args.cxx_include_bin.resolve(strict=True)
            environment["PATH"] = str(compiler_bin) + os.pathsep + environment.get("PATH", "")
        command = ["cargo", "run", "--locked", "--manifest-path", "Cargo.toml", "-p", "generate"]
        with (destination / "generator.log").open("wb") as transcript:
            subprocess.run(command, cwd=destination, env=environment,
                           stdout=transcript, stderr=subprocess.STDOUT, check=True)
        binding_sha = sha((destination / "src/bindings.rs").read_bytes())
        receipt.update({"generated":True, "command":command,
                        "binding_sha256":binding_sha,
                        "matches_canonical":binding_sha == provenance["generated_binding_sha256"],
                        "transcript_sha256":sha((destination / "generator.log").read_bytes())})
        if binding_sha != provenance["generated_binding_sha256"]:
            (destination / "reproduction-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
            raise RuntimeError("Generated binding differs; inspect compiler/target evidence before modifying ABI declarations")
    (destination / "reproduction-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"output":str(destination), "generated":receipt["generated"]}))


if __name__ == "__main__":
    main()
