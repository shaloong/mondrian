## Problem and change / 问题与变更

Describe the user problem, resulting behavior, and relevant issue (if any).
Please target **develop**; maintainers promote develop to main separately.

## Validation / 验证

Record commands, results, platform/runtime versions, and relevant evidence.
Explain checks that were not run. Add regression coverage for behavior changes;
do not mark skipped or hardware-unavailable runs as passed.

## Compatibility and security / 兼容性与安全

Describe project/schema/plugin compatibility and migration needs, if affected.
Call out file parsing, FFI/unsafe code, subprocesses, paths, dependencies,
workflow permissions, or packaging changes needing closer review.
Report undisclosed vulnerabilities privately through [the security policy](https://github.com/shaloong/mondrian/blob/main/.github/SECURITY.md).

## Review checklist / 审查清单

- [ ] I have read the contribution and review requirements.
- [ ] Tests and documentation match the changed behavior, or I explained why they are not needed.
- [ ] Submitted code and fixtures have appropriate provenance and licensing; private data and credentials are absent.
- [ ] I completed the applicable CLA process, or identified an authorization question for maintainers.
