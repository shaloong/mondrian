# Mondrian Documentation

These docs are the product and engineering contract for Mondrian's current self-hosted UI and editor architecture. They should stay short, factual, and close to code.

## Map

- `architecture/`: system shape, crate boundaries, runtime flows, and long-term direction.
- `specs/`: data contracts for project files, assets, sequences, clips, effects, masks, color, export, and shortcuts.
- `ui/`: product UI design rules and interaction patterns for the self-hosted editor.
- `dev/`: setup, build, testing, debugging, profiling, and module-boundary rules.

## Maintenance Rules

- Update docs when changing a public data shape, crate boundary, UI interaction contract, persistence format, render path, or plugin-facing API.
- Prefer documenting stable principles and current constraints over implementation trivia.
- If code and docs disagree, either update the code or explicitly mark the doc section as a target direction with migration notes.
- Do not resurrect legacy egui architecture as a compatibility target. The self-hosted UI is now the main product line.
