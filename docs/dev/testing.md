# Testing

## Test Levels

- Unit tests: pure models, property mutations, widget state machines, route logic.
- Integration tests: app UI action flows, project lifecycle, export queue.
- Visual tests: widget geometry/paint command invariants.
- Golden image tests: renderer/compositor output where deterministic.
- Media compatibility tests: FFmpeg probe/decode fixtures.
- Color accuracy tests: color transform plans and known sample conversions.
- Performance smoke tests: lifecycle/render/export paths.

## UI Tests

For widgets:

- pointer and keyboard focus separately
- disabled/hidden state cleanup
- layout under small/large bounds
- overlay hit/paint order
- tokenized visual metrics
- accessibility role/name/state/value

For event routing:

- focus traversal
- shortcut scope priority
- unmatched shortcut fallthrough
- pointer capture release
- stale widget cleanup
- IME ownership

## Commands

```bash
cargo test --workspace
cargo test -p mondrian-ui-widgets
cargo test -p mondrian-ui-events
cargo test -p mondrian-app app_ui
```

Ignored performance smoke tests write JSONL output when their env vars are set.
