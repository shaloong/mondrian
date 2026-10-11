"""Verify official CLAP ABI input, shipped source and unique path ownership."""
from pathlib import Path
import argparse
import hashlib
import json
import tomllib

def digest(data): return hashlib.sha256(data).hexdigest()

def verify(worktree):
    sdk = worktree / "vendor/clap-sys"
    provenance = json.loads((sdk / "PROVENANCE.json").read_text(encoding="utf-8"))
    inputs = json.loads((sdk / "OFFICIAL-SOURCES.json").read_text(encoding="utf-8"))
    assert inputs["official_commit"] == "195b42a004144fab0b3cf95e9c067187d15365b7"
    assert digest((sdk / "OFFICIAL-SOURCES.json").read_bytes()) == provenance["official"]["receipt_sha256"]
    assert len(inputs["files"]) == 71
    for item in inputs["files"]:
        relative = Path(item["path"])
        assert not relative.is_absolute() and ".." not in relative.parts
        value = (sdk / "official" / relative).read_bytes()
        assert len(value) == item["bytes"] and digest(value) == item["sha256"], relative
        assert hashlib.sha1(b"blob " + str(len(value)).encode("ascii") + b"\0" + value).hexdigest() == item["git_blob"], relative
    assert (sdk / "LICENSE-CLAP.txt").read_bytes() == (sdk / "official/LICENSE").read_bytes()
    assert provenance["generator"]["actual_output_sha256"] == "0300511ca69490436738c05d6e5051de6745524a95029a78ba95ce2f43b74ca8"
    assert digest((sdk / "src/raw.rs").read_bytes()) == provenance["shipped_raw"]["sha256"]
    if provenance["shipped_raw"]["format_delta"] is None:
        assert provenance["shipped_raw"]["sha256"] == provenance["generator"]["actual_output_sha256"]
    seen = set()
    for item in provenance["shipped_files"]:
        relative = Path(item["path"])
        assert not relative.is_absolute() and ".." not in relative.parts
        assert relative.as_posix() not in seen
        seen.add(relative.as_posix())
        value = (sdk / relative).read_bytes()
        assert len(value) == item["bytes"] and digest(value) == item["sha256"], relative
    root = tomllib.loads((worktree / "Cargo.toml").read_text(encoding="utf-8"))
    clack = tomllib.loads((worktree / "vendor/clack/Cargo.toml").read_text(encoding="utf-8"))
    assert root["workspace"]["dependencies"]["clap-sys"] == {"path": "vendor/clap-sys"}
    assert clack["workspace"]["dependencies"]["clap-sys"] == {"path": "../clap-sys"}
    assert "clap-sys" not in root.get("patch", {}).get("crates-io", {})
    for name in ("clap-sidechain", "clap-sample-gain"):
        path = worktree / "crates/mondrian-audio/tests/fixtures" / name / "Cargo.toml"
        fixture = tomllib.loads(path.read_text(encoding="utf-8"))
        for package in ("plugin", "extensions"):
            assert (path.parent / fixture["dependencies"]["clack-"+package]["path"]).resolve() == (worktree / "vendor/clack" / package).resolve()
        assert "clap-sys" not in fixture.get("dependencies", {})
        assert "clap-sys" not in fixture.get("patch", {}).get("crates-io", {})
    return {"status": "SOURCE_AND_MANIFEST_VERIFIED_NOT_ABI_OR_PLUGIN_QUALIFIED", "official_commit": inputs["official_commit"],
        "shipped_source_files": len(seen), "raw_shipped_sha256": provenance["shipped_raw"]["sha256"],
        "normal_cargo_lock_migration_required": True, "native_platform_receipts_required": True}

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worktree", type=Path, default=Path.cwd())
    args = parser.parse_args()
    print(json.dumps(verify(args.worktree.resolve()), indent=2))
