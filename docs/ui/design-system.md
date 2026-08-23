# Design System

Mondrian is a dense production editor, not a landing page. The UI should feel quiet, precise, and modern.

## Theme Tokens

All visual values must come from `mondrian-ui-theme` tokens:

- colors
- typography
- spacing
- radius
- borders
- animation duration/easing
- accessibility scaling

Dark and Light are the only concrete built-in themes. System resolves to one of them at runtime.

## States

Controls should consistently expose:

- normal
- hover
- active/pressed
- selected
- focused
- disabled

Visible focus rings are for keyboard traversal. Pointer focus owns input but should normally not show a ring.

## Typography

Use compact type. No negative letter spacing. Tool surfaces, sidebars, compact panels, and row labels should use body/small token styles rather than hero-scale text.

## Surfaces

Panels use restrained backgrounds and separators. Avoid nested cards. Cards are for repeated items, modal contents, and framed tools only.

## Icons

Use SVG/vector icons through the icon pipeline. Icon-only buttons need accessible names, usually from tooltip text.

## Scrollbars

Scrollbars should be compact, tokenized, and visually stable. Drag handles must not cause layout shifts.
