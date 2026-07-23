# Timeline UI

Timeline is a primary editing surface and should prioritize precision, density, and low-latency interaction.

## Tracks

- Video tracks expose visibility/lock and visual controls.
- Audio tracks expose mute/solo/lock and waveform controls.
- Track header icon spacing should be compact and stable.
- Track heights are model-backed and should not shift due to hover labels.

## Clips

Clip rendering must show:

- media/generated kind
- label
- selection state
- trim handles
- disabled/offline state
- effect/mask/keyframe affordances when appropriate

Basic Title is an ordinary generated Clip for selection, placement, trim,
transform, effects, masks, transitions, and nesting. It uses a dedicated
semantic theme token so it is distinguishable from media and solid-color
sources without encoding domain meaning as a hard-coded Widget color.

## Time

The timeline ruler uses frame-exact time. Zoom and scroll must keep playhead and selection stable. Snapping should consider clip in/out, playhead, marks, and eventually keyframes.

## Playback Head

The playhead is a high-priority visual. It must be visible over clips, ruler, and keyframe lanes without stealing hit tests unintentionally.

## Waveforms

Waveform display mode is a user preference. Waveform generation/caching belongs in app/media adapters, not widget paint code.

## Menus

Timeline context menus should use the shared menu primitives and command/action system. Business logic belongs in actions/handlers.
