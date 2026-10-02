# Contributing

Bug reports, feature requests, and pull requests are welcome in English or
Chinese. Please follow the [code of conduct](../CODE_OF_CONDUCT.md).
Report suspected vulnerabilities [privately](../.github/SECURITY.md).

## Getting started

Use [GitHub Issues](https://github.com/shaloong/mondrian/issues/new/choose) for
bugs and proposals. Include reproduction steps for bugs; discuss substantial
changes before implementing them.

Follow [development setup](dev/setup.md) for the pinned Rust toolchain and
native dependencies. On Windows, use PowerShell 7 and the
[Windows development guide](dev/windows-development.md), then run:

```powershell
. ./scripts/enter-windows-development.ps1
cargo build --locked
```

On Linux and macOS, configure the native dependencies in the setup guide before
running `cargo build --locked`.

## Making a change

Branch from `develop` and submit your PR to **develop**. Use a descriptive
branch name such as `feature/clip-search` or `fix/export-metadata`. Keep the
change focused and explain the problem, resulting behavior, and validation
using the PR template.

Follow [coding style](dev/coding-style.md) and the
[review requirements](dev/code-review.md). Add tests for new behavior and bug
fixes, and update relevant documentation. Do not commit credentials, private
media, or large downloaded samples; use small, redistributable fixtures.
For performance changes, follow [performance profiling](dev/performance-profiling.md).

Run the checks relevant to your change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
```

CI runs additional product checks; consult
[the CI workflow](../.github/workflows/ci.yml) for affected UI and platform paths.
Documentation-only changes do not require running the full product test suite.

Use Conventional Commit messages, for example:

```text
feat(timeline): add Bezier keyframe interpolation
fix(media): release decoder resources on cancellation
docs: clarify Windows setup
```

## Contribution agreement

Contributions require the [CLA](legal/CLA.md). Read the
[privacy notice](legal/CLA-PRIVACY.md) and follow the
[signing instructions](legal/CLA-SERVICE.md). Contributors retain copyright;
the CLA includes commercial relicensing permission. Using Mondrian or publishing
an independent plugin does not require signing it.

Keep private authorization records out of public issues and PRs. Contact
**contact@shaloong.com** for signing or authorization questions.
