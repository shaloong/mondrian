# Security Design and Evidence

Initial controls reviewed against source `a1925ca3bcc6faf2868ab35bf40cc584da4f2537`
on 2026-10-02; native-plugin scope aligned with develop source
`2a418ecb86d9b8aecd7cf42aa6f9477e03bd0228` on 2026-10-03.
This is an initial design/evidence inventory, not an exhaustive security audit,
an OpenSSF certification, or a list of findings that may be ignored.

## System and trust boundaries

Mondrian is a native desktop video editor with a self-hosted winit/wgpu UI.
It runs with the user's filesystem and process privileges. Assets include
project state, source media, exports, caches, user configuration, and build and
release authority. Inputs from another person remain untrusted even if stored
on a local disk.

| Boundary | Untrusted input or authority | Required security property |
| --- | --- | --- |
| Project and interchange import | ZIP/JSON/XML documents, entry names, lengths, schema and referenced paths | Validate structure and semantics; bound parsing and allocation; reject ambiguous entries and unauthorized writes |
| Media, LUT, font and image processing | Container/codec metadata, pixels, color profiles, LUT values and font tables | Bound resources; validate assumptions before indexing or FFI; contain child lifetime and failure |
| Storage and publication | Destinations, existing objects, links and concurrent replacement | Publish the intended object; preserve foreign replacements; cleanup only objects owned by the operation |
| Native libraries and helpers | FFmpeg/OCIO and device bridges, executables and dependent libraries | Explicit lifetime/ownership contracts; trusted runtime selection; avoid shell evaluation of input |
| Plugins and future network/AI integration | Contributed plugin code, workflow/config input, provider endpoints and credentials | Treat executable plugins as trusted code unless isolation is implemented; evaluate actual network/credential paths before making guarantees |
| Build and release | PR code, Actions, registries, downloaded SDKs and packaged DLLs | Least privilege, immutable action identity, dependency provenance, trusted source qualification and review before publication |

## Existing controls and where to verify them

| Property | Implementation and test evidence | Limit of the evidence |
| --- | --- | --- |
| Project archive admission limits compressed and declared/actual entry sizes | [project module](../../crates/mondrian-project/src/lib.rs): `ProjectArchiveReadBudget`, `BudgetedArchiveEntryReader`; tests `archive_budget_rejects_compressed_and_declared_lengths_before_json_parse`, `archive_entry_reader_rejects_actual_length_over_budget_and_declared_mismatch` | Covers these archive readers; does not establish boundedness of every media parser |
| Exact archive entry set, duplicate/directory rejection, semantic validation | [project module](../../crates/mondrian-project/src/lib.rs): `exact_entry_contract_precedes_manifest_parse_and_rejects_duplicates_and_directories`, `reopen_verifier_rejects_any_non_contract_archive_entry` | Schema validation and CRC/hash checks are not proof of authentic authorship |
| Publication checks object identity and preserves a foreign replacement | [storage module](../../crates/mondrian-storage/src/lib.rs), [publication architecture](../architecture/storage-publication.md); `external_reservation_rejects_and_preserves_an_observed_replacement`, project test `prepublication_identity_recheck_rejects_and_preserves_a_foreign_replacement` | Platform-specific guarantees must be verified on each supported OS; not a general filesystem sandbox |
| Native child execution has deadline/cancellation and lifecycle handling | [FFmpeg command](../../crates/mondrian-media/src/ffmpeg_command.rs), [process supervisor](../../crates/mondrian-media/src/process_supervisor.rs); `expired_native_spawn_deadline_rejects_before_creating_child` | Process supervision is not decoder memory isolation or a hostile-code sandbox |
| Qualification can retain approved provider/runtime identity | [approved provider](../../crates/mondrian-media/src/approved_provider_command.rs), [qualified runtime](../../crates/mondrian-media/src/qualified_ffmpeg.rs); `approved_provider_rejects_foreign_handle_even_with_identical_bytes` | Qualification/validation-specific behavior must not be advertised as a universal ordinary-build guarantee |
| Packaged runtime checks require bundled FFmpeg tools and product capabilities | [runtime verifier](../../crates/mondrian-media/src/ffmpeg_runtime.rs), [tool resolution](../../crates/mondrian-media/src/ffmpeg_tools.rs), [release workflow](../../.github/workflows/release.yml) | Ordinary development/tool resolution can use PATH; runtime capability checks are not malware authentication |
| Rust advisories, licenses and allowed sources are checked | [deny policy](../../deny.toml), [CI](../../.github/workflows/ci.yml), [periodic audit](../../.github/workflows/dependency-audit.yml) | Cargo advisories do not cover the bundled FFmpeg/OCIO/SDK runtime; ignored advisories need continued review |
| External Action identities are pinned and checked | [pin validator](../../scripts/validation/validate-github-actions-pins.ps1) | Pinned code still needs review and updates; a release tag for a native registry can be moved |
| Release binds a source SHA to CI, qualification and a candidate package | [release workflow](../../.github/workflows/release.yml), [contribution guide](../CONTRIBUTING.md) | Provenance JSON and checksums are not cryptographic signatures or independent reproducible-build evidence |

## Current limitations and verification work

Do not claim arbitrary media, native libraries, executable plugins, drivers, or
the desktop process are sandboxed. Source-crate SDK package distribution remains planned. Separate OpenFX and
CLAP/VST3 adapters load installed native plugins in supervised children; see
[Plugin System](../architecture/plugin-system.md) for their admitted operations.
Crash/hang containment does not establish a hostile-code security sandbox. A compromised OS, driver or toolchain invalidates
assumptions and needs an explicit deployment-specific analysis, rather than an
automatic exclusion of a reported problem.

A complete assurance case still needs per-boundary abuse cases, dynamic analysis
of parsers and native/unsafe paths, native-dependency monitoring, runtime
hardening verification, and a recorded security review. Static inspection and
unit-test presence are useful evidence, but they do not prove those goals.
No project-wide coverage, reproducible-build, release-signature or recent
independent-security-review result is asserted here.

For changes crossing a boundary, record the requirement, attacker-controlled
input, intended control, regression/dynamic test, platform/feature assumptions,
and remaining limitation. Update the relevant architecture and validation
documents. Suspected defects follow the
[private reporting policy](../../.github/SECURITY.md).
