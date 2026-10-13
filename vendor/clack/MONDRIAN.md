# Canonical Clack runtime

This directory preserves the complete common/host/plugin/extensions source,
tests, documentation and examples from immutable upstream 27ca28391b57a61bad4bb5099288a7bf9247350d.
Only these four runtime packages are workspace members. Finder/example packages
are not product dependencies; the unchanged example sources are retained for
provenance rather than advertised as independently runnable standalone packages.
Upstream author and MIT/Apache-2.0 licenses are preserved, including fully
materialized package licenses in place of upstream symlinks.

Local changes: runtime workspace membership; one official-header-generated clap-sys
path package for every standalone/root/fixture build; and an immutable-borrow
AudioPorts allocation observer for both actual Vec capacities/layouts. The
observer reports host-owned pointer/configuration storage, not PCM/plugin memory.
No CLAP lifecycle or ABI implementation was manually replaced.

PROVENANCE.json binds every shipped source/license/input receipt. Formatting
changes must be recorded with their actual hashes and explicit upstream deltas.
The four 0.2.0 packages retain upstream edition 2024 / MSRV 1.85.0; the product
requires Rust 1.97.1. Cargo owns every lockfile migration; do not edit locks by hand.

The wrapper revision and ABI package version alone do not mean the host implements
all latest CLAP 1.2.10 features. See ../../docs/dev/native-audio-standard-features.md
and ../../docs/native-plugin-compatibility.md for actual feature/receipt boundaries.
Source verification is not ABI, native-plugin, allocator, platform or deadline
qualification. The permanent verify script checks source before qualification.

## Bounded Host Log observer

The local HostLogImpl contract receives a borrowed fixed prefix (512 bytes),
exact signed severity, termination and null-pointer facts. The seven official
severities are distinct; invalid UTF-8 and unknown values are not silently
replaced. The native callback reads at most513 bytes and uses the observer-only
HostWrapper entrypoint: no initialization callback, recursive fallback log,
formatting or stderr on the normal log path. Observer implementations remain
responsible for realtime-safe admitted storage; example terminal sinks are
explicitly not realtime safe. Panic hooks remain process policy.

The owned callback oracle covers all seven native severities plus defensive
unknown/invalid/null/truncation inputs; defensive cases are not conforming-vendor
qualification. Product deadline, full owner grant, UI delivery and platform
qualification are separate receipts.


## Official 1.2.10 ABI input migration

All runtime/root/fixture consumers now resolve one direct `../clap-sys` package.
Its unchanged generated raw declarations come from the complete immutable
official include tree. Header-local version/color values have no exported C
symbol; independent native comparison checks their Rust value projections.
Clack uses checked enum-value projections into actual event/flag/array carriers
and the real generated anonymous window union, without synthetic FFI aliases.
Storage owner APIs and the exact libloading 0.8.9 implementation remain selected.
Source/ABI input migration is not complete host-extension or vendor qualification.


## Thread-safe GUI false replies

The host GUI resize/show/hide adapters map an implementation's ordinary refusal
directly to the official bool reply. They do not turn that legal false reply
into HostWrapper error formatting or a synthetic Host Log record. Successful
replies are unchanged. Wrapper pointer/panic failures retain the existing error
boundary; this is not a realtime panic-recovery promise.

The owned Clack ABI allocation example invokes all three callbacks from a joined
plugin background thread, after warming the real wrappers. Thread construction,
teardown, assertions and printing are outside its System allocation interval.
This narrow control does not establish a product GUI lifetime, audio-role
deadline, all SDK allocation ownership or third-party plugin compatibility.

## Same-call parameter query observer

The additive `get_info_observed` and `get_value_observed` methods report owned
numeric evidence from the one existing native callback and its original
conversion. Ordinary methods share that core with no observer. Missing or false
metadata output is unavailable and is never read; successful raw and converted
numeric bits remain separate, including a reserved identity rejected by the
original conversion. Current value is separate from metadata default. A fresh
buffer construction fact is not a generic before-memory snapshot.

These are diagnostic main-thread, non-realtime APIs. Observers must be bounded,
must not requery or service the plugin, and must not change admission. Formatting
and file publication belong to an outer owner. Observer panic behavior remains
caller policy. Mondrian's private startup consumer is disabled unless
`MONDRIAN_INTERNAL_CLAP_PARAMETER_SAME_CALL` is exactly `1`; it preserves the
original queries, first refusal and structure comparison and appends a bounded
error-only suffix. It does not use Host Log ingress or establish third-party,
processing, performance, platform or hardware qualification.

## MSRV and borrowed owner proof

The Common checked nonnegative i32-to-u32 projection uses native-endian byte
roundtrip const APIs available at MSRV1.85; its original negative-value guard
remains. The unchanged usize target-width assertion is evaluated in inline
const. Neither change alters an ABI carrier, exported constant or value range.

`HostStorageRef::from_box` intentionally accepts `&Box<T>` to prove a complete
Box allocation. A generic `&T` would also admit stack or Arc payload references
under that label. Its function-local `clippy::borrowed_box` expectation documents
this type proof while preserving the signature, body and distinct ArcPayload
constructor. This does not relax a lint group or establish full allocator closure.

The new source/documentation deltas inherit the unchanged upstream input and
historical formatting/note-port receipts. Those sealed receipts retain their
original endpoints; they do not claim to have formatted or qualified this later
revision. PROVENANCE.json records the new actual shipped hashes and byte counts;
its own hash is supplied independently by collection/validation.

## Explicit raw Note Ports operation

Mondrian registers the additive HostNotePortsRaw table only for explicit Note
operations, under the one official note-ports identifier. Its callbacks observe
immutable Shared state before checking thread and lifecycle facts; no main-state
borrow or lazy initialization occurs on a rejected foreign thread. Raw rescan
bits and sticky failure facts survive without truncation. The existing typed
legacy API remains, with its native implementation table pointer corrected to
one reference as required by RawExtensionImplementation::new.

The first actual new native regression crashed with 0xC0000005; isolated Notes
controls reproduced it while the metadata-only control passed. A double reference
had exposed a pointer slot as the official function table. Both table registrations
now pass the table itself. Original failures remain in the sprint evidence; this
source change alone does not certify successful native processing or the complete
host. Product Note Score execution remains gated pending whole-root preparation,
consumer and DSP verification. No third-party, realtime or platform claim follows.
