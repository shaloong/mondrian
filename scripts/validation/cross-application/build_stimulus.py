"""Generate input stimulus, never vendor output or a qualification oracle.

Single-part Float32 EXR layout follows https://openexr.com/en/latest/OpenEXRFileLayout.html.
Readers independently validate these files before any comparison can qualify.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def attribute(name, kind, value):
    return name.encode() + b"\0" + kind.encode() + b"\0" + struct.pack("<I", len(value)) + value


def pixel(x, y, frame, patches):
    # Distinct top-row binary frame identity; no auto-alignment is permitted.
    if y == 0:
        bit = (frame >> (x % 8)) & 1
        return (float(bit), 0.0, float(1 - bit), 1.0)
    if y < 17:
        rgb = patches[(x // 8 + ((y - 1) // 8) * 8) % len(patches)]
    elif y < 33:
        rgb = (x / 63.0,) * 3
    elif y < 49:
        rgb = (float(x % 2), float((x + 1) % 2), 0.5)
    else:
        value = -0.125 + 16.125 * x / 63.0
        rgb = (value, value, value)
    # Retain color under zero alpha and RGB greater than coverage.
    alpha = (0.0, 0.25, 0.5, 1.0)[min(y // 16, 3)]
    return tuple(rgb) + (alpha,)


def exr(frame, patches):
    channels = b"".join(name.encode() + b"\0" + struct.pack("<iB3xii", 2, 0, 1, 1) for name in "ABGR") + b"\0"
    box = struct.pack("<4i", 0, 0, 63, 63)
    header = struct.pack("<II", 20000630, 2)
    for name, kind, value in [
        ("channels", "chlist", channels), ("compression", "compression", b"\0"),
        ("dataWindow", "box2i", box), ("displayWindow", "box2i", box),
        ("lineOrder", "lineOrder", b"\0"), ("pixelAspectRatio", "float", struct.pack("<f", 1)),
        ("screenWindowCenter", "v2f", struct.pack("<2f", 0, 0)), ("screenWindowWidth", "float", struct.pack("<f", 1)),
        ("chromaticities", "chromaticities", struct.pack("<8f", .708, .292, .170, .797, .131, .046, .3127, .3290)),
        ("mondrianFrameIndex", "int", struct.pack("<i", frame)),
    ]:
        header += attribute(name, kind, value)
    header += b"\0"
    rows = []
    for y in range(64):
        values = [pixel(x, y, frame, patches) for x in range(64)]
        samples = b"".join(struct.pack("<64f", *(value[channel] for value in values)) for channel in (3, 2, 1, 0))
        rows.append(struct.pack("<iI", y, len(samples)) + samples)
    offset = len(header) + 64 * 8
    offsets = []
    for row in rows:
        offsets.append(offset)
        offset += len(row)
    return header + struct.pack("<64Q", *offsets) + b"".join(rows)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    manifest_bytes = args.manifest.read_bytes()
    manifest = json.loads(manifest_bytes)
    if manifest["raster"] != {"width": 64, "height": 64, "pixel_aspect_ratio": "1/1", "orientation": "top-left"}:
        raise ValueError("this edition supports the fixed 64x64 input stimulus only")
    if manifest["timeline"]["required_frame_indices"] != [0, 17, 119]:
        raise ValueError("unreviewed temporal stimulus")
    args.output.mkdir(parents=True, exist_ok=False)
    inventory = []
    for frame in range(120):
        data = exr(frame, manifest["patches"]["required_rgb"])
        name = f"input-{frame:04d}.exr"
        (args.output / name).write_bytes(data)
        inventory.append({"path": name, "sha256": hashlib.sha256(data).hexdigest(), "frame_index": frame})
    (args.output / "stimulus.json").write_text(json.dumps({"schema_version": 1, "kind": "analytic-input-only",
        "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(), "generator_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "color_space": "LinearRec2020", "alpha": "straight_coverage", "orientation": "top-left", "frames": inventory}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
