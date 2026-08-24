# Timeline Render Cache

`mondrian-render-cache` is the persistent, content-addressed cache for complete
post-Effect/post-composite Timeline frames. It sits below Preview scheduling
and above ordinary frame materialization. It is distinct from both source
proxies and the playback-owned in-memory Preview Frame Store.

## Semantic boundary

A cache artifact contains a straight-coverage, working-linear RGBA32F frame
before Program Output and monitor adaptation. A hit therefore re-enters the
same renderer Viewer output stages as an ordinary composite. It cannot encode
an sRGB UI raster, display profile, stale presentation decision, Playback
epoch, GPU texture owner, decoder lease, or Export quality substitution.

The versioned SHA-256 identity binds:

- the deterministic recursive Prepared Visual author closure;
- the resolved Viewer program/Effect graph identity;
- every resolved media revision and exact source sample;
- the working and Program Output color contract;
- the exact root frame, materialization extent and resolved Preview quality;
- the authored Preview format discriminator plus physical cache format and
  alpha interpretation.

Only a cross-call-reusable resolved plan with
`SequencePreviewSettings::cache_enabled` may receive an identity. Stateful,
external, or otherwise uncacheable Effects bypass lookup and publication.
Immutable content addressing provides local invalidation: authoring, relink,
color, geometry, quality or format changes name a different artifact instead
of clearing unrelated cache entries.

## Artifact and Store

Version 1 stores independent little-endian RGBA32F frames compressed losslessly
with Zstandard. The header binds the complete key, dimensions, decoded and
compressed lengths, format/alpha contracts, and SHA-256 of decoded bytes.
Reads enforce both compressed and decoded byte limits before accepting pixels;
truncation, checksum mismatch, identity mismatch and decompression overflow
fail closed. One corrupt content address is removed locally.

Complete bytes are published through `mondrian-storage`'s sibling temporary
file and typed durable atomic-publication boundary. The Store uses two-level
digest fan-out, scans only its versioned owned namespace, and enforces an LRU
compressed-byte budget. It never writes into `.mdp`.

## Concurrency and Preview Adapter

One dedicated worker owns scanning, filesystem I/O, compression,
decompression, validation and disk eviction. Its command and result queues are
independently bounded. Lookup/publication admission is non-blocking and
deduplicated by exact identity; Busy, Miss or cache failure always falls back
to ordinary production rendering.

The App Adapter retains at most one verified working hit for immediate
promotion and a bounded negative-identity set to prevent UI poll storms. A hit
is uploaded as one identity working layer, then follows the ordinary Viewer
Program Output and monitor-adaptation path. Paused CPU correctness rendering
and the bounded GPU-failure CPU worker can publish complete working results;
they never perform compression or disk I/O themselves.

GPU playback does not read every completed frame back merely to populate the
cache: that would add a full-frame device-to-host copy to the realtime path.
Future explicit background Timeline rendering may populate the same Interface
under its own resource slot. Export does not consult this cache unless a future
immutable delivery policy explicitly opts in and validates compatible quality;
the default Export path always renders from its frozen production snapshot.
