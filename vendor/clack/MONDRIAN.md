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
