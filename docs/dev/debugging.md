# Debugging

## UI

Use event-router diagnostics for:

- unmatched shortcut chords
- stale captured widgets
- stale hovered/drag/focused widgets
- unfocusable focused widgets
- overlay capture preemptions

Use widget tests to isolate layout/event/paint behavior before debugging the full app.

## Project Lifecycle

Project save/open issues usually involve:

- `.mdp` ZIP entries
- `project.json` serde shape
- runtime library extraction
- `library-generation-<uuid>/index.db` (live runtime; archive-only path remains
  `library/index.db`)
- autosave manifest normalization

Inspect the recovery-bearing runtime root under the stable per-user state
namespace:

- Windows: `%LOCALAPPDATA%/Mondrian/state/project-runtime-v4/`
- macOS: `~/Library/Application Support/Mondrian/state/project-runtime-v4/`
- other Unix: `$XDG_STATE_HOME/mondrian/project-runtime-v4/`, falling back to
  `~/.local/state/mondrian/project-runtime-v4/`

`library-generation-<uuid>/` is explicitly ephemeral; `autosave/` and its
manifest are recovery authority and must not be treated as disposable cache.

## Media

Separate probe failures from decode failures. Probe uses FFmpeg metadata; decode/render paths may fail later due codec, pixel format, frame availability, or cache state.

## Color

Check:

- selected `ColorEngine`
- OCIO config availability
- clip interpretation override
- sequence working/output spaces
- missing metadata policy
- display/export transform location

## Renderer

Name readback paths explicitly. If CPU pixels appear in a GPU-native path, confirm whether it is a final boundary, test capture, thumbnail, or accidental intermediate hop.
