# Coding Style

## General

- Prefer small modules with explicit ownership.
- Use structured data and typed IDs.
- Avoid hidden cross-layer calls.
- No `unwrap()` in production code unless the invariant is truly impossible and documented.
- Public API items need doc comments.

## Rust

- Use `thiserror`/structured errors for recoverable domain errors.
- Persist author time as canonical `TimelineTime`; resolve to `FramePosition` or
  audio samples once at an explicit evaluation boundary with a named rounding policy.
- Use `PropertyHost`/`PropertyMutation` for editable properties.
- Keep tests close to the behavior they protect.

## UI

- Use theme tokens for color, spacing, typography, radius, borders, and animation.
- Keep widget `paint()` side-effect free.
- Use `EventRequests` for platform side effects.
- Accessibility state must reflect real control state.

## Commits

Use Conventional Commits:

```text
feat(scope): subject
fix(scope): subject
refactor(scope): subject
docs(scope): subject
```
