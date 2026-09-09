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

The identity is complete by construction rather than a caller-filled digest
bag. `mondrian-core` carries opaque resolved-visual value types; Renderer binds
one exhaustive node materialization to exact Prepared Visual time, authored and
execution rasters, working space, color-engine identity, and a render-semantics
epoch. The Preview source Adapter canonicalizes every resolved element,
including transforms, opacity/blend, compiled Effect fingerprint and required
frame seed, generated-source identity, recursive nested-frame identity, and
every exact media source revision/sample/interpretation.

The media projection deliberately normalizes `NativeCpu`, compact CPU YUV, and
native decoder surfaces to the same Full semantic representation. Reduced and
proxy rasters remain distinct. Decoder/provider/backend choice, selected GPU
handle, queue generation, and diagnostic execution path cannot rotate a
persistent content identity when the promised pixels are the same. Conversely,
source revision, source sample, picture geometry, alpha interpretation, input
color/RAW development, working space, Effect graph, transform, or raster
quality names a different identity.

Interlaced execution does not cache a woven delivery picture. Each field is an
ordinary complete working-linear progressive sample whose exact doubled-grid
`FramePosition`, source field-processing contract, dominance, and picture scan
participate in the resolved identity. Prepared visual author fingerprint v4
includes Sequence field order. The later field prefilter/weave and encoded scan
tags are delivery work and remain outside this pre-Program-Output Store.

`mondrian-render-cache` accepts only that opaque resolved identity plus the
physical artifact envelope: exact output extent, lossless RGBA32F format, and
straight-coverage alpha. There is no public partial builder or raw-digest
constructor. Program Output, authored Preview codec/format, monitor/ICC,
Scopes, signal warnings, scheduling, and author revision are excluded because
they do not shape the cached pre-Program-Output working pixels. Viewer output
identity remains a separate presentation key.

Only a cross-call-reusable resolved plan with
`SequencePreviewSettings::cache_enabled` may receive an identity. Stateful,
external, or otherwise uncacheable Effects bypass lookup and publication.
Immutable content addressing provides local invalidation: authoring, relink,
color, geometry, quality or format changes name a different artifact instead
of clearing unrelated cache entries.

## Artifact and Store

Artifact schema version 1 stores independent little-endian RGBA32F frames compressed losslessly
with Zstandard. The header binds the complete key, dimensions, decoded and
compressed lengths, format/alpha contracts, and SHA-256 of decoded bytes.
Reads enforce both compressed and decoded byte limits before accepting pixels;
truncation, checksum mismatch, identity mismatch and decompression overflow
fail closed. Publication and decode also verify that payload dimensions match
the extent carried by the typed identity envelope. One corrupt content address
is removed locally.

Canonical identity schema 2 lives in the independent `timeline-render-v2`
Store namespace. Old v1 artifacts are never interpreted under the new semantic
contract and age out under their former namespace without a destructive global
clear. Canonical encodings are domain-separated, length-delimited and covered
by a golden digest test; future pixel-algorithm changes must bump the render
semantics epoch or namespace.

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

The service exposes both ordinary and deadline-bounded consuming shutdown
boundaries. `shutdown_and_wait` closes both channels and synchronously joins the
sole worker only for an explicit caller that accepts an unbounded wait. Qualification first calls the
repeat-safe, non-blocking `begin_shutdown`, then consumes the owner through
`shutdown_until` with the campaign's shared absolute `Instant`. That bounded
path polls `JoinHandle::is_finished` and joins only after completion is proven;
it never enters an unbounded join. A worker still active at the deadline is
detached and the typed receipt records both timeout and detachment, so
`all_workers_terminated` fails closed. The same receipt distinguishes clean
termination, panic, invalid same-thread shutdown, timeout, and detachment.
Preview endurance closure consumes that receipt instead of inferring worker
return from an empty queue or object Drop. Ordinary `Drop` closes both channels,
joins only an already-finished handle, and otherwise detaches immediately; it
cannot freeze the UI thread or provide qualification evidence.

Preview retains the cache service's exact startup and terminal evidence inside
its own shutdown receipt. A production cache configuration is required: start
failure, missing terminal evidence, panic, timeout, same-thread skip, or detach
all make the aggregate Preview closure dirty. Unit-test Preview compositions
may explicitly mark the cache as not required, but production cannot infer
`NeverStarted` from a failed startup.

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

Preview prepares its cache Adapter before creating native workers. Production
still requires attempting the cache; a deliberately disabled test cache is a
different state from required-but-unattempted or failed construction. The actual
returned service is installed before subsequent diagnostics/construction can
unwind. Its owner-free native shutdown facts are serializable without replacing
the consuming receipt with a projected success flag. Partial Preview closure
binds these facts to exact startup inventory; normal Preview qualification still
requires the existing required-cache closure predicate.


### Preview closure inventory replay

The cache worker shutdown receipt supports typed deserialization for independent
owner replay. Preview requires a configured cache to carry an actually started,
synchronously terminated worker and matching `Terminated` aggregate evidence.
A disabled test cache is valid only with no worker, no startup failure and a
`NotStarted` aggregate; a required cache cannot borrow that empty inventory.
