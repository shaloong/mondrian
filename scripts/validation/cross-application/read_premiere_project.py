"""Read observed native Project fields; never infer a supported color transform.

Premiere's private Project representation is diagnostic evidence only. A missing
or unknown field remains absent, and does not become an acquisition attestation.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import xml.etree.ElementTree as ET


def read_project(path):
    with path.open("rb") as source:
        encoded = source.read(64 * 1024 * 1024 + 1)
    if not encoded or len(encoded) > 64 * 1024 * 1024:
        raise ValueError("native Project encoded size exceeds readback bound")
    with gzip.GzipFile(fileobj=io.BytesIO(encoded), mode="rb") as stream:
        decoded = stream.read(64 * 1024 * 1024 + 1)
    if len(decoded) > 64 * 1024 * 1024:
        raise ValueError("native Project XML exceeds readback bound")
    if b"<!DOCTYPE" in decoded.upper() or b"<!ENTITY" in decoded.upper():
        raise ValueError("native Project declaration is not admitted")
    root = ET.fromstring(decoded)
    selected = []
    for item in root:
        if item.tag in {"Sequence", "VideoStream", "Media", "VideoSettings", "VideoTrackGroup"}:
            fields = []
            for node in item.iter():
                if not list(node) and node.text and node.text.strip() and node.get("Encoding") != "base64":
                    fields.append({"name": node.tag, "value": node.text.strip()})
            selected.append({"type": item.tag, "identity": item.attrib, "observed_fields": fields})
    return {"schema_version": 1, "status": "native-project-readback-only",
            "project_path": str(path.resolve()), "project_sha256": hashlib.sha256(encoded).hexdigest(),
            "decoded_project_sha256": hashlib.sha256(decoded).hexdigest(), "objects": selected}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    report = read_project(args.project)
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(report, output, ensure_ascii=False, indent=2, allow_nan=False)
        output.write("\n")


if __name__ == "__main__":
    main()
