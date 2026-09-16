"""Build the native image-sequence compositor fixture using Blender 5.1 APIs."""
import json
from pathlib import Path
import sys
import bpy


directory, output = [Path(value).resolve() for value in sys.argv[sys.argv.index("--") + 1:]]
inventory = json.loads((directory / "stimulus.json").read_text(encoding="utf-8"))
if inventory["kind"] != "analytic-input-only" or output.exists():
    raise ValueError("fresh native project and analytic input required")
scene = bpy.context.scene
scene.render.engine = "BLENDER_WORKBENCH"
scene.render.resolution_x = scene.render.resolution_y = 64
scene.render.resolution_percentage = 100
scene.render.fps = 24000
scene.render.fps_base = 1001
scene.render.use_compositing = True
tree = bpy.data.node_groups.new("Frozen analytic input sequence", "CompositorNodeTree")
scene.compositing_node_group = tree
tree.interface.new_socket(name="Image", in_out="OUTPUT", socket_type="NodeSocketColor")
destination = tree.nodes.new("NodeGroupOutput")
source = tree.nodes.new("CompositorNodeImage")
image = bpy.data.images.load(str(directory / "input-0000.exr"), check_existing=False)
image.source = "SEQUENCE"
image.colorspace_settings.name = "Linear Rec.2020"
image.alpha_mode = "STRAIGHT"
source.image = image
source.frame_start = 0
source.frame_duration = 120
# Image-sequence indexing adds one before applying the filename offset. For an
# input-0000.exr sequence with scene origin zero, -1 keeps scene 0 on input 0000.
# Independent decoded frame-identity pixels additionally check this mapping;
# Blender 5.1's node RNA does not expose the evaluated image-user filepath.
source.frame_offset = -1
source.use_auto_refresh = True
if source.frame_start != 0 or source.frame_duration != 120 or source.frame_offset != -1:
    raise ValueError("Blender clamped the exact source sequence frame mapping")
tree.links.new(source.outputs["Image"], destination.inputs["Image"])
scene.frame_start = 0
scene.frame_end = 119
bpy.ops.wm.save_as_mainfile(filepath=str(output), check_existing=False)
print("Frozen native compositor project created:", output)
