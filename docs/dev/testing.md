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

## Frozen plugin host lab

`scripts/validation/invoke-plugin-host-lab.ps1` executes explicit frozen libtest
groups on Windows using `mondrian_plugin_lab_runner`, built from
`mondrian-validation-launcher`. It reuses that crate's suspended-spawn,
Job assignment/resume, kill-on-close and active-process accounting. A successful
parent exit with a live descendant fails the gate; the owner terminates and
settles the tree. The original process deadline and a separate two-second cleanup
budget are recorded. Output goes to create-only files without pipe draining.
This is test supervision, not preloader attestation or a native security sandbox.

A schema-1 manifest names `sourceRoot`, `sources` (`path`/`sha256`), `artifacts`
(`id`/absolute `path`/`sha256`), a `runner` artifact ID, and `groups`
(`id`/`artifact`/literal `arguments`/`expectedPassed`/`timeoutSeconds`). Optional
`environment` binds helper/fixture paths and runtime search paths. The producer
must select matching source and binaries; the runner does not prove that a
producer's source list is complete. Include the full relevant dependency closure,
every helper/plugin artifact and independent source/build receipts.

The controller retains artifact read leases, freezes manifest bytes, checks source
hashes before and after execution, and requires one exact passing libtest summary
with zero failures/ignored tests plus a successful native-owner report. Existing
evidence directories and duplicate group IDs are refused. Failed, timed-out or
unproved settlement cannot qualify a group. Receipts keep full-standard and
full-performance qualification false; group elapsed time is not a plugin benchmark.
See [the sprint record](plugin-host-sprint.md) for actual controls and boundaries.

## Reference media and projects

Reference media, Golden/Stress Project contracts, validation tiers, and Windows
machine evidence are specified in [Reference Validation](reference-validation.md).
The PR manifest gate is:

```powershell
pwsh -File scripts/validation/validate-reference-assets.ps1 -Tier Pr
```

Unit or simulated performance tests cannot substitute for a required real-media
nightly or release result.
