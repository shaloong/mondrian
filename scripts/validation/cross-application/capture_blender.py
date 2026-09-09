"""Run inside pinned Blender with --background --factory-startup --disable-autoexec.

This acquires native project renders; it never generates qualification pixels
from expected values or attests an unobserved application setting.
"""
import hashlib
import json
import math
import os
from pathlib import Path
import sys

import bpy


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def exact_keys(value, expected, label):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise ValueError(label + " has missing or unknown fields")


def existing_file(root, raw, expected_hash):
    path = (root / raw).resolve(strict=True)
    if not path.is_file() or path.is_symlink() or digest(path) != expected_hash:
        raise ValueError("source file identity mismatch: " + str(path))
    return path


def main():
    arguments = sys.argv[sys.argv.index("--") + 1:]
    if len(arguments) != 2:
        raise ValueError("expected request JSON and fresh output directory")
    request_path = Path(arguments[0]).resolve(strict=True)
    request = json.loads(request_path.read_text(encoding="utf-8-sig"))
    exact_keys(request, ["schema_version", "run_id", "expected_version", "expected_build", "project", "project_sha256", "ocio", "ocio_sha256", "dependencies", "cases"], "Blender request")
    if type(request["schema_version"]) is not int or request["schema_version"] != 1 or not isinstance(request["run_id"], str) or not request["run_id"]:
        raise ValueError("invalid request identity")
    build = bpy.app.build_hash.decode("ascii")
    if bpy.app.version_string != request["expected_version"] or build != request["expected_build"]:
        raise ValueError("Blender version/build mismatch before opening project")
    project = existing_file(request_path.parent, request["project"], request["project_sha256"])
    ocio = existing_file(request_path.parent, request["ocio"], request["ocio_sha256"])
    # OCIO must be selected before process startup, not after a render context exists.
    if Path(os.environ.get("OCIO", "")).resolve() != ocio:
        raise ValueError("process OCIO does not match the frozen startup config")
    if not isinstance(request["cases"], list) or not 1 <= len(request["cases"]) <= 64:
        raise ValueError("case inventory must be bounded and nonempty")
    identities = set()
    for case in request["cases"]:
        exact_keys(case, ["case_id", "frame_index", "width", "height", "rate_numerator", "rate_denominator", "display", "view", "look", "exposure", "gamma", "format", "depth", "linear_output_space"], "Blender case")
        case_id = case["case_id"]
        if not isinstance(case_id, str) or not case_id or len(case_id) > 128 or any(character not in "abcdefghijklmnopqrstuvwxyz0123456789-_" for character in case_id) or case_id in identities:
            raise ValueError("case identity must be unique and path-safe")
        identities.add(case_id)
        for key in ["frame_index", "width", "height", "rate_numerator", "rate_denominator"]:
            if type(case[key]) is not int or case[key] < (0 if key == "frame_index" else 1):
                raise ValueError("invalid exact integer: " + key)
        if case["width"] * case["height"] > 33554432 or case["frame_index"] > 1048574:
            raise ValueError("render case exceeds bounded raster/frame inventory")
        if (case["format"], case["depth"]) not in [("PNG", "8"), ("OPEN_EXR", "32")]:
            raise ValueError("only exact RGBA8 PNG and lossless Float32 EXR captures are supported")
        if not all(type(case[key]) in (int, float) and math.isfinite(case[key]) for key in ["exposure", "gamma"]):
            raise ValueError("nonfinite color setting")
    if not isinstance(request["dependencies"], list) or len(request["dependencies"]) > 1024:
        raise ValueError("native dependency inventory is unbounded")
    dependencies = {}
    for item in request["dependencies"]:
        exact_keys(item, ["path", "sha256"], "native dependency")
        path = existing_file(request_path.parent, item["path"], item["sha256"])
        if path in dependencies:
            raise ValueError("duplicate native dependency")
        dependencies[path] = item["sha256"]
    output = Path(arguments[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    result = {"schema_version": 1, "run_id": request["run_id"], "producer": "blender", "version": bpy.app.version_string, "build": build,
              "executable_sha256": digest(Path(bpy.app.binary_path)), "project_sha256": request["project_sha256"], "ocio_sha256": request["ocio_sha256"],
              "adapter_sha256": digest(Path(__file__)), "request_sha256": digest(request_path), "status": "failed", "artifacts": []}
    try:
        bpy.context.preferences.filepaths.use_scripts_auto_execute = False
        bpy.ops.wm.open_mainfile(filepath=str(project), load_ui=False)
        # Referenced media and libraries must belong to the pinned parent lease
        # inventory before any render can count as acquired evidence.
        for collection in [bpy.data.images, bpy.data.sounds, bpy.data.movieclips, bpy.data.libraries, bpy.data.fonts]:
            for resource in collection:
                raw = getattr(resource, "filepath", "")
                if not raw or raw.startswith("<") or getattr(resource, "packed_file", None):
                    continue
                path = Path(bpy.path.abspath(raw, library=getattr(resource, "library", None))).resolve()
                if path not in dependencies or digest(path) != dependencies[path]:
                    raise ValueError("unfrozen native project dependency: " + str(path))
        scene = bpy.context.scene
        for case in request["cases"]:
            scene.render.resolution_x = case["width"]
            scene.render.resolution_y = case["height"]
            scene.render.resolution_percentage = 100
            scene.render.pixel_aspect_x = scene.render.pixel_aspect_y = 1.0
            scene.render.fps = case["rate_numerator"]
            scene.render.fps_base = case["rate_denominator"]
            scene.render.dither_intensity = 0.0
            scene.display_settings.display_device = case["display"]
            scene.view_settings.view_transform = case["view"]
            scene.view_settings.look = case["look"]
            scene.view_settings.exposure = case["exposure"]
            scene.view_settings.gamma = case["gamma"]
            settings = scene.render.image_settings
            settings.file_format = case["format"]
            settings.color_depth = case["depth"]
            settings.color_mode = "RGBA"
            # FOLLOW_SCENE ignores the file's requested linear output space and
            # writes the scene working space. Override must own both the display
            # transform and the linear file transform, with native readback.
            settings.color_management = "OVERRIDE"
            settings.display_settings.display_device = case["display"]
            settings.view_settings.view_transform = case["view"]
            settings.view_settings.look = case["look"]
            settings.view_settings.exposure = case["exposure"]
            settings.view_settings.gamma = case["gamma"]
            if case["format"] == "OPEN_EXR":
                settings.exr_codec = "ZIP"
                settings.linear_colorspace_settings.name = case["linear_output_space"]
            elif case["linear_output_space"] is not None:
                raise ValueError("display PNG must not claim a linear output colorspace")
            scene.frame_set(case["frame_index"])
            actual = {"width": scene.render.resolution_x, "height": scene.render.resolution_y,
                      "rate_numerator": scene.render.fps, "rate_denominator": scene.render.fps_base,
                      "display": settings.display_settings.display_device, "view": settings.view_settings.view_transform,
                      "look": settings.view_settings.look, "exposure": settings.view_settings.exposure, "gamma": settings.view_settings.gamma,
                      "format": settings.file_format, "depth": settings.color_depth,
                      "linear_output_space": settings.linear_colorspace_settings.name if case["format"] == "OPEN_EXR" else None,
                      "frame_index": scene.frame_current}
            for key, value in actual.items():
                if value != case[key]:
                    raise ValueError("Blender clamped or changed requested " + key)
            if settings.color_management != "OVERRIDE":
                raise ValueError("file output override is not active")
            suffix = ".png" if case["format"] == "PNG" else ".exr"
            target = output / (case["case_id"] + suffix)
            scene.render.filepath = str(target)
            bpy.ops.render.render(write_still=True)
            if not target.is_file() or target.stat().st_size == 0:
                raise ValueError("Blender returned without the expected artifact")
            # Blender renders/composites associated-alpha pixels. Its EXR save
            # path has no native straight-alpha setting; do not mislabel this as
            # straight coverage or recover hidden RGB by rewriting the artifact.
            output_contract = {"color_management": settings.color_management,
                               "alpha_association": "premultiplied" if case["format"] == "OPEN_EXR" else "straight",
                               "alpha_evidence": "native format convention; independent pixel verification required",
                               "qualification": "capture_only_not_qualified",
                               "straight_zero_alpha_rgb_preservation": "not_supported_by_this_compositor_path"}
            result["artifacts"].append({"case_id": case["case_id"], "path": target.name, "sha256": digest(target), "bytes": target.stat().st_size, "observed_settings": actual, "native_output_contract": output_contract})
        if any(digest(path) != sha for path, sha in dependencies.items()):
            raise ValueError("native dependency changed during acquisition")
        if digest(project) != request["project_sha256"] or digest(ocio) != request["ocio_sha256"]:
            raise ValueError("native project or OCIO changed during acquisition")
        result["status"] = "captured"
    except BaseException as error:
        result["failure"] = str(error)
        raise
    finally:
        (output / "capture.json").write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
