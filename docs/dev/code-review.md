# Code Review Requirements

Submit external changes to `develop`; `main` is promoted from the repository's
`develop` branch. Keep PRs focused and explain the problem, resulting behavior,
compatibility impact, and validation. Review the final diff after the last
material change, including generated code and workflow changes.

## Acceptance requirements

- The change solves the stated problem and respects the domain model and crate
  boundaries. Record significant decisions in an ADR or architecture document.
- Rust follows [coding style](coding-style.md), `cargo fmt`, and strict Clippy.
  Check native/shader changes with the tools appropriate to their language.
- Major new behavior includes automated tests. Bug fixes should add a regression
  test reproducing the failure before the fix; explain exceptions in the PR.
  Review failure paths, cancellation and cleanup, not only successful behavior.
- Validate schema/project/plugin compatibility and document migrations. Review
  performance-sensitive paths using the existing product/reference gates.
- Check input limits, integer arithmetic, allocation, archive/path handling,
  FFI lifetimes, subprocess arguments/environment, resource ownership and
  privilege changes when applicable. See [security design](../security/design.md).
- Check dependency source/revision, advisories, licenses, and distributed native
  runtime changes. Revisit advisory exceptions rather than copying them forward.
- Confirm documentation and user-visible claims reflect implemented behavior.
  No credentials, private media, or contribution-authority records enter Git.
- Applicable CI checks and CLA checks must succeed on the final candidate.
  An unrelated green commit or a skipped hardware run is insufficient evidence.

## Independent review

The project targets review by another human for every production change.
At least 50% of proposed modifications must receive author-independent review
before release to meet the OpenSSF Gold criterion `two_person_review`.
Reviewers record their assessment on GitHub and resolve material concerns before
approval. An AI review can assist but does not constitute another person's review.

Re-review material changes after approval. When only the author is available,
record the lack of independent review; do not self-approve or imply that an
approval rule, bot comment, or template proves the criterion is met. Review
coverage and administrator/backup availability remain evidence gaps until
confirmed. Urgent security work uses the private response process and still
records the reviewer and release decision privately until disclosure.

Repository administrators should require CI and an independent approval when
the reviewer roster can support it. Verify live GitHub rules rather than inferring
enforcement from this document.
