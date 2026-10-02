# Dependency Security and Exceptions

Rust dependency policy is [deny.toml](../../deny.toml), with sources and exact
Git revisions visible in [Cargo.lock](../../Cargo.lock). Git sources must use an
explicit revision and belong to the repository allowlist; it includes the
existing pinned OCIO and native CLAP/VST3 adapters and fixtures. New repositories
require review before allowance. PR/push CI checks
advisories, licenses, bans and sources. A separate weekly audit refreshes advisory
knowledge even without source changes; Dependabot proposes Rust and Action
updates to develop. These workflows take effect after merge and do not imply
that alerts have been routed or every scheduled run succeeds.

The audit's default branch snapshot does not cover all historical releases or
an unpromoted develop branch. Before releasing, check the exact candidate's
lockfile against the current database. A failed audit is triaged by maintainers;
review reachability and fix exploitable problems rather than blindly adding
ignore entries. Review Action updates despite their immutable SHA pins.

## Existing Rust advisory exceptions

The following rationales are preserved from the current configuration. They are
not new findings, a fresh exploitability assessment, or blanket accepted risk.

| Advisory | Recorded dependency/rationale | Removal condition |
| --- | --- | --- |
| RUSTSEC-2026-0194 / RUSTSEC-2026-0195 | quick-xml selected by xcb/wayland-scanner build-time XML parsing; configuration says product media/project input does not reach these parsers | Upstream resolves to quick-xml >=0.41, or evidence changes the build-input exposure |
| RUSTSEC-2026-0192 | ttf-parser 0.25 unmaintained; selected by the existing text/window stack | Upstream migrates or a reviewed replacement becomes available |
| RUSTSEC-2024-0436 | paste unmaintained, selected by pulp/exr; configuration describes no known vulnerability/safe upgrade | Upstream switches or a reviewed alternative becomes appropriate |

Maintainers own exception review until named dependency handlers are appointed.
At least quarterly and before every release, recheck the upstream advisory,
resolved dependency path, attacker-controlled build/runtime inputs and removal
condition; record dated results and a next review date. Do not infer safety just
from the word build-time or unmaintained. Cargo ignores suppress alerts and
must not suppress reports through the security policy.

## Native and distributed dependencies

Cargo advisory checks do not cover the complete shipped runtime. Inventory
FFmpeg, OCIO, vcpkg ports, transitive DLL/dylib/ELF dependencies, fonts, device SDKs,
and independently supplied native tools. Record exact source revision and
version, upstream security/advisory source, binary identity, license/notices,
update route, and who reviews the component. The release's VCPKG_REF tag is a
version reference, not an immutable source commitment.

For each release, retain the inventory or SBOM tied to the candidate source and
package hash; review actual encoder/decoder/filter configuration and affected
platforms. A linked-only Rust BOM omits privately bundled executables and native
libraries. Verify binary closure and applicable license/source-distribution
requirements separately from security scanning. Current packaging has closure
and runtime-capability checks, but a complete native security inventory/SBOM and
periodic advisory coverage are still open work.
