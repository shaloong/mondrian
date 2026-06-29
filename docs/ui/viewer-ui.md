# Viewer UI

Viewer is the preview and direct-manipulation surface.

## Stage

The stage should not add decorative borders around the preview image area. Safe margins, transform bounds, selection boxes, masks, and guides are purposeful overlays.

## Controls

Transport, fit/zoom, resolution, and quality controls must be compact. Dropdowns should open according to available window space, not just the owning panel bounds.

## Overlays

Viewer overlays include:

- selected clip bounds
- transform handles
- anchor/position controls
- masks and mask handles
- safe area guides
- eyedropper preview where applicable

Overlays must not alter the rendered frame. They are UI chrome.

## Color

Preview display transform belongs at viewer presentation. It must not modify timeline source data or exported data.
