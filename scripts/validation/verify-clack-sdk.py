"""Verify canonical Clack source, licenses, provenance and unique package paths."""
from pathlib import Path
import argparse
import hashlib
import json
import tomllib

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def require(condition, detail):
    if not condition:
        raise RuntimeError(f"Canonical Clack validation failed: {detail}")

def verify(worktree):
    sdk = worktree / "vendor/clack"
    provenance = json.loads((sdk / "PROVENANCE.json").read_text(encoding="utf-8"))
    inputs = json.loads((sdk / "upstream-source-receipt.json").read_text(encoding="utf-8"))
    require(inputs["upstream_commit"] == "27ca28391b57a61bad4bb5099288a7bf9247350d", "pinned upstream commit")
    require(digest(sdk / "upstream-source-receipt.json") == "1dca0019a173c69ce5644abe6b91ddc6d4c4fcc5a323dc89b564a7ff39e5e9d7", "independently verified immutable input receipt")
    require(inputs["upstream_commit"] == provenance["upstream_commit"], "upstream identity")
    require(digest(sdk / "upstream-source-receipt.json") == provenance["source_input_receipt_sha256"], "upstream input receipt")
    tree = json.loads((sdk / "upstream-tree.json").read_text(encoding="utf-8"))
    require(digest(sdk / "upstream-tree.json") == inputs["tree_response_sha256"], "official tree receipt")
    entries = {entry["path"]: entry for entry in tree["tree"] if entry["type"] == "blob"}
    deltas = {item["path"]: item for item in provenance["local_deltas"]}
    require(len(deltas) == len(provenance["local_deltas"]), "duplicate source delta")
    for source in inputs["files"]:
        entry = entries[source["path"]]
        require((entry["sha"], entry["mode"]) == (source["git_blob_sha1"], source["mode"]), source["path"])
        value = (sdk / source["path"]).read_bytes()
        actual_sha = hashlib.sha256(value).hexdigest()
        if source["path"] in deltas:
            delta = deltas[source["path"]]
            require(delta["upstream_sha256"] == source["official_content_sha256"], "delta original source")
            require(delta["shipped_sha256"] == actual_sha, "delta shipped source")
            require(bool(delta["reason"]), "source delta reason")
        else:
            require(actual_sha == source["official_content_sha256"], "undeclared source modification")
            blob = hashlib.sha1(b"blob " + str(len(value)).encode() + b"\0" + value).hexdigest()
            require(blob == source["git_blob_sha1"], "unchanged official Git blob")
    source_paths = set()
    for shipped in provenance["shipped_files"]:
        path = Path(shipped["path"])
        require(not path.is_absolute() and ".." not in path.parts, "relative shipped path")
        require(path.as_posix() not in source_paths, "duplicate shipped path")
        source_paths.add(path.as_posix())
        require(digest(sdk / path) == shipped["sha256"], path)
    manifest = tomllib.loads((sdk / "Cargo.toml").read_text(encoding="utf-8"))
    require(manifest["workspace"]["members"] == ["common", "host", "plugin", "extensions"], "runtime workspace membership")
    require(manifest["workspace"]["dependencies"]["clap-sys"] == {"path": "../clap-sys"}, "direct canonical ABI source")
    for name in manifest["workspace"]["members"]:
        package = tomllib.loads((sdk / name / "Cargo.toml").read_text(encoding="utf-8"))
        require(package["package"]["name"] == "clack-" + name, "package name")
        require(package["package"]["version"] == "0.2.0", "package version")
        require(package["package"]["license"] == "MIT OR Apache-2.0", "package license")
        require(digest(sdk / name / "LICENSE-MIT") == digest(sdk / "LICENSE-MIT"), "materialized MIT license")
        require(digest(sdk / name / "LICENSE-APACHE") == digest(sdk / "LICENSE-APACHE"), "materialized Apache license")
    root = tomllib.loads((worktree / "Cargo.toml").read_text(encoding="utf-8"))
    for name in ["host", "plugin", "extensions"]:
        require(root["workspace"]["dependencies"]["clack-" + name]["path"] == "vendor/clack/" + name, "root canonical path")
    require(root["workspace"]["dependencies"]["clap-sys"] == {"path": "vendor/clap-sys"}, "root same canonical ABI package")
    require("clap-sys" not in root.get("patch", {}).get("crates-io", {}), "obsolete root ABI patch")
    for fixture in ["clap-sidechain", "clap-sample-gain"]:
        path = worktree / "crates/mondrian-audio/tests/fixtures" / fixture / "Cargo.toml"
        content = tomllib.loads(path.read_text(encoding="utf-8"))
        for name in ["plugin", "extensions"]:
            require((path.parent / content["dependencies"]["clack-" + name]["path"]).resolve() == (sdk / name).resolve(), "fixture canonical path")
        require("clap-sys" not in content.get("patch", {}).get("crates-io", {}), "obsolete fixture ABI patch")
    return {"status": "SOURCE_AND_MANIFEST_VERIFIED_NOT_RUNTIME", "upstream_commit": provenance["upstream_commit"],
        "provenance_sha256": digest(sdk / "PROVENANCE.json"), "shipped_files": len(source_paths),
        "root_and_fixture_canonical_paths": True, "cargo_lock_qualification": "must independently verify Cargo-produced locks and metadata after migration",
        "abi_native_performance_qualified": False}

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worktree", type=Path, default=Path.cwd())
    args = parser.parse_args()
    print(json.dumps(verify(args.worktree.resolve()), indent=2))
