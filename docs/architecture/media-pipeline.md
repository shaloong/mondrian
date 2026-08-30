# Media Pipeline

Decoded RGBA payloads cross the Media/Renderer seam with an explicit
`PreviewSourceSampleIdentity`: either `ColorManaged(ColorSpace)` or
`DataTexture`. Media owns this decode evidence and emits
`DecodedRgbaEncoding::DataTexture` for the latter; it never invents a color
identity for technical channels. DataTexture admission is deliberately limited
to CPU-addressable RGB because YCbCr matrix conversion, decoder-native YUV
surfaces, compact-YUV materialization, and generated proxies cannot currently
prove numeric-channel preservation. Those representations fail closed at the
decode-contract Interface rather than silently entering color management.

`mondrian-media` owns FFmpeg-based media inspection, decode support, waveform/proxy/cache primitives, and audio buffers.

## Camera RAW Adapter

`.dng` is an admitted picture extension only after the probe proves exactly one
picture frame. The bounded TIFF/DNG parser publishes typed CFA pattern, raster,
bit depth, compression, camera identity, ColorMatrix availability, and
AsShotNeutral availability on `VideoStreamInfo`. Bayer sampling is explicit in
`PixelFormat`; a probe-admitted RAW stream has the executable source identity
`LinearRec709` rather than inheriting generic CICP interpretation.

The dedicated CPU Adapter uses FFmpeg only for TIFF/DNG packet decompression
and DNG black/white normalization. It requires a Bayer 8/16-bit output matching
the probed 2x2 CFA, then performs deterministic bilinear or edge-aware
demosaic, camera/as-authored white balance, exposure, camera-to-XYZ development,
D50-to-D65 adaptation, and scene-linear Rec.709 output. The resulting
`FloatRgbaFrame` is tagged `SourceLinearRgb` and
`InProcessCameraRawDng`; generic RGB swscale is never an admitted RAW path.
Probe scratch bytes are released before FFmpeg opens the image.

`CameraRawDecodeIntent` binds Adapter, exact fixed-point controls, and algorithm
version into the Preview key. Thumbnail and Export carry the same value and use
the same decode Session Adapter. RAW proxy selection/generation, compact CPU
YUV, and decoder-native GPU surfaces are disabled until their artifacts can
bind the complete development identity. Current execution is whole-frame CPU
Float32 and therefore remains a performance gap for high-resolution bursts;
image-sequence authoring and tiled/GPU debayer belong to later work.

## Picture scan and stored geometry

Media probing publishes one typed `PictureStreamMetadata` contract containing
exact sample aspect ratio, display scan order, exact FFmpeg field transport
(`tt`/`bb`/`tb`/`bt`), and the cardinal orientation classified
from FFmpeg display-matrix side data (with legacy rotate metadata only as a
fallback). Arbitrary matrices remain `Unsupported`; Preview and Export must
resolve these facts through `ResolvedPictureGeometry` before execution. The
display-first mapping is `tt|bt -> TFF` and `bb|tb -> BFF`; coded-first and
display-first evidence are never conflated.

`PreviewSourceFieldProcessing` is part of the physical decode key and Session
reuse predicate. Progressive input passes through. Qualified interlaced input
uses one Session-owned linked-FFmpeg BWDIF graph in `send_field` mode and emits
full-height progressive frames at exact field timestamps before scaling, color
conversion, Effects, or compositing. Automatic mode observes decoded AVFrame
flags; mixed progressive/interlaced content or changing dominance fails closed.
Seek, cancellation recovery, decoder flush, and Session replacement discard the
temporal filter graph. Unknown scan requires CPU-addressable decode evidence and
is never silently treated as progressive. Residual interlaced frames after the
field-processing boundary are rejected by materialization.

Proxy FFmpeg commands disable automatic rotation and normalize the generated
proxy's physical SAR to 1:1 before scaling. The original source metadata remains
the sole interpretation authority applied later by Preview/Export. A proxy may
change sampled extent, but never source orientation or display geometry.
Interlaced or unknown-scan sources currently bypass proxy selection and proxy
generation because the proxy artifact contract does not yet carry field-rate
deinterlace identity.

## Realtime audio output evidence

`RealtimeAudioOutput` owns the concrete CPAL stream and a fixed-capacity
`ArrayQueue<f32>` PCM queue. The device callback may pop samples, fill silence,
and update atomics only; it does not acquire the former queue mutex. Main-thread
enqueue is bounded to two seconds and admits a complete interleaved buffer only
after exact rate, semantic layout, frame shape, and whole-buffer capacity
preflight. Backpressure rejects the whole buffer; it never evicts old PCM or
shifts media time.

`RealtimeAudioOutputSnapshot` is the media-to-app Adapter evidence seam. It
reports stream generation, configured sample format, cumulative and
active-interval callback frames, callback count/age/quantum, queued frames,
underrun frames, active state, and asynchronous stream failure. These are media
facts. `mondrian-media` does not select a Clock Master or label this estimate as
an exact hardware playback head; the app lowers the snapshot to a typed playback
observation and `mondrian-playback` applies preroll, uncertainty, epoch,
monotonicity, and handoff policy.

Callback consumption is guarded by a per-stream checked quiescence revision.
An inactive callback writes silence without popping PCM or advancing the active
media-frame counter. Deactivation is acknowledged only after all active blocks
reserved under the retired revision complete; a later activation must present
that exact stream/revision token. This prevents a callback spanning a
deactivate/reactivate boundary from consuming PCM for the new generation.

The device worker owns the non-`Send` CPAL stream. On backend failure or a
validation-only exact-generation controlled recycle, it deactivates callbacks,
destroys the concrete stream, observes the final atomics/queue snapshot through
a read-only handle, and publishes `Lost(reason, final_snapshot)` in that order.
The worker then opens a distinct checked stream generation. The validation seam
is feature-gated and reachable publicly only through `AudioPlayback`; normal
builds cannot synthesize lifecycle events.

Output selection uses CPAL's cross-process stable `DeviceId`, not display name
or enumeration index. `SystemDefault` and `Specific(DeviceId)` are distinct
runtime intents. A specific identity that is absent fails closed and remains
recoverable; it is never redirected to a similarly named or default device.
The worker observes selection changes and low-frequency system-default identity
changes outside the callback, destroys the old stream, publishes typed loss
evidence, and then negotiates a fresh generation. Catalog enumeration is a
separate App Window Adapter so operating-system device discovery cannot block
the UI or callback. CPAL still supplies only portable channel counts: named
multichannel speaker layouts remain blocked until an OS Adapter proves channel
positions and order.

Realtime transport, Clock Master selection, Frame Demand deadlines, and the
interpretation of Frame Deliveries belong to the app Playback Engine described
in [Playback Engine](playback-engine.md). `mondrian-media` executes bounded
decode requests and reports facts; it does not pause or advance transport.

## Media source revision evidence

`MediaFileFingerprint` is a conservative reuse boundary for one observed
filesystem object; despite its historical name, it is not a content digest and
does not prove that two different files contain equal bytes. A complete value
combines length and modification time with the opened object's stable identity
and the filesystem-owned change generation. Equality of only path, length, or
mtime never authorizes cache, decoder Session, proxy, waveform, thumbnail,
Export, or physical-stream reuse.

The Unix Adapter records device/inode identity plus inode-change time. The
Windows Adapter obtains file ID and `ChangeTime` from one open handle and
authorizes fast cross-Session reuse only when the opened volume reports NTFS or
ReFS, whose contracts are explicitly admitted by the implementation. FAT,
exFAT, and any other unsupported or unobservable filesystem produce a partial
value. Partial evidence may remain useful for diagnostics, but
`MediaFileFingerprint::authorizes_reuse()` is false and every reuse or
stream-binding boundary fails closed instead of falling back to path/size/mtime.

## Bounded audio source windows

Playback and Export do not decode complete audio sources into resident memory.
`AudioSourceCache` opens fingerprinted source readers and supplies exact
interleaved PCM through aligned ten-second windows. One weighted LRU spans all
readers at the prepared sample-rate/native-layout contract. Entry capacity and
PCM byte budget are runtime values, not hard-coded product assumptions; online
reconfiguration immediately trims the ordinary LRU and all later admission
uses the new limits. In-flight decode leaders are not canceled to reclaim
residency, but their results observe the latest limits before publication.
Terminal failures retain their independent 64-entry bound. Physical identity
includes the path and complete source fingerprint described above; length and
modification time are retained evidence but are never sufficient reuse
authority. Cache diagnostics expose effective limits, bytes, entry pressure,
online trim counts/bytes, hits/misses, decode results, oversize windows,
single-flight leaders, and evictions; an entry-count-only claim is insufficient.

The concrete miss Adapter owns an online-reconfigurable bounded pool of
persistent FFmpeg child-process Sessions. A Session is keyed by the complete source
fingerprint, absolute selected stream/native layout, and output sample-rate
contract, opens at the first requested sample, and
continuously emits interleaved `f32le`. Consecutive
windows reuse that stream; a non-contiguous miss terminates and reopens only
that source Session using at most ten seconds of input-side coarse preroll plus
output-side exact trim. This preserves the sample coordinates of a sequential
decode instead of trusting codec-dependent input-seek priming. Pool pressure
evicts an idle least-recently-used Session. Reducing capacity terminates idle
LRU sessions immediately; busy sessions retain their current decode and
converge after releasing the slot. Effective capacity, trim count, and temporary
over-capacity residency are diagnostic facts. This is an intentionally isolated
process Adapter, not an in-process FFmpeg claim; a linked FFmpeg Adapter may
replace it behind the same Interface without changing cache or sample semantics.

The cache, reader, and every returned `AudioBuffer` carry the selected stream's
validated native `AudioChannelLayout`, not an independent channel count. Mono, canonical named
speaker sets, and bounded Discrete buses are distinct signal facts; 5.1(side),
5.1(back), and 7.1 therefore cannot alias by extent. The Adapter selects
`-map 0:<absolute stream index>` and installs only an explicit ordinal identity
`pan=<N>c|c0=c0...` matrix before setting the raw output count. Component
standard/explicit conversion executes later in `mondrian-audio` and therefore
cannot fragment or corrupt the native PCM cache. Cache validation rejects a decoded buffer
whose layout differs even when its raw sample extent happens to match. The
selection, including its authorizing source fingerprint and exact native
layout, participates in window single-flight and persistent Session identity,
so two distinct physical selections cannot alias the same PCM entry and a file
replacement cannot reuse an old selection.
At the product execution Seam the complete binding is
`AssetId + AudioSourceSelection`: `AudioSourceSelection` retains the complete
file revision, absolute physical container stream index, and native layout,
while the owning App resolver supplies `AssetId` and the current canonical
path. Media adds output sample rate and window coordinates to its private cache
identity. Neither a stream index without its file revision nor a file revision
without its Asset binding is a valid product selection.

Audio probing reads FFmpeg's declared channel layout rather than deriving it
from channel count. The persisted probe value has only three states:
`Exact(AudioChannelLayout)`, `Unspecified(count)`, and `Unsupported(count)`;
standard layouts are no longer duplicated in a second enum. Native FFmpeg
speaker masks whose every position is modeled project to one canonical named
speaker set, including custom combinations such as 6.0. `5.1(side)`,
`5.1(back)`, and 7.1 therefore remain distinct; bounded unspecified layouts
project to Discrete values without inventing speakers, while custom-order or
unmodeled positions remain explicit unsupported probe facts. The standard
execution mapping policy admits declared mono/stereo/5.1(side)
and explicit discrete defaults for otherwise-unlabelled one- or two-channel
sources; it never promotes an ambiguous 3+ channel count. Each probed audio
stream also retains its absolute stream index, optional container stream ID,
language/title metadata, and default disposition. These are selection evidence
for the Asset Component Catalog, not permission to auto-retarget an authored
Component when a relinked file differs.

`mondrian-assets` owns the persistent Component Catalog. Library schema v5
transactionally rewrites v4 standard layout variants into the sole exact
signal representation and `Other` into `Unsupported`, touching only typed
layout fields. Import assigns stable
IDs, persists conservative stream signatures plus the probe source fingerprint,
and SQLite schema v3 migrates existing records transactionally while revoking
unproven legacy fingerprints.
Relink preserves old identities, discovers only genuinely unclaimed stream
indices, and leaves same-index signature drift unresolved. Playback resolves
the live catalog, waveform explicitly requests `primary`, and Export snapshots
freeze only the reachable bindings; media receives only a validated
`AudioSourceSelection` and never owns author identity.

Asset Library organization is also a deep SQLite mutation Interface. Moving
any mixture of visible Asset records and folders to one bin deduplicates the
complete request, validates every strong identity and the complete folder graph,
and applies only real membership/parent changes inside one transaction. Single
item movement reuses the same Implementation. Missing or retired Assets,
missing folders, self/descendant cycles, a changed preflight row, or a storage
failure roll back the entire request; UI-side loops are not publication
authority.

Stdout has two bounded 64 KiB look-ahead chunks and stderr retains only its
latest 64 KiB while always draining the pipe. Generation cancellation is
polled every 5 ms while waiting for output, then kills, waits, and joins the
child and both pump threads. Partial EOF is accepted only on a complete
interleaved frame boundary. The cache additionally provides single-flight per
complete source-window key, so concurrent consumers share one decode result
instead of serially reopening the Session. None of this work runs in the CPAL
callback or UI thread. The whole-file FFmpeg helper is compiled only for parity
tests; no product Preview, waveform, Playback, or Export path may call it.

The block contract now carries the generation-owned
`ExecutionCancellationToken` all the way into `AudioWindowDecoder`. Cancellation
is checked before lookup, across concrete decode, and before cache admission.
Canceled results are neither decoded-window entries nor terminal failures, so a
seek cannot poison the same source coordinate for its successor generation.
Session diagnostics separate cold opens, sequential reuse, and random-seek
restarts and expose resident/peak/capacity, evictions, cancellations, and each
class's worst wall duration. Acceptance may constrain steady sequential
latency without falsely relabeling cold-open or random-seek cost.

The deterministic cache unit gate proves cancellation-token propagation,
canceled-result semantics, and zero success/failure admission. It does not
apply a wall-clock SLA to a synthetic worker because caller-to-join time also
contains unbounded host scheduler delay. The manual real-child qualification
gate remains the sole owner of the 50 ms FFmpeg kill/wait/join requirement.

## Waveform analysis

`mondrian-media::WaveformEnvelopeBuilder` is a streaming, partition-invariant
PCM-to-peak Implementation. It accepts exact source-frame coordinates and
interleaved chunks, validates channel/frame boundaries and declared source
extent, caps retained output at 4096 columns, and never owns asset identity,
threads, caches, or UI state. Peak assignment is computed against the complete
source span, so changing FFmpeg window boundaries cannot change the envelope.

`app::waveform_service::AudioWaveformService` owns the product execution
lifecycle. It resolves the primary audio stream and finite duration from the
bound asset library. Its `WaveformSourceKey` is the exact comparable pair
`AssetId + AudioSourceSelection`, so it includes the complete file revision,
absolute physical stream index, and native layout without compressing them into
a UI-generated `u64`. It admits at most 512 demands and feeds a dedicated
16-job worker transport without paint-time retry storms; rotates a monotonic
generation on project-library changes; cooperatively checks cancellation at no
more than 4096 decoded mono samples; and decodes through a private
`AudioSourceCache`. This cache is deliberately separate from realtime Playback
and Export budgets. The coordinator's Waveform residency grant is partitioned
explicitly: three quarters for completed envelopes and one quarter for decoded
PCM windows, with at most four PCM entries and one persistent decoder Session.
Pressure can reconfigure and trim both partitions online; changing either
partition never changes envelope math or selected media identity. The service
retains bounded source LRU, failure memory, and terminal evidence and exposes
both effective budgets plus PCM/session trim facts for Headless tests.

Timeline paint receives only `AudioWaveformSource`, a shallow nonblocking
lookup Adapter. A cache miss may request work and returns `None`; no Widget,
layout pass, or event callback opens FFmpeg, blocks for PCM, owns a worker, or
reconstructs generation/failure policy. The presentation-width resampler uses
max aggregation while reducing resolution so a narrow transient cannot vanish.

## Asset thumbnails

`app::thumbnail_service::AssetThumbnailService` is the product composition and
execution boundary for asset thumbnails. It derives one exact request identity
as a full typed `ThumbnailRequestKey`: `AssetId`, source path, complete live
file fingerprint, and a `ThumbnailColorContract` containing resolved source
video-stream index, color/range, working space, output space, tone-map, engine,
and output-transform intent. Process-global OCIO generation is deliberately
absent: changed Custom bytes must resolve a different complete engine identity,
while the service's own generation controls publication. Ordinary map hashing
is only bucket selection; neither a short hash nor a UI resource label
authorizes execution reuse. A relink,
same-path replacement, interpretation override, or sequence/project color
change therefore cannot reuse an unrelated raster or retained failure.

The service owns a 512-demand admission bound, a 16-job deterministic-still
transport, generation cancellation, publication ownership, a 512-entry/128 MiB
weighted raster LRU, bounded failure memory, and bounded terminal evidence.
Completion pumping is constrained by both result count and elapsed time per
event-loop turn. Canceled and superseded work cannot enter the success or
failure cache. A worker decodes through `PreviewDecodeRequest` with
`RandomAccessStillFrame`, observes the shared cancellation token, executes the
canonical source-to-working and working-to-sRGB output boundaries, and returns
only a validated UI-independent `ThumbnailRasterFrame`.

`app_ui::asset_thumbnails::AssetThumbnailAdapter` is a shallow Window Adapter.
It maps a resident raster into `RasterImage` while sharing the same `Arc<[u8]>`;
it owns no worker, queue, cache, color interpretation, generation, retry, or
failure policy. Headless verification consumes the service's immutable
diagnostics and terminal records without importing Widget types.

## Visual Mask tracking decode

`app::visual_tracking::VisualTrackingService` is an instance-owned, single-worker
analysis domain with a four-job transport and eight-result exact LRU. It is
separate from realtime Preview scheduling so a long analysis cannot occupy the
Playback lane, while each job still uses media's reusable
`PreviewDecodeSessionContext` and `RandomAccessStillFrame` contract. Admission
freezes the complete file fingerprint, physical video stream, resolved source
color/range contract, exact Clip retime mapping and every requested source
sample. The worker scales decode to the persisted analysis dimension, retains
only adjacent luminance frames, and checks cooperative cancellation through
decode, feature search and model fitting.

Requests are capped at 10,000 frames and their analysis raster, feature count,
search radius, patch radius and combined correlation work are all hard-bounded.
Cache identity includes source revision/stream, exact source-sample vector,
anchor, initial geometry, model/direction and every setting; Recompute bypasses
reuse. Project replacement cancels and retires all attempts. Completion returns
to the UI thread and may publish only after revalidating Authoring Session,
Sequence revision, Clip/Mask/locks, Asset and live file fingerprint. Cancel wins
over an already queued completion event, and stale/failure paths publish no
partial author state.

## Probe

`mondrian-core::MediaProbeSnapshot` is the only stable, serializable media-probe
contract. `mondrian-media::probe_media_info(path)` is the concrete synchronous
FFmpeg Adapter that produces it without decoding full media. The stable type has
no inherent probing method, path field, fingerprint field, or FFmpeg dependency.
The App/Asset candidate Seam owns those source-identity facts. The Adapter
extracts:

- container and duration
- file size
- video streams: codec, decoder-proven codec profile, stream-local declared
  duration, dimensions, frame rate,
  pixel format, bit depth, alpha, detected color space, structured color
  interpretation, frame count, and HDR side-data summaries
- audio streams: codec, stream-local declared duration, sample rate, channels,
  layout, bit depth

The probe runs off the UI thread. Once stream/CICP evidence identifies a video
as HDR (or stream metadata declares dynamic HDR/Dolby Vision), the probe opens a
short-lived decoder and reads only the first decoded frame, with a hard limit of
512 target-video packets. This is required because FFmpeg exposes HEVC ST 2086,
MaxCLL/MaxFALL, and HDR10+ metadata as `AVFrameSideData` for common files even
when `AVStream`/`codecpar` side data is empty. The frame facts are merged with
the stream facts by semantic kind; a typed frame payload replaces an unparsed
stream summary but never creates duplicates. SDR and audio-only imports do not
open this metadata decoder. Failure to obtain the optional first-frame evidence
is logged without making otherwise decodable media offline.

Probe absence is explicit. Invalid/zero FFmpeg frame-rate rationals are stored
as unproven rather than silently replaced with 25 fps. Unsupported or unknown
pixel formats retain an unproven marker rather than becoming YUV420P/8-bit.
`VideoCodecProfile::Unknown` is not equivalent to a profile inferred from codec,
bit depth, filename, or extension. A professional Main10 gate requires the
opened decoder context to report `HevcMain10`; persisted records written before
these proof fields default to unproven and must be re-probed before acceptance.
The same rule applies to H.264. Baseline, constrained baseline, Main, Extended,
High, High10, 4:2:2, 4:4:4, predictive and Intra variants are retained as typed
`VideoCodecProfile` values from the opened decoder context. A known H.264 High
stream must not collapse to `Other`, because reimport and proxy/original
comparison would otherwise discard evidence the decoder already proved.
Likewise, a container duration cannot prove that its primary audio or video
stream spans the same interval. Professional acceptance requires a positive
stream-local duration for the relevant primary stream and rejects missing or
shorter evidence. Audio cannot count post-EOF silence as source coverage, and
video cannot count a longer container or unrelated stream as playable frames.

Asset registration is separate from metadata probing. `AssetLibrary` cannot
call FFmpeg and has no dependency on `mondrian-media`. The App media-import
Module supervises the packaged product executable in an exact, versioned
one-shot Isolated Media Probe mode. The Helper canonicalizes the native path,
captures a complete fingerprint, calls the bounded FFmpeg probe, verifies the
same source revision again, and returns one typed snapshot. Parent-side
cancellation or the 120-second monotonic deadline kills and reaps that Helper;
stdout has a strict 8 MiB limit and stderr retains only a 64 KiB diagnostic
tail. The App worker never owns in-process FFmpeg probe state. It rechecks
cancellation and Project generation, then constructs
`AssetMediaProbeCandidate`. `commit_media_probe(candidate, folder)` revalidates
the source fingerprint and
publishes source facts, stable audio bindings, and target-folder placement as
one SQLite transaction. This preserves one deep Asset mutation Interface while
keeping execution and authoring dependency direction correct.

Product media import is an app-level background batch, not a synchronous UI
action. `ImportMedia` and asset-panel import actions validate only cheap
preconditions on the event thread (library availability, target folder
existence), admit the request into the instance-owned bounded Media Import
execution Module, and return immediately. Its fixed worker lanes own
generation, cancellation, dispatch policy, and result transport; the separate
physical-media Adapter supervises the Isolated Media Probe and only then
crosses the Asset Library candidate Seam. `AppState` polls import completions
during the normal background-task tick, publishes
`AssetImported`, applies proxy policy, updates status, and saves the project
once per completed batch. UI panels must not call `probe_media_info` or commit
Asset candidates directly from action handling, drag/drop, or paint/layout
code.

Prepared import results cross an additional publication gate before SQLite
mutation. That gate serializes the irreversible commit with Project-generation
rebinding and user cancellation, so an intent that loses authority before the
gate cannot publish. The execution-state lock is released for the complete
SQLite transaction: slow storage therefore cannot freeze worker admission,
resource-policy updates, or diagnostics. Once a commit owns the gate it is the
terminal publication phase; Project replacement and cancellation wait for that
phase to resolve instead of reporting a false cancellation after durable Asset
state was already written.

Existing file-Asset mutations use a separate narrow
`MediaAssetMutationExecution` Module rather than reopening synchronous paths in
action handling. Relink, audio Component refresh, and explicit Component
rebind lower to one typed operation, enter a fixed-capacity ordered queue, and
prepare the same immutable `AssetMediaProbeCandidate` on a single worker. They
reuse the same physical Isolated Media Probe Adapter as Import, but keep their
own ordering, generation, publication gate, and terminal evidence.
Single-worker ordering is intentional, and the Module admits at most one
uncommitted operation for a given Asset: a later intent therefore cannot bind
the old source path while an earlier relink is still pending. Product resource
policy may pause queued probes during realtime playback without canceling a
running probe or discarding the bounded author intent. The Module publishes two
non-interchangeable revision facts in one coherent diagnostic snapshot:
`operation_revision` advances for admission, execution-phase, Project-generation,
publication, and terminal changes, while `policy_revision` advances only when
the effective dispatch policy changes. The event-loop product-change poll
observes only `operation_revision`; applying a resource decision can therefore
never masquerade as an Asset/product-model mutation or cause a full UI-model
refresh. Policy changes remain directly inspectable, and any queued work they
release becomes observable when the worker performs a real operation-state
transition. The event-loop completion poll is the sole commit authority and
admits a candidate only when its Project
generation and cancellation token are still current. It then calls exactly one
of `commit_relink_probe` or `commit_audio_component_probe`; a Project close,
reopen, or replacement revokes all queued and in-flight commit authority.
Terminal evidence is bounded and distinguishes completed, failed, canceled,
and superseded attempts. No action handler, Asset panel, or `AssetLibrary`
method probes media synchronously.

## Decode and Cache

Decoding and frame caching belong to media/renderer/export paths, not UI widgets. UI panels may request thumbnails or waveform data through app adapters, but must not own FFmpeg state. Waveform and thumbnail execution are owned by the UI-independent App services described above; the media crate owns only its decode and streaming-analysis primitives.

Media frame caches must use a security-supported `lru` dependency. Dependency
upgrades must preserve the documented capacity bounds, eviction order, cache-key
identity, and thread ownership; workspace tests and `cargo deny` jointly guard
that contract.

The media decode layer exposes three access contracts, matching the way mature
NLEs separate playback, interactive navigation, and precise still extraction:

- `PreviewDecodeAccessMode::PlaybackCursor` is for sustained timeline playback
  and forward prefetch. It is mostly-forward, should keep decoder/session
  locality, and is the seam where hardware decode, low-copy P010/NV12
  residency, deadline/drop policy, and GPU input transforms belong. Its output
  must prove that its decoded presentation interval contains the requested
  source timestamp: a non-covering or unproven decoder selection is a typed
  temporal failure and cannot enter the Frame Store or be presented as a
  degraded current frame.
- `PreviewDecodeAccessMode::ScrubCursor` is for latest-wins playhead dragging,
  jog, and shuttle. With a probe-backed index it seeks toward the nearest
  keyframe and presents the first valid decoded frame inside the adjacent-GOP
  evidence radius. That temporal approximation is Degraded; the settled request
  then resolves the exact frame. It prioritizes cancellation and visible
  feedback over warming a long forward queue.
- `PreviewDecodeAccessMode::RandomAccessStillFrame` is for deterministic still
  extraction: thumbnails, poster frames, export fallback, diagnostics, and exact
  one-off requests. Like Playback, a non-covering or unproven selected
  presentation interval fails closed; temporal approximation belongs only to
  an explicitly active Scrub request.
  The App thumbnail service passes the already-probed
  `MediaFileFingerprint` into the still-frame request and uses the same
  fingerprint for raster and failure invalidation, so replaced files cannot
  reuse stale output. Decoded RGBA remains source-encoded: the service resolves
  asset input color through the active sequence/project policy, executes the
  renderer source-to-working CPU reference stage, and crosses an explicit
  working-to-sRGB display boundary before publishing its validated raster.

Every request also carries a required `PreviewSourceColorContract`: the
app-resolved input/source color space, an authority-aware
`DecodedVideoRangeContract`, and an optional explicit missing-matrix policy.
The Sequence `AssumeRec709` policy binds `Bt709` into this immutable contract
and therefore into decode/cache identity; it never relies on an implicit
swscale default. In Auto mode CPU/native decode prefers each YUV
frame's explicit range and falls back to the stream probe only when the frame
omits it; Full/Limited user overrides remain authoritative. Matrix is resolved
independently: an explicit decoded matrix controls YCbCr-to-RGB sampling even
when it differs from the resolved RGB source color space. Decode still rejects
unknown facts and unsupported matrices such as BT.2020 constant luminance.
Before `sws_scale`, media configures `sws_setColorspaceDetails` with the exact
matrix, input range, and full-range RGBA output. FFmpeg/swscale defaults are not
part of Mondrian's color contract.

CPU materialization selects its output precision from the actual decoded-frame
pixel descriptor, not a short surface-format allow-list. Eight-bit encoded
sources use RGBA8. Encoded 10/12/16-bit YUV or RGB sources scale to RGBA64LE and
are normalized into a typed source-encoded f32 payload before OCIO, so software
fallback cannot silently quantize a high-bit source. Scene-linear planar-f32
remains a distinct direct float path. `DecodedVideoSampling.bit_depth` is also
derived from FFmpeg component descriptors; explicit native NV12/P010 surface
contracts remain the fallback authority for opaque hardware formats.

These contracts are media-layer interfaces. The current in-process adapter can
share the same CPU RGBA FFmpeg implementation while diagnostics and app
scheduling distinguish the requested access mode. Future hardware-resident
decode must specialize behind these contracts instead of adding app-layer flags
or treating playback as repeated random-access still decode. The generic
access-mode router is intentionally media-internal. Public callers enter through
one request seam: `PreviewDecodeRequest`. App preview owns worker lanes,
priority admission, current/prefetch cancellation, queue diagnostics, and
timeout reporting around that media request. Do not add mode-specific public
helpers or a second preview decode pool; they become compatibility debt and
split future hardware/low-copy routing across shallow wrappers.

The same typed request travels unchanged through the public facade and the
worker-family Session router. After the worker revalidates the file fingerprint
and resolves the concrete backend, it derives one private immutable Session
open contract. Session-reuse matching and replacement Session construction
consume that same value, so stream, geometry, hardware-device, and source-color
identity cannot drift between parallel positional argument lists.

Every successful result carries one explicit
`PreviewDecodeSessionDisposition`: `Opened`, `Replaced`, `Reused`, or
`BypassedCache`. `Unspecified` is reserved for producers that cannot attest a
Session lifecycle and is invalid performance evidence. A playback-ring hit is
`BypassedCache`, never a reused or newly opened Session; conversely, a reused
Session may still perform an exact/GOP seek and therefore is not automatically
steady-state work. App diagnostics derive the non-overlapping execution class
(`CacheHit`, `SessionOpened`, `SessionReplaced`, `ForwardSteady`,
`ReusedSeek`, `ReusedOther`, or `Unclassified`) from this disposition plus the
seek/forward facts. Media owns the facts; it does not assign product latency
budgets.

Cancellation returns a typed `PreviewDecodeCancellation`, never a unit success
or an error-string classification. Its checkpoint names the first observation
inside input open, stream-info discovery, cache lookup, seek, packet read,
codec work, frame materialization, or the optional external process; its source
distinguishes a normal cooperative checkpoint, `AVIOInterruptCB`, and
parent-enforced `IsolatedDemuxTermination`. The
request probe is installed before `avformat_open_input`, remains attached to the
owned format context through stream discovery, seek, and packet I/O, and is
reset for every reused Session request. The interrupt callback records only the
first active checkpoint with atomics and never owns a cancellation cause or
deadline. The App Frame Work Broker remains authority for why and when work was
canceled; the media fact proves where blocking execution actually yielded.

`mondrian-media::preview` is the public request/outcome facade, not the owner of
every implementation detail. Its private deep modules separately own the
request-scoped interrupt protocol, typed CPU frames, one FFmpeg decode Session,
the recoverable demux-process boundary,
hardware admission/context state, frame materialization, native-frame resource
lifetime, the probe/session seek index, the byte-bounded session-local playback
output ring, the decoded-surface temporal window, and the optional external
still process. The FFmpeg Session
is the sole owner of open → stream-info → seek → packet/codec → materialize
ordering; the other modules provide narrow stateful services and cannot publish
a second decode outcome. The external process module always drains both pipes,
retains only the exact expected RGBA byte count and 64 KiB of stderr, and on
cancellation performs kill → wait → reader join before returning `Canceled`.
That lifecycle is implemented by the crate-level `process_supervisor`, not by a
Preview-only waiter. Every FFmpeg/FFprobe CLI Adapter that uses this seam starts
bounded stdout and stderr drains immediately after spawn, before any stdin
payload is submitted. Stdout may use a strict retained-head limit when its
payload is semantic input; diagnostics use a bounded retained tail while still
draining the entire pipe. A dedicated stdin pump accepts owned reusable
buffers, writes them in bounded chunks, and observes the same cancellation and
absolute monotonic deadline between chunks. The parent also polls while an OS
pipe write is blocked. Cancellation, deadline expiry, output-limit violation,
pipe failure, and spawn/wait failure therefore remain distinct typed terminal
stages. Every abnormal path performs kill → wait and joins all pipe/pump
threads; dropping an unfinished supervised child has the same fail-safe
ownership rule.

Reusable Preview execution resources have an explicit worker-family owner:
`PreviewDecodeWorkerResources`. A scheduler injects that owner through
`PreviewDecodeSessionContextBootstrap`; no process-global seek-index or
hardware-device resource cache participates in Session setup. The bootstrap
deliberately does not implement general `Clone`; the explicit
`clone_for_sequential_recovery` operation is valid only after the previous
worker-local context has stopped executing, so one observer cannot acquire
concurrent stage writers. Its seek-index
cache is keyed by exact path, complete authorizing file fingerprint, and
absolute physical stream index. One online policy simultaneously bounds entry
count, aggregate keyframe anchors, and approximate retained bytes; trimming is
LRU and cannot invalidate index data already cloned into an active Session.
Preview, Thumbnail, and Export may use the same resource type but receive
independent owners and budgets unless their scheduling coordinator explicitly
places workers in one family.

The hardware-device pool shares only immutable FFmpeg device roots for an exact
backend/adapter key. Every created root has a monotonic pool-local generation;
codec-open, attach, or active hardware execution failure retires that
generation from future acquisition. Existing Sessions keep independent
`Arc` leases and therefore remain safe until their ordinary retirement, while
new Sessions create a later generation. Codec contexts, DPB state, frame pools,
and surfaces are never pooled here. Idle roots are bounded by policy and can be
released online on a zero-idle policy or explicitly at a worker-family
no-activity/pressure boundary. Static codec/backend configuration may be
probed without creating a device. Concrete device availability is proven only
by an acquisition from this worker-family owner; it is never promoted into a
process-wide success/failure memo.

Session destruction has one non-negotiable native-resource order. Every
session-local retained `AVFrame`, decoded-surface window, playback-ring entry,
and native output surface is released before its `AVCodecContext`, hardware
frames/device contexts, or device-root lease. `PreviewDecodeSession` encodes
this in field ownership and declaration order so ordinary replacement, error
unwinding, cancellation, and explicit worker-family retirement cannot diverge.
A caller may request Session retirement only after externally published
native-output leases are released; the Session itself additionally guarantees
the correct order for its private DPB-adjacent frame residency.

Playback prefetch and visible-current work intentionally overlap, but they
share one position-owning decoder Session. Prefetch may decode past a current
request that is still queued; discarding those intermediate surfaces would
force the later current request to seek backward through the same long GOP.
The Session therefore retains a FIFO decoded-surface window bounded by both
eight entries and 256 MiB, aligned with the bounded startup-preroll prefix. A
request covered by that raw window materializes directly without seek. The raw
window survives output-only Preview scale/geometry rebinding, but is cleared on
seek, cancellation recovery, source/session replacement, and destruction. It
does not replace the App-owned output Frame Store or the compact materialized
playback ring; each owner remains independently byte bounded.

`preview::demux_process`, `demux_protocol`, and `demux_worker` form one deep
compressed-packet Source Seam. The packaged `mondrian` executable dispatches a
hidden worker mode before constructing UI state; no second release binary or
`PATH` lookup exists. Parent and child exchange the OS-native path through a
private stdin pipe, not process arguments. The response validates a launch
nonce, protocol/build identity, pointer width/endian, and the actual
`avcodec`/`avformat`/`avutil` runtime versions. Codec descriptor name must agree
with numeric `AVCodecID`; extradata, packet payloads, side-data count/entry/total,
keyframe anchors, errors, stderr, and the single-response queue are independently
bounded.
Packets are rebuilt only through checked FFmpeg allocation and never carry
`buf`, `opaque`, `opaque_ref`, or `AV_PKT_FLAG_TRUSTED` across the process seam.
The helper calls `av_read_frame` directly so EOF, EAGAIN, and fatal demux errors
cannot collapse into ffmpeg-next's retrying iterator behavior.

Production Playback, Scrub, and `RandomAccessStillFrame` all use one reusable
helper per decode Session. Open carries no timeline target. The parent lowers
the exact source time once to a checked PTS seek window, sends one non-zero
monotonic command ID, waits for the matching Seek completion, and only then
flushes its codec. Read is pull-based and returns at most one validated video
packet or EOF per command; EOF does not close the Session, so a later seek is
legal. Close has a bounded graceful interval and falls back to kill/wait/join.
The parent retains `AVCodecContext`, reorder state, D3D12VA/D3D11VA device and
frames contexts, and native output leases; there is no child-process RGBA or
GPU-surface serialization. Cancellation after a command is dispatched kills
and reaps the helper, poisons that packet source, and returns a distinct
isolated-termination fact at Seek or PacketRead. Source change, protocol/FFmpeg
failure, and cancellation reopen with a new nonce/process; a process never
switches between sources. The direct `AVFormatContext` Adapter exists only for
explicit unconfigured tests and diagnostics, not as a production fallback.
An isolated termination also makes the paired packet-source/codec Session
unrecoverable. The worker publishes `SessionRetire` and drops that complete
slot directly; it does not call the ordinary cooperative-cancellation codec
flush first, because no valid packet source can ever resume that Session and
mutating a hardware codec immediately before forced teardown adds risk without
reuse value. Cooperative and returned `AVIOInterruptCB` cancellations retain
their existing flush-and-reuse path when the Session contract still matches.
The launch request also carries the parent's conservative file revision. A
complete caller revision is revalidated by the media execution worker after
any output-lease wait and before either Session matching or input open. The
direct FFmpeg Adapter checks it again after `AVFormatContext` open, while the
isolated helper checks it before input open and after stream discovery. A race
with source replacement therefore returns typed
`MediaSourceRevisionChanged` evidence before any packet, cache entry, or
reusable Session can be published. An incomplete revision may still open a
non-file source, but cannot authorize Session or cache reuse.

Packaged worker discovery is a production admission requirement, not a hint.
An explicitly configured worker path that is missing, or a product/test layout
without the packaged `mondrian` executable, prevents media decode workers from
starting and closes their scheduler. Timeline evaluation then returns a typed
`MediaDecode` failure while non-media UI remains available. Production never
falls back to the direct `AVFormatContext` Adapter merely because discovery
failed; that Adapter remains reachable only through explicitly unconfigured
media tests and diagnostics.

Helper execution evidence is part of the existing per-worker
`PreviewDecodeExecutionObserver`, not a global process Registry or an App-owned
reconstruction. Every successful spawn creates one single-owner lifecycle
lease. The lease publishes ready only after the ordered open phases and
validated stream contract, publishes Seek/Read results only after matching
command completion, and publishes cross-request reuse only when that same
helper completes a command under a later media request sequence. Exactly one
clean-close, cancellation, failure, or forced-close terminal fact is published
after the child has been reaped; active count therefore remains nonzero when a
code path loses process ownership instead of falsely claiming cleanup. The
packaged short tests exercise reuse, all four format-call cancellation stages,
and stale-source failure. Professional Headless acceptance additionally
requires real Seek/Packet execution, cross-request reuse, bounded clean close,
zero active helpers, complete launch-to-reap accounting, and no failure or
forced-close terminal outcome.
The real-media short qualification adds a different proof: it samples a live
Main10 helper in Seek or PacketRead, supersedes that generation through the
production Broker, requires Broker and concrete media cancellation evidence,
then requires the latest frame to complete the Headless GPU Presentation
Adapter. Fast local I/O is allowed to return between observation and generation
rotation, so helper termination is not mandatory in that real run; when it is
observed, termination and checkpoint evidence must agree. The deterministic
packaged tests remain authoritative for guaranteed cancellation inside each
helper call stage. Neither short suite claims 30-minute cadence, memory plateau,
or fixed-machine performance acceptance.

Ordinary CI exercises this contract through a loopback HTTP server that accepts
FFmpeg's connection and deliberately withholds a response. Both the media
Interface test and the production Preview worker test must prove bounded return
and an exact `FfmpegIoInterrupt + InputOpen` fact. This deterministic harness is
not a substitute for fixed-reference-machine stream-info, seek, packet-I/O, or
driver-stall evidence, but it prevents the blocking-I/O seam from regressing
into a merely declared callback.

Packaged applications carry their complete non-system media/color runtime
closure. The private `ffmpeg` and `ffprobe` tools live beside Mondrian on all
platforms. Windows also places the vcpkg/`FFMPEG_DIR` DLL closure there; Linux
places the recursive closure of all three executables under `lib/` with
relative RPATH; macOS places their non-system dylibs in
`Contents/Frameworks` with rewritten install names. `mondrian-media` is the
single tool-resolution Interface: packaged adjacent tools win, while a source
development build may fall back to `PATH`. Media, Proxy, Preview, Audio, Export,
and post-encode validation must not resolve their own command independently.
Every release stage runs `--verify-runtime` in a sanitized environment. That
gate rejects PATH-only tools, missing linked baseline decoders, missing
production CLI encoders/filters/muxers, and an unredistributable `--enable-nonfree`
build; product startup must not depend on Cargo's test-only search path,
Homebrew, or a developer-specific `PATH`.
On Windows the sanitized gate retains only the staged directory and Windows
system directories in `PATH`; the vcpkg build tree cannot heal an omitted DLL.
Linux additionally resolves the recursive staged ELF graph after RPATH rewrite
and rejects every non-baseline dependency whose canonical path escapes the
package, so an installed build-host library cannot create a false pass. macOS
builds the union dependency closure of Mondrian and both tools in one
`dylibbundler` transaction, then checks the dependency edges of those three
executables and every copied Framework image; each non-system edge must name an
existing object below the App Bundle's `Contents/Frameworks`, with no residual
Homebrew or unresolved `@rpath` edge.
Linux release builds are pinned to the oldest supported Ubuntu/glibc baseline
instead of `ubuntu-latest`, because a complete private dependency closure
cannot make a binary compatible with an older glibc ABI. macOS declares its
minimum deployment target explicitly and uses a pinned Apple-Silicon image
rather than inheriting the runner SDK's default. Windows likewise pins its
MSVC/UCRT image. These are release-admission constraints, not runtime fallback
paths.
Dynamic linkage is a delivery policy, not a license bypass. The current
software Export baseline intentionally enables GPL-compatible x264/x265 in the
runtime used by this AGPL project, never FFmpeg's nonfree profile. Each package
therefore carries Mondrian's license plus the concrete FFmpeg `-L`, `-version`,
and `-buildconf` output so the distributed runtime's license and build profile
remain inspectable rather than inferred from CI configuration.
`PreviewDecodeAccessMode` intentionally has no default value, and serialized
decode diagnostics must include it. Missing access-mode evidence is a diagnostic
coverage bug, not a reason to assume still-frame semantics.
Preview Frame Store, in-flight Broker-job, and decode-request identities include
the requested access mode where execution policy needs it, exact source path,
complete file fingerprint, absolute physical video-stream index, source and
sampled dimensions, and one canonical nonnegative source-local `TimelineTime`.
`mondrian-media` exposes those physical facts through the validated
`PreviewDecodeSource` → `PreviewDecodeGeometry` → `PreviewDecodeKey` contract.
The source cannot be constructed from a relative physical path or an incomplete
filesystem revision; reusable identity never depends on the process working
directory.
`FitWithin` requires a non-empty CPU-addressable extent, while `NativeSource`
requires proven opaque sampling and a codec/profile-qualified native surface
hint. Both retain the same non-empty, aspect-preserving materialization target.
A native decoder returns its full physical surface; that source-sized decode
identity is reusable across Viewer scale changes, while the separate renderer
materialization extent remains part of the presentation execution contract.
`PreviewDecodeRequest::from_key` projects this
contract into the existing access-mode execution Interface without rebuilding
path, revision, stream, time, geometry, or source color independently. App
Preview has completed that migration: its worker can only start from
`PreviewDecodeRequest::from_key` and then attach access mode, adaptive hints,
the geometry-compatible hardware request, and device selection. Legacy
field-by-field construction remains only for media callers that have not yet
adopted the exact key, such as the independent Thumbnail/Export adapters; it is
not a second App Preview interpretation.
Decoded CPU identity additionally carries the complete input color/range,
Alpha, working-space, input-tone-map, and Project-engine contract. A bare
timeline frame number is not a media identity: the same frame index can
represent different source times under different Sequence grids, and
relink/proxy/source path changes must not reuse stale decoded frames. Production
resolution takes the physical stream from the authoritative probe and carries
it unchanged through direct demux, isolated-demux protocol v4, and the external
FFmpeg `-map` seam; FFmpeg's best-stream heuristic is not production identity.
Generated proxies are a distinct physical source. The generator maps its sole
picture output to absolute stream zero; `PreviewDecodeSource` derives NV12 from
`H264High8`, P010 from `H265Main10`, and no currently importable native hint
from either DNxHR 4:2:2 profile. App source resolution admits a fresh proxy
through its exact `ProxyArtifactManifest` and selected artifact fingerprint,
then calls `PreviewDecodeSource::from_proxy_artifact`; it never copies the
original probe's stream or sampling fields into the proxy key. A proxy must
therefore never inherit the original container's absolute stream index or
pixel-format hint merely because both represent the same Asset.
Original-source native hints intersect probe-proven pixel sampling with the
codec/profile family: H.264 High10/4:2:2/4:4:4 and software-oriented codecs do
not advertise NV12/P010 merely because a pixel format could be packed that way.
HEVC Main10, AV1, and VP9 may retain a P010 candidate; the FFmpeg device-backed
decoder open and the retained frame contract provide the later execution proof.
This hint is conservative admission only, never device capability evidence.
The manifest's source-referred color space and encoded range must also equal
the exact Clip-occurrence source-color contract before the proxy is selectable.
If author interpretation makes them incompatible, Preview records
`ProxyColorIncompatible` and decodes the original source; it never silently
interprets proxy pixels under a different color contract.
Render-plan evaluation
preserves the exact result of the Clip Time Transform; only an explicit media
frame-rate override quantizes once, with `Floor`, onto that declared source
Evaluation Grid. Nested Sequence time is projected onto the child Sequence's
grid, never the parent's.

The FFmpeg Adapter lowers the exact source target once to stream PTS using
checked integer arithmetic and nearest rounding with exact half-tick ties away
from zero, then adds the stream's declared start PTS. Invalid/negative targets,
invalid time bases, and overflow fail closed. The external FFmpeg still-frame
Adapter formats a microsecond command-line argument at the process boundary;
that lossy value is neither a cache key, scheduling authority, nor temporal
selection evidence. Until that Adapter returns the selected stream PTS and a
proven presentation extent, its rawvideo payload is discarded and the same
request continues through the in-process exact decoder; the unpublishable
optimization result never escapes as success. Distinct exact source instants therefore
cannot collapse into one request before FFmpeg's declared stream-time-base
quantization or be accepted afterward without evidence.
CPU `RgbaFrame` payloads are explicitly source-encoded RGB with straight alpha,
not implicit sRGB or working-linear pixels. Their applied YUV matrix/range and
source contract travel with the payload. Decode Sessions and the playback ring
are isolated by that source contract because they sit downstream of YUV-to-RGB
conversion; interpretation changes may reuse raw/native YUV resources, but
must not reuse differently converted CPU RGBA. Cross-request decoded residency
belongs only to the App's Preview Frame Store under the same exact contract.
Embedded ICC profiles likewise cannot manufacture that source contract. Media
ingest records a mapped ICC identity only when the shared core parser identifies
a supported named standard; generic RGB/GRAY profiles remain unmapped evidence
and enter the explicit missing-metadata policy.
App preview scheduling preserves decoder-session locality with semantic worker
lanes. The production App has at most two decode workers: worker 0 has Playback
affinity and worker 1 is the shared NonPlayback lane for scrub and deterministic
Still work. A worker may dequeue current work only when its lane accepts that
work class; this prevents an idle playback worker from cold-opening a second
interactive FFmpeg hardware session or blocking realtime work behind a
deterministic seek. `Prefetch` remains playback-only and lower priority than
every eligible current-frame request. With only one worker, the lane is `Any`
and all classes still make progress without a hidden cross-lane exception.
The sole bounded exception is recovery after a live Playback-lane execution has
authoritative cancellation evidence: the NonPlayback lane may take one
`Current + Playback` replacement ahead of ordinary Interactive/Still current
backlog. It never takes Playback Prefetch, never admits a second live failover,
and never releases the old execution's physical lease before normal return.
The NonPlayback worker carries an explicit per-worker decoder-thread cap into
its worker-owned media Session context. Therefore a failover request cannot
reinterpret its `PlaybackCursor` access mode as authority to open a second
full-machine FFmpeg software decoder while the canceled Playback Session is
still unwinding.
Because workers filter by lane, enqueue and priority promotion wake all preview
workers, not just one; otherwise a playback-only queue could wake the
non-playback worker and leave the playback worker asleep until another request
arrives. The three semantic access modes remain distinct even though scrub and
Still share physical execution locality.
Forward prefetch is a playback-only behavior. Settled still-frame preview and
active scrubbing must not enqueue `PlaybackCursor` prefetch work, because that
turns random access or latest-wins interaction into hidden background playback
decode and can keep project shutdown waiting on invisible media. In the app
preview scheduler, `MediaPreviewRequestPriority::Prefetch` is therefore valid
only with `PreviewDecodeAccessMode::PlaybackCursor`; non-playback prefetch
requests are rejected at admission and surfaced as structured diagnostics. The
worker transport queue repeats this invariant and derives priority only from
`MediaPreviewJob::priority`; queue callers must not pass a second priority value
that can drift from the job payload.
Playback forward prefetch is resource-bounded and lower priority, but its
admission is pipelined with current media work. A queued or in-flight current
request does not by itself suppress planning: the Broker always dequeues
Current before Prefetch, and the Frame Store projects headroom only after the
current reservation is charged. This lets a Playback worker move directly from
the exact current frame into the admitted sequential prefix instead of waiting
for another main-thread candidate turn. A current reservation that leaves no
physical headroom still forms a frontier, and queued plus in-flight Prefetch
that already covers the configured window suppresses duplicate admission.
`prefetch_skipped_current_work` is retained as a report-schema-compatible
coexistence observation; `prefetch_skipped_prefetch_backlog` remains the actual
covered-window skip. The configured forward window is derived from a
250 ms wall-clock horizon and the active sequence frame rate, then capped at
16 frames before enqueueing. The temporal prefix and its physical decode queue
are independent bounds: Priming may admit the complete prefix before starting
the Clock Master, while Running retains at most three queued-plus-in-flight
prefetch reservations and replenishes them on completion. This prevents farther
reservations from evicting the ready 4K frame buffer they exist to maintain.
Sixteen frames is only a temporal ceiling. Before admitting prefetch or counting a preroll prefix, the
scheduler asks the Preview Frame Store for typed
entry/host-byte/native-resource-unit speculative headroom. That snapshot comes
from the Store's physical allocation ledger: queued, in-flight, unconsumed
completion, external, and protected ownership remains charged, while
Store-exclusive LRU ownership is treated as releasable. Each new key consumes a
conservative source-plus-working/CPU-fallback reservation and, for native
requests, one decoder-resource unit from the planning copy. Budget exhaustion
shortens the speculative prefix even when the temporal horizon and queue still
have slots. Prefetch and preroll share this same near-to-far prepared-closure
planner. It holds short-lived frame-allocation leases for accepted nearer
resident keys while planning and admitting missing work, so the headroom
projection cannot release those residents in favor of farther requests.
Pending Broker keys retain their existing physical charge and are not counted
again. A media-bearing frame is available only when its complete dependency
closure fits; one deterministic partial frontier may warm incrementally but
cannot advance availability or permit farther work to bypass it. Blank frames
do not terminate the prefix, whereas dependency errors and terminal failures
do. Actual Store admission remains authoritative; the scheduler never inflates
policy to make all eight fit.
Once a slack-admitted `PlaybackCursor` prefetch begins, a later Playback-current
request does not by itself preempt the lease. The task has a two-second hard
execution budget and may finish only under normal Broker freshness/publication
rules; interactive current work and explicit resource pressure can still
preempt it. This lets cold isolated-demux and codec setup become reusable
Session locality instead of repeatedly canceled process launches during the
first second of playback.
After the priming-current frame is presented, its consumed Frame Demand must not
suppress this top-up. Preview continues admitting future `PlaybackCursor` work
until bounded preroll completes or the Engine's priming deadline releases the
Clock Master; a Viewer-only generation rotation may independently make the old
output stale without revoking that media admission.
Preview diagnostics expose this playback-clock contract as structured
`playback_schedule` evidence, including the current-frame display deadline
budget, the prefetch horizon/window, and invalid frame-rate counters. Invalid
sequence frame-rate data must warn through diagnostics instead of silently
removing playback deadlines or cache warming.
When the prefetch backlog is below the active physical reservation limit,
scheduling must top up only the remaining queued-plus-in-flight prefetch job budget across the
evaluated tracks and nested sequences, not enqueue a full new prefetch window
for each future frame offset. During Running that limit is three jobs; it is the
complete temporal window only while Priming builds the initial resident prefix.
The app preview service owns worker thread lifetimes. Service shutdown must be
non-blocking on the UI/event thread: it first closes the Frame Work Broker to
establish the authoritative cancellation instant, sets the timestamp-free local
stop flag, clears queued/UI generation state, and moves worker
handles to a background reaper that joins them after FFmpeg exits. A codec,
filesystem, or driver stall inside a preview worker must not prevent pause,
window close, or app quit from being processed. Each production worker owns one
explicit `PreviewDecodeSessionContext` and drops it before exit; production
codec, DPB, and hardware-surface residency must not hide behind TLS teardown.
Each dequeued Preview media job also has a per-job unwind boundary around codec
execution and frame materialization. A contained panic publishes a bounded,
typed `WorkerPanicked` completion with the original execution identity, so the
ordinary completed-publication-resolution path cannot leave ghost in-flight
Broker work. The panicked stack drops its move-only physical residency attempt,
the worker clears and rebuilds its local decode context from the same bootstrap,
and only then may it accept the next job. A panic is execution-facility
evidence, never a fabricated source decode error or a reason to terminate the
worker. Every dequeued execution also owns an armed RAII cleanup guard: normal
result publication, retirement, shutdown, or disconnect disarms it only after
that path has completed its own resolve/abandon responsibility; any panic
outside the catch boundary instead fails the exact execution binding on drop.
An otherwise idle worker performs a timed Broker receive and clears its owned
decode context after two seconds without work. This preserves
short-gap playback locality while bounding native decoder/device residency
during an open but inactive project; the next request cold-opens normally.
Decoded native frames have a second, independent lifetime in the Preview Frame
Store. A stopped Viewer releases that residency only after an exact registered
GPU output exists and Broker work is idle. Because output registration may be
observed just before the completing execution lease is resolved, the Runtime
retries this release before binding a different stopped generation, while the
old output still carries the only valid safety proof. Rotation retains the
output for stale presentation but deliberately removes its authority to release
source media. This ordering prevents one hardware surface pool per exact seek
from surviving across generation changes without weakening fail-closed
queued/in-flight checks.
The renderer has a distinct, shorter native-source lease while copying that
frame into its own shared texture. It releases the source resource when the
copy-ready fence completes rather than waiting for the next import; otherwise a
bounded decoder could wait for a surface that only the not-yet-possible next
import would release. Preview generation proves when the Frame Store may forget
the decoded payload, while the renderer fence proves when GPU command execution
no longer needs the native resource. Neither proof substitutes for the other.
The only alternate physical terminal for that short renderer lease is D3D's
typed device-removed fence sentinel: the bridge clears that exact retained
source and preserves `NativeDeviceRemoved` evidence. A wgpu device loss,
timeout, generic backend string error, Adapter drop, or generation rotation is
not a decoder-copy release proof and must retain the source fail-closed.
The worker-owned context contains one continuous Playback slot, a resource-governed
set of native-capable Interactive slots for scrub and GPU-resident exact Still
work, and one physically separate CPU Still slot. Sharing an Interactive slot is an
execution-locality decision, not a semantic shortcut: every request derives its
own seek, precision, decode-budget, approximation, and cancellation policy from
its declared access mode. CPU still extraction cannot reconfigure the native
Interactive slot.
Every logical native output carries one RAII token inside the retained FFmpeg
resource; App Frame Store clones and renderer copy-fence clones share that token
instead of incrementing it again. The token charges both one strongly retained
worker-family counter and the originating Session's local counter. An
Interactive slot may seek/flush or cross between Scrub and native exact Still
only after its local count reaches zero. Released slots are reused first. When
all existing outputs remain owned, the worker may add another Session only up
to its share of the App-owned current-media resource-unit grant; further work
waits at the same cooperatively cancellable `OutputLease` checkpoint. A policy
shrink retires only released slots and never revokes an in-flight native frame.
Playback remains a continuous pipeline
and may have multiple logical outputs in flight; dropping/replacing its Session
does not drop their family charges or retained FFmpeg surfaces. Clearing a
context always releases its codec/demux owners promptly, while family retirement
acknowledgement independently requires the family total to reach zero. Source
switching, cancellation, or Session drop therefore cannot erase proof for an
output still held downstream. No weak-observer list or “last output” approximation
participates. While Interactive output remains owned the worker waits at the explicit, cooperatively
cancellable `OutputLease` checkpoint instead of opening a spare hardware
context or entering a codec call that may block for a decoder surface. Reports
include `output_lease_wait_us`, and a wait that dominates a frame is classified
separately from queue wait, session open, seek, or packet decode. Number of
completed requests, generation rotation alone, cache eviction alone, and fixed
delays are not release proofs.
A released Interactive codec may execute another request only when its
packet-source execution family, conservative source revision, and codec/output
contract all match. Production Scrub and exact use the same isolated execution
family, so a healthy Session may cross access modes after the final native
output lease retires; access policy is still recomputed for every request.
Incomplete file metadata never authorizes Session, decoded-frame, or seek-index
cache reuse. Codec/DPB/frames teardown is reserved for a changed contract,
poisoned helper, residency-family retirement, idle retirement, or shutdown—not
for the historical fact that the last successful request was exact. This keeps
one surface pool without making generation count, elapsed time, or cache
eviction a false ownership proof.
The app scheduler lowers explicit `MediaPreviewAccessIntent` values to media
access modes. Viewer playback lowers to `PlaybackCursor`, active playhead/ruler
dragging lowers to `ScrubCursor`, and settled non-playing viewer frames plus
deterministic one-off work such as thumbnail stills lower to
`RandomAccessStillFrame`. The timeline widget emits explicit seek source
events, including a settled event on drag release, so preview scheduling does
not infer stillness from wall-clock timeouts. Playback pause, stop, project
switch, and natural end-of-playback transitions also settle the preview access
source before the next non-playing viewer request. New UI states must extend
that intent layer instead of passing booleans or strategy flags into
`mondrian-media`.
Playback, scrub, and exact-still cancellation discards partial output and never
masquerades as media failure or successful output. Cancellation before a
format command remains cooperative; cancellation of an in-flight isolated
seek/read terminates the helper, poisons the packet source, and makes the paired
decode Session ineligible for recovery or reuse. It is retired as a whole
without an otherwise normal codec flush. The worker-family hardware-device
pool remains a separate resource owner; only an actual attach, codec-open, or
hardware-execution failure retires the affected device generation. Exact Still always performs an
indexed exact seek and codec flush; scrub follows its independently derived
bounded low-latency policy. Neither may reuse the shared Interactive context
until its prior native-output lease has retired. After release, access-mode
change alone is not a terminal condition.
The playback decode Session also owns a small CPU-frame ring. A hit
requires containment in the entry's retained Decoded Presentation Extent and
is reported as `PlaybackSessionRingHit`; ring lookup never derives a tolerance
from nominal rate or frame diagnostics. Entries are not available to Scrub or
Still requests. The ring is bounded independently by eight entries and 96 MiB
of actual CPU payload bytes, rejects a single oversize payload, and dies with
its decoder Session. This keeps continuous playback locality inside the media
access-mode implementation without creating a second process-wide residency
authority.
Playback direction arrives explicitly in `PreviewDecodeAdaptiveHints`; the
media Session never guesses it from PTS arrival order. Because inter-frame
codecs still decode a GOP forward, reverse Playback additionally retains the
most recent decoded GOP tail in a private four-entry/96 MiB window. It is
bounded by both conservative decoded-surface bytes and retained-frame count,
uses the same exact presentation-extent selector, and dies or clears on Session
retirement, cancellation, seek, output-contract rebind, or forward traversal.
An adjacent reverse request may therefore materialize a retained decoded frame
with zero packet decode and zero repeated keyframe seek. This window is an
execution-local decoder optimization, not App Frame Store residency and not a
second semantic frame cache.
Forward session reuse must also preserve FFmpeg's send/receive backpressure
contract. After every packet, the decoder drains the complete ready queue to
`EAGAIN` before selecting or publishing. Selection therefore cannot return on a
first future frame while a closer future frame or duplicate PTS is already
ready. The next request also drains any output retained by codec threading before
submitting another packet; it must not
discard those frames or treat `AVERROR(EAGAIN)` from `avcodec_send_packet` as a
terminal media failure. This keeps sequential playback frame-exact and avoids
reopening or seeking a healthy decoder merely to clear its output queue.
Frame-grid lowering at this Seam always uses the frame time base (`1 / rate`),
never the frame-rate rational itself. The checked B-frame fixture exercises
exact random-access frames zero and one through one reused Session, including
the negative-DTS decode preroll; a test that supplies `25/1` where
`FramePosition` requires `1/25` would request 25 seconds and is invalid
evidence, not a decoder failure.
Access-mode decode behavior is centralized in a media-layer policy, not in app
conditionals or FFmpeg call sites. Playback has the widest mostly-forward
session reuse window, the playback ring, and the exact-path forward decode
budget; scrub has only a very short forward reuse window, a smaller CPU
fallback forward-scan budget, no playback ring, and the low-latency
probe-backed approximate-first-frame strategy with a bounded-any fallback;
random-access still extraction has no forward reuse
and keeps keyframe-safe exact seeking with the exact-path budget. Scrub
low-latency seek is product semantics, not an opt-in environment variable. This
preserves still-frame correctness while preventing latest-wins scrubbing from
spending the same long-GOP CPU budget as deterministic extraction, and leaves a
clear replacement point for future hardware-resident playback and low-latency
scrub backends.

Software decoder threading follows the same residency boundary. Interactive
and Still sessions use the fair per-worker CPU share, while `PlaybackCursor`
may use the larger bounded decoder-thread grant after reserving UI, render, and
audio cores. Playback and Interactive residency are ordinarily mutually
exclusive. The one cancellation failover exception retains the NonPlayback
worker's fair-share cap even though its request is `PlaybackCursor`, so it
cannot multiply the full Playback grant while the old Session returns. Explicit
thread-count environment overrides remain bounded by the same machine grant.
Real-media validation treats hardware engagement as an optimization: a CPU
fallback that proves the complete continuous window as exact, on-time Ready
output stays at full quality. Half/Quarter execution is mandatory only when the
fallback misses that evidence; tests must not manufacture degradation merely
because a requested hardware profile was unsupported.

A settled paused Viewer output automatically retires decoder-native frames but
keeps ordinary CPU-decoded frames inside the bounded Frame Store. This prevents
a single native handle from pinning a hardware surface pool while preserving
the exact paused CPU frame for immediate Play. Explicit lifecycle, pressure,
and validation cleanup may still clear all decoded-media residency.

The normal scrub budget derived from an indexed keyframe includes
decoder-reordering headroom and retains the unindexed safety floor.
Frame-threaded codecs and imperfect container indexes can make the actual
decode distance exceed the simple presentation-frame distance, so that
distance alone is not a safe stopping point. Adaptive hot/slow/recovery modes
may still tighten that floor because newer input cooperatively cancels their
work. Timeout and forward-budget exhaustion are transient scheduling outcomes
and must not enter the terminal media-failure cache; a settled deterministic
request for the same frame must remain eligible to retry exactly.
Likewise, completion of a canceled current scrub or still request schedules one
follow-up render pass so the settled position can submit fresh work after its
scheduler entry is released. Playback deadline cancellations do not use this
rule, because immediate resubmission would create a realtime retry loop. Decode
cancellation is scheduler evidence, not a frame presentation: it records its
reason and return latency but must not emit `FrameDelivery::Canceled` for an
identity already replaced by latest-wins scheduling.

Exact Preview access is defined by a proven Decoded Presentation Extent, not
by a nominal frame-rate tolerance or nearest-PTS distance. A selected frame
covers source time only inside `[selected_pts, selected_pts +
selected_duration_pts)`. A positive decoded-frame duration supplies a
provisional end only while no successor is known. Once a successor PTS is
observed, that timestamp is the authoritative exclusive boundary: it truncates
overlapping duration metadata and extends a shorter packet duration through a
VFR cadence gap, matching continuous video presentation's predecessor hold.
For an interior request the exact decoder keeps one-frame lookahead whenever a
successor can still arrive; at EOF a positive duration remains sufficient.
Without a positive duration or successor, only equality with `selected_pts` is
exact. A duration or successor proves the same interval for Playback, Scrub,
and Still; access mode changes only the permitted fallback when no interval
covers. Playback and random-access still selection prefer the causal covering
frame and must never publish a future nearest frame as exact. Scrub may select
the nearest non-covering frame only with explicit Degraded evidence. A proven
long VFR hold is not constrained by nominal-frame distance. The Session keeps
at most a two-candidate temporal window across forward calls: the selected
frame and, only when it was decoded to prove that frame's boundary, its first
successor. Consuming a successor for exact selection and then forgetting it is
forbidden because the next forward request must be able to select that already
decoded frame without seeking or stretching its predecessor. The session-local
insertion rule is unique: retain the maximum PTS at/before the target and the
minimum PTS after it, independent of receive order, while `last_pts` remains a
monotonic high-water mark. Equal PTS keeps the first decoded frame deterministically;
once a duplicate is observed, Playback and deterministic Still fail closed with
typed `DecodeTemporalMismatch`, while Scrub retains its ordinary interval-based
approximation classification.
The session-local
playback ring returns its own authoritative extent with the payload. Both apply
the same end-exclusive interval contract rather than reconstructing a
tolerance from nominal rate or mutable diagnostics.

Every preview decode diagnostic emitted by `mondrian-media` must carry the
resolved access-mode policy contract alongside the observed result:
`seek_strategy`, `forward_reuse_frame_window`,
`forward_decode_budget_frames`, `any_seek_window_ms`, `requested_pts`,
`selected_pts`, `selected_duration_pts`,
`selected_temporal_extent_source`, and `temporal_approximation`. App/UI
performance reports may aggregate those fields, but must not reconstruct them
from app conditionals. This keeps policy bugs diagnosable: for example, a scrub
sample that reports `BoundedAnyFrame` with a zero `any_seek_window_ms` is a
broken media contract, not a UI presentation issue. A deterministic exact-path
`TemporalMismatch` is retained only for the current Preview generation. One
refresh publishes the failed/Unavailable state; subsequent evaluation of the
same media key does not resubmit identical work, while a new generation remains
eligible to retry.
Each in-process preview decode session also maintains a session-local,
incremental keyframe seek index from video packet metadata observed during real
decode work. The index may bound later seeks to an already-known keyframe
anchor. When bounded scrub has index evidence, it selects the nearest keyframe,
seeks toward that timestamp with keyframe-safe backward semantics, and requests
FFmpeg `NonKey` discard as an optimization. Hardware decoders are not required
to honor that discard hint: the warm phase accepts the first valid decoded
frame within the adjacent indexed-GOP radius, preventing a slow-result feedback
loop from making the same target permanently exhaust a smaller recovery budget.
Diagnostics preserve both requested and selected PTS plus
`temporal_approximation`; presentation policy reports the result as Degraded
rather than claiming frame accuracy. `AVSEEK_FLAG_ANY` is
reserved for the unindexed bounded-window fallback. The index must not perform
a blocking whole-file scan on first frame or
pretend that unknown GOP structure is known. This is a CPU fallback bridge
toward a real GOP/keyframe map: future probe-backed indexes and hardware
decode session adapters should replace the evidence source behind the media
request boundary, while preserving the same diagnostics for availability,
observed keyframes/packets, and whether a seek actually used an index anchor.
App-owned Preview Frame Store entries retain the original payload's temporal
evidence. Session-local playback-ring hits bind the ring's retained Decoded
Presentation Extent to the current `requested_pts` while replacing only
execution/cache diagnostics. `mondrian-media` owns no process-wide decoded-frame
cache. The App's strong decoded-frame identity
binds that selected PTS together with the complete request and actual
payload/color or native-surface contract, so a nearby scrub selection cannot
alias the settled exact frame and cache provenance cannot spuriously rename
identical retained pixels.
App, export, and thumbnail callers submit a `PreviewDecodeRequest` to the
media preview decode boundary instead of matching on `PreviewDecodeAccessMode`
or calling mode-specific FFmpeg helpers. Access-mode routing, session
retention, cache lookup, playback-ring use, and future hardware/low-copy
backend selection must stay behind the request boundary in `mondrian-media`.
The request may carry a `PreviewHardwareDecodeRequest`, but this is only caller
intent: `Auto` means use the media default, `PreferHardwareDecode` means prefer
FFmpeg hardware decode even when decoded frames must transfer back to CPU RGBA,
`PreferGpuResident` means prefer a native GPU-resident decoded surface when the
access mode/backend can provide one, and `RequireGpuResident` means fail closed
instead of silently returning a CPU RGBA frame. The app playback scheduler may
use `PreferHardwareDecode` for `PlaybackCursor` while renderer native import is
not ready, then upgrade to `PreferGpuResident` only after device-scoped Renderer
native decoded-frame import admission succeeds. Scrub and still-frame work
should remain `Auto` unless a future backend explicitly supports those access
patterns.
Preview decode diagnostics also carry decoder payload sampling facts:
`DecodedVideoSampling` records the decoder-reported YCbCr matrix, encoded
range, chroma location, and effective bit depth observed on the decoded FFmpeg
frame. These values are media facts,
not color-management interpretation. `mondrian-media` must not translate them
into renderer sampling contracts or infer missing values from platform defaults.
The app layer combines them with the resolved source color space when evaluating
native decoded-frame import readiness; missing or unsupported sampling remains a
structured blocker instead of silently falling back to guessed NV12/P010 shader
constants.
Absent matrix metadata is distinct from an explicitly unsupported matrix.
Only the absent case may use a matrix fallback explicitly bound into the
resolved source contract (currently the Sequence `AssumeRec709` policy); explicit
BT.2020 constant-luminance, derived, YCgCo, and ICtCp-style matrices remain
fail-closed until their conversion math is implemented.
Complete BT.470BG/gamma-2.8 and SMPTE 170M primaries/transfer pairs resolve to
the PAL and NTSC Rec.601 product color spaces respectively, regardless of
whether the decoded samples are RGB or YCbCr. Matrix remains an independent
sampling fact. Partial matrix-only metadata remains
diagnosed as partial evidence but may still select the matching Rec.601 family;
it must not be relabeled as Rec.709.
Metadata-hint normalization also recognizes exact ACES2065-1, ACEScg, ACEScct,
linear Rec.709/Rec.2020/P3-D65, and Sony S-Log2/S-Gamut identities. These hints
retain the same priority/evidence/warning model as existing camera Log hints;
generic `linear`, `ACES`, or `EXR` text is insufficient. Scene-linear sources
force high-precision proxy admission just like HDR and scene-Log sources.
App preview decode execution must run synchronous FFmpeg preview decode on
dedicated preview worker threads, not on the UI/event thread. Current-frame and
prefetch workers pass a cooperative cancellation predicate into
`PreviewDecodeRequest`, and the media loop checks that predicate before
open, seek, packet decode, frame receive, EOF drain, and RGBA conversion. Do
not depend on thread abort to preempt synchronous packet decode.
Decoded Preview payload ownership is UI-independent:
`app::preview_media_frame` owns decoded CPU/source/native residency, lazy CPU
working adaptation, logical and sampled geometry, presentation quality, decode
provenance, and exact host/decoder reservation. Its closed payload enum admits
exactly one of working CPU, source-domain CPU/GPU-capable, or native decoder
surface residency; no frame can be empty or claim contradictory residency. The
concrete worker loop, cancellation checkpoints, result publication, bounded
shutdown signal, and FFmpeg Preview Adapter live together in the UI-independent
`app::preview_media_task` Module. Window and Headless Adapters consume the same
structured terminal result and may not implement a second decode loop.
Each media worker creates one persistent cancellation observer, communicates
with it over bounded command/response channels, and explicitly joins it at
worker shutdown. The observer waits on the Broker lifecycle condition variable,
freezes the first logical cancellation fact, and wakes on cancellation,
completion, removal, or close; it is not spawned or detached per frame. Codec
probes read only that frozen logical fact. A returned
`PreviewDecodeCancellation` remains separate evidence that a concrete FFmpeg or
media checkpoint actually yielded.
`app::preview_media_source` independently resolves an immutable asset record and
complete Viewer intent into one canonical key, explicit color rejection, or
structured unavailable outcome. It owns source/proxy policy,
color/range/Alpha author interpretation and proxy intent; the
media physical-contract Interface owns revision validation, absolute stream
binding, native-surface classification, and canonical CPU/native decode
geometry. `MediaPreviewKey` now composes exactly one `PreviewDecodeKey` and
retains only App semantics outside it: `AssetId`, the original Asset's logical
source resolution, author Alpha interpretation, working color, input tone-map,
and Project engine. It has no parallel path, fingerprint, stream, source-time,
decode-extent, source-color/range, physical Alpha, or native-surface fields.
The Preview composition snapshot supplies
the immutable Asset Library view, while `app::preview_runtime::media_adapter`
owns lookup and submits optional proxy demand only through a narrow
`PreviewProxyDemandSink`. The Proxy service owns deduplication and scheduling.
Window code retains only typed presentation registration and evidence
projection.
The renderer's transient `PreparedVisualFrameClosure` is the sole canonical
recursive visual interpretation. It fixes nested Sequence lookup, exact child
time, cycle/depth, per-Sequence execution resolution, color context,
Transition endpoint side, temporal sample binding, and distinct instance paths
before any media request. `app::preview_timeline_execution` consumes those
typed nodes and bindings to materialize nested working-space
composition/conversion, propagate pending/unavailable outcomes, and publish
mandatory ready-plan cache identity plus ordered execution facts. Window and
Headless Adapters supply the same typed media outcome seam and may project
facts; neither the App Module nor an Adapter may maintain a second recursion,
time projection, or child-sizing rule. The same App Module exposes read-only
media-demand collection by iterating the prepared closure. Prefetch, preroll
readiness, and input-color evidence consume it instead of re-evaluating nested
plans.
The UI-independent `app::preview_viewer_plan` Module owns resolved element
representation, stable cache identity, quality/provenance aggregation, deferred
composite classification, and renderer GPU-layer lowering. Final presentation
arbitration and diagnostics live in separate private deep Modules.
The sibling UI-independent `app::preview_cpu_execution` Module owns source to
working-linear preparation, renderer timeline composition, Program Output,
monitor adaptation, and exact stage durations. It returns pixels plus all input,
composite, output, and monitor facts; Window diagnostics only project that result,
and Headless execution asserts the same result through the shared
`PreviewProductionRuntime`.
`app::preview_runtime::presentation` exclusively
chooses exact registered GPU output, raster cache, scoped stale reuse, deferred
playback composite, or the CPU output boundary; it cannot schedule media work
or mutate transport. CPU raster cache and stale state use the validated,
UI-independent `app::preview_raster_frame::PreviewRasterFrame`; only this final
Window Adapter converts it to a Widget `ViewerFrameImage`, without copying the
shared pixel allocation. Raster extent, encoded color identity, byte reservation,
and stable resource naming therefore remain available to Headless presentation
without importing Widget types. `app::preview_runtime::result_pump` exclusively
performs the bounded foreground drain of completed work, Broker resolution,
deadline expiry, cache admission, and terminal Frame Delivery projection; it may
record facts but cannot invent generation or deadline authority. Window lifecycle
code initiates project/switch shutdown in `service_lifecycle`, while the shutdown
signal, worker observation, and bounded exit behavior stay in
`app::preview_media_task`; teardown cannot become an alternate completion policy.
Playback Epoch and interactive/playback-family rotation retire pending work,
generation-bound output, and decoder-backed surfaces, but do not invalidate an
exact semantic CPU media/raster cache entry. Those entries remain governed by
their source revision, time, geometry, color, and effect identity; project or
Authoring Session lifecycle rotation clears the complete Frame Store. This keeps
transport authority separate from immutable cache validity and avoids a
synchronous re-decode merely because the user pressed Play or Pause.
The sole Frame Store policy Adapter inside the Preview Production Runtime is
`app::preview_frame_store::PreviewFrameStoreAdapter`; its generic storage
and residency algorithm remains owned by `mondrian-playback::PreviewFrameStore`,
while diagnostic aggregation remains in the Preview Adapter. This is a
behavioral module boundary, not a second scheduler: all admission, deadline,
generation, and worker-lane authority still comes from `app::preview_access_mode`
and `mondrian-playback::FrameWorkBroker`.
This Store is the only persistent decoded-frame residency authority shared by
Preview requests. Media keys with absent or incomplete filesystem revision
evidence are never read from or admitted to it. Ordinary residency is bounded
independently by entry count, exact host bytes, and opaque decoder-resource
units. Each Broker attempt owns its own move-only physical work lease; a
successful measured admission consumes it into one cloneable frame allocation
shared by Store and external owners. Optional residency is trim-responsive.
Current correctness instead uses a typed, untrimmed per-demand grant and a
bounded multi-key overflow LRU; all demands together remain under the
component-wise aggregate hard grant. Demand protection counts a physical
allocation once and follows it through asynchronous visual execution. A
capacity refusal is a resource blocker, not a source decode error or an
unbounded continuity pin. A new Authoring Session cancels Broker work, rotates
the prepared-visual scope, and clears Store ownership before any new request can
reuse durable IDs that happen to match an earlier open; Broker, worker, result,
and external leases remain charged until their real owners retire. Frame-work
generation remains Broker lifecycle identity, not a duplicated cache-key field.
The cross-Adapter GPU output blocker taxonomy and its aggregate breakdown live
in UI-independent `app::preview_gpu_output_blocker`. Renderer, display-contract,
Window, and Headless Adapters may contribute typed facts to it, but no Widget
module owns or reinterprets those diagnostic identities.
Mutable counter accumulation and point-in-time diagnostics projection are
localized in `app::preview_runtime::evidence`. It records facts selected elsewhere;
it cannot schedule, change pressure state, or derive pass/fail verdicts.
Within diagnostics, the versioned Color Health report model and its
check/root-cause/action derivation live in the private `color_health` deep
Module. It consumes the same immutable aggregate as before and cannot count
execution independently from the Preview Adapter.
The sibling private `performance` Module derives versioned Decode/Render
budget checks, verdicts, bottleneck classification, root causes, and actions
from immutable summaries. Public report builders are re-exported unchanged;
the Module owns no counters, scheduler feedback, or acceptance thresholds.
`app::preview_runtime::request_scheduler` is the concrete Adapter that consumes
canonical current/nested media demands, queries preroll residency, applies the
already selected adaptive hints, and submits Broker work. It does not own pressure
thresholds, access-mode mapping, deadlines, or generation identity; those
remain in the UI-independent policy/Broker Modules.
Its request seam carries one `MediaPreviewRequestIntent`, which derives both
Broker priority and Frame Store intent; callers cannot combine Current priority
with a missing or different working-set identity. Before reserving physical
decode capacity, the Adapter submits a payload-free binding update carrying the
exact semantic key and `FrameWorkResourceScope::Media(intent)`. The Broker
retains an existing queued payload or rebinds compatible in-flight work only
when both values match. `NeedsPayload` leaves scheduling untouched and is the
only path that may acquire a new move-only Frame Store work lease. This makes
repeated Viewer evaluation idempotent with respect to physical charging while
preserving independent accounting for different current demands and Prefetch.
Its exhaustive result keeps
newly scheduled work, compatible existing work, already-resident media,
observable pressure, per-demand blockers, aggregate blockers, obsolete
generation, invalid scheduling/source identity, and terminal worker health
distinct. `media_frame_for_plan` sets Preview pending only for an outcome with
a concrete progress edge. It reports grant exhaustion or unobservable aggregate
ownership as `Blocked`, and obsolete/invalid/worker-terminal outcomes as
non-pending typed unavailability. This prevents a rejected reservation from
leaving the Viewer in Loading with no job or completion.
Viewer lifecycle is adapted through `app_ui::playback_feedback` into typed Frame
Deliveries. `Ready`, `StaleAvailable`, and `Blocked` are terminal observations;
`Loading` is non-terminal pending work. Loading or stale presentation does not
hold the Synthetic/Audio Clock Master and does not mute or clear realtime audio.
Late video is dropped while authoritative media time continues.

Current playback presentation deadlines originate in the Playback Engine Frame
Demand. The app Adapter converts the remaining monotonic lifetime to an
`Instant` at enqueue/promotion time and submits it unchanged to the Broker.
That deadline expires queued work and classifies worker completion, but
`Current + PlaybackCursor` explicitly uses
`FrameInFlightDeadlinePolicy::FinishForLocality`: once its playback-lane lease
has started, a display miss cannot tear down the worker-owned sequential
demux/codec Session. Full/Half/Quarter, output dimensions, monitor/color output
contracts and presentation-source changes within the same Playback Epoch and
unchanged Sequence/Project author authority also rotate Viewer/output generation
without canceling that lease; it loses publication authority first and an
on-time, reusable result may finish only as exact-key `CacheOnly` media work.
Seek, source authoring, Project authoring
and lifecycle changes retain ordinary latest-generation cancellation.
`app::preview_scheduler_policy` owns the pure
worker-deadline eligibility, executed decode-quality classification, and bounded
frame-rate-to-prefetch-window policy;
`app::preview_access_mode` owns Broker admission/job transport,
`app::preview_media_task` owns concrete decode execution and cooperative
cancellation observation, `app::preview_media_source` owns canonical source
interpretation, renderer `PreparedVisualFrameClosure` owns canonical recursive
Timeline semantics, and `app::preview_timeline_execution` owns Preview pixel
materialization over its typed bindings. `app::preview_runtime` owns
asset-library and proxy-dispatch side effects plus immutable diagnostics
projection. The shallow `app_ui::preview` Adapter owns only Widget conversion.
None of them duplicate clock math, construct a second media key, or reinterpret
nesting.

The same current request carries an opaque Frame Demand identity end-to-end.
The preview queue and worker may transport but must not interpret that identity;
the poll Adapter converts the exact result into a terminal Frame Delivery and
only `mondrian-playback` decides whether it still belongs to the active Playback
Session. Decode generation continues to guard media task/cache relevance, but
cannot authorize a transport transition or replace epoch/demand validation.

The app/window layer retains a separate interaction escape hatch: `Loading`
feedback may defer synchronous reconstruction of the same GPU preview candidate
during redraw. This is a presentation-work guard, not transport buffering, and
cannot advance, pause, or select the Clock Master. Transport actions still
cancel obsolete preview generations and refresh controls without synchronously
requesting new preview/composite work.
Background polling must also enforce a hard escape hatch for the current
realtime playback request. If a scheduler-accepted `Current + PlaybackCursor`
request remains pending past the realtime stall budget, the app preview service
expires only that playback-current pending work, removes matching queued worker
jobs, and clears the current-frame pending flag.
The shorter playing-frame stall budget is disabled during `Priming`: the
Playback Engine's own bounded preroll deadline remains authoritative, allowing
a cold software-decoded first frame to establish output instead of being
reclassified Late every 250 ms.
The scheduler returns the expired key, access mode, and original optional Frame
Demand identity instead of requiring the host to inspect current transport.
An expired `PlaybackCursor` carrying that identity records
`playback_current_stalled_expirations` and emits an exact Late Frame Delivery.
`ScrubCursor` is explicitly outside this wall-clock expiry: source open, GOP
seek, or index construction may legitimately exceed a playback presentation
window, and interactive latest-wins generation changes already cancel obsolete
scrubs. Applying the playback timeout to a stable scrub would repeatedly cancel
slow camera originals before their first decodable frame. This
is not a renderer fallback and must not clear ready/stale frames,
still-frame work, media caches, or external GPU viewer textures. It exists so a
lost, wedged, or pathologically slow current decode cannot retain scheduler
capacity indefinitely.
Expiration is transport-only feedback: the host refreshes transport/pending
models without synchronously rebuilding Viewer preview content.
The same counter must appear in the preview decode performance summary/report
even before a worker returns a canceled decode result, because the product
symptom is already user-visible at the moment the demand expires.
Expired playback work counts as one late-drop and proxy/hardware recommendation
only when its identity equals the Playback Engine's still-pending demand.
Redundant jobs collapse to that single decision; work that outlives an accepted
presentation releases capacity without reviving or penalizing the completed
demand. Re-requesting the same media key
refreshes stall age only when generation, access mode, or Frame Demand identity
changes; duplicate redraws for one demand cannot keep stalled work alive.
Consecutive current playback late drops form a sustained playback pressure
state. Once the app preview service observes the pressure threshold, it must
suppress forward playback prefetch and keep worker/queue capacity available for
the visible current frame. The state exits only after a successful `Current`
`PlaybackCursor` result. This recovery path must be structured diagnostics, not
opaque logging: reports include the late streak, pressure entries, recoveries,
and prefetch skips. It must not silently enable proxies, lower decode quality,
or choose hardware decode outside the project/backend policy; those are
separate recovery decisions layered behind the same playback controller.
Performance gates must include real-media playback fixtures, not only synthetic
generated clips. The ignored
`preview_media_external_continuous_playback_smoke` test accepts
`MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH` (or the shared
`MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH`) so CI or a local workstation can run
4K HEVC Main10, 4K H.264, HDR PQ/HLG, and Long-GOP camera samples through the
same playback decode/render/color report contract. A sustained playback
pressure root cause is a playback smoke failure, not merely advisory evidence.
The external real-media variant also emits `real_media_gates` and fails on
`PlaybackCursor` decode p95, queue-wait p95, or visible-frame-ratio regression;
those gates must remain separate from broad timeout windows so slow-but-eventual
4K playback is not mistaken for production readiness. The generic decode report
still records and diagnoses the slowest individual frame, but the continuous
playback hard-failure selector does not let one session-open maximum override a
healthy p95 plus readiness gate. Timeout, queue loss, invalid access modes,
sustained pressure, missing locality, and p95 regressions remain hard failures.
Exact-current readiness is proven by the union lower bound of accepted Engine
`Ready` deliveries and de-duplicated Viewer GPU publications. This matters at a
clock boundary: an older Frame Demand can be superseded after its exact output
has already become visible. The two ledgers remain independently gated, so a
repeated stale output cannot borrow coverage from rendered-but-unpublished GPU
work and a missing Engine delivery cannot erase an already-proven presentation.

External smoke media is always registered from one real `probe_media_info`;
the harness does not synthesize codec, profile, resolution, bit depth, duration,
frame count, or color facts from a filename. It builds the sequence at the
probed rational frame rate and advances 1× using a nanosecond frame interval.
The media probe canonicalizes positive FFmpeg average rates that fall within
100 ppm of a standard nominal cinema/broadcast rate. This removes container
time-base quantization (for example, an 11 ppm drift around `24000/1001`)
without rounding genuinely distinct or variable rates into a standard cadence.
The dedicated ignored
`preview_media_professional_4k_hevc_main10_hardware_playback_gate` requires
`MONDRIAN_PREVIEW_PROFESSIONAL_4K_HEVC_MAIN10_MEDIA_PATH` and never reports a
skip when that fixture is absent. Before starting GPU work, the harness rejects
sources shorter than the complete observation window; it must never extend a
short clip beyond source EOF and call that professional evidence. Its
`professional_media_gates` require decoder-proven UHD HEVC Main10 identity,
sufficient duration, 23.976/24/25/29.97/30/50/59.94/60 fps, and at least 90%
actual P010/10-bit hardware provenance on media layers attached to completed
headless Viewer GPU candidates. CPU-transfer hardware decode and retained
native hardware decode are reported separately; both are actual hardware
execution, while candidate/config/device probes are not.
The `uhd_hevc_main10_hardware_1x_v7` report additionally requires the complete
Headless execution-resource cycle to run at least once per planned observation
frame. GPU-completion, Prepared Viewer Successor, and exact-current alias fast
paths cannot bypass process-tree memory sampling or the Preview/Viewer resource
projections merely because they avoid a new decode/render request.
The same long gate samples `ProductProcessTree`, never `CurrentProcess`:
the App root, isolated demux helpers, and any other live descendant must all be
present in one complete OS inventory before their checked aggregate typed
private-memory metric can contribute to the plateau. The current Windows
qualification profile requires Windows Private Commit; Linux anonymous resident
memory and macOS physical footprint are separate future profile metrics, not
aliases. Missing or changing child membership,
one inaccessible process, or an unsupported platform Adapter fails the memory
gate closed; whole-system available-memory pressure is useful policy evidence
but cannot replace product ownership accounting.
Once a GPU Viewer Adapter has admitted a hardware decode request and device
selector, that decision applies to playback, active scrub, and settled still
access modes. Reverting scrub or still requests to `Auto` would silently move a
4K Main10 interaction from the admitted native P010 path back to CPU decode and
is forbidden. CPU-only preview services retain the default `Auto` policy
because no GPU Adapter admission is installed.
UI-independent `app::preview_hardware_admission` stores the device-scoped
Renderer observation as one Copy snapshot (or the explicit pre-discovery
`None` state).
Base request, native-surface-specific downgrade, and device selector are always
projected from that same observation; independently updated booleans cannot
manufacture a mixed admission state. The concrete Preview module only projects
this state into its diagnostics and job fields; it owns no admission rule.
Pre-discovery state is `Auto`, and a missing/unmapped physical surface hint can
at most retain hardware decode with CPU transfer; neither can authorize
`PreferGpuResident`. Payload requirement and that coherent admission snapshot
are canonicalized once into `PreviewDecodeGeometry`. A CPU-addressable key
remains `FitWithin` even if a later observation gains native support, so
scheduler promotion cannot mutate one cache identity into a native payload.
CPU Preview decode also distinguishes the media representation from the Viewer
output extent. Authored Preview scale, Window size, and nested composition
resolution are spatial targets and never enter `PreviewDecodeKey`. Playback's
temporary `PreviewResolutionScale` is different: each coherently sampled frame
request projects `Full`, `Half`, or `Quarter` into an explicit media
representation (`NativeCpu`, `Reduced(2)`, or `Reduced(4)`). A source with an
exact compact YUV contract instead uses `CompactCpuYuv` or
`ReducedCompactCpuYuv(divisor)`, so adaptive recovery cannot expand a proven
plane payload into RGBA. That choice rotates
the decode/cache identity and charges residency at the materialized source
raster. Returning to Full restores the original identity, so the full and
reduced frames may coexist without invalidating one another. A renderer-admitted
native surface remains at source extent and is sampled directly into the lower
Viewer target; reduced CPU decode must not force a native surface through host
memory. Divisor one and empty proxy representations are rejected at the decode
key boundary to prevent duplicate or non-materializable cache identities.
Representation is a materialization/cache contract, not compressed-stream
decoder identity. For an unchanged source, stream, hardware plan, and color
contract, the worker reuses its demux/codec Session and atomically rebinds only
the representation geometry, scaler, and materialized-frame rings.
Tests that assert fixed residency counts must inject an explicit machine-resource
profile; they must not inherit product host detection because that detection can
legitimately select a reduced realtime representation.
When background preview completion changes Viewer lifecycle, the app host may
perform one preview-aware model refresh, then adapt its payload-free feedback
without requesting preview again. A feedback transition must not trigger a
second full root refresh or layout pass that re-enters preview interpretation.
Completion policy itself is not Window-owned. `app::playback_preview` samples
the pending Frame Demand once, asks the production Preview Adapter to combine
bounded completion drain with stalled-current expiration, applies exact terminal
Frame Deliveries, and only then observes video preroll. The Window host and real
Headless GPU harness call this same pump; neither may reproduce this ordering or
interpret worker results independently. The Window still owns repaint/layout,
and the Preview Adapter still owns decode/cache facts, so this seam does not move
Widget or codec responsibilities into the Playback Engine.
The native app event loop also records stage-level responsiveness telemetry for
action draining, redraw, GPU preview preparation, UI refresh, paint/render,
background-task polling, and playback-clock advancement. Any stage that exceeds
the UI responsiveness budget is logged with the stable stage name and elapsed
time. This diagnostic boundary is intentionally in the app layer because it
measures host scheduling and UI-thread residency, not media decode semantics;
media/render changes must preserve it so a future playback report can identify
whether the stall is decode backlog, GPU preview preparation, redraw/render, or
control dispatch.
Completed preview decode results are also consumed under an explicit UI-thread
budget. The preview service may process only a bounded number of completions per
poll and must yield once the completion-drain time budget is reached, requesting
a follow-up tick instead of monopolizing the event loop. Diagnostics must expose
completion poll calls, drained results, count-budget exhaustions, time-budget
exhaustions, and poll durations. This keeps worker bursts, cache insertion, and
decode diagnostic aggregation from delaying transport controls or close/quit
events during buffering. The producer side is independently bounded to eight
results, equal to one maximum foreground drain. At most one additional result
may remain owned by each of the two workers while that queue is full. A blocked
publisher polls shutdown and decoder-residency revisions: shutdown abandons its
lease, while a transport-family transition resolves the completed binding as
non-reusable, drops the payload, then returns to the worker-owned
codec/surface-pool retirement barrier. It may never wait indefinitely on a
channel send or leave hidden pending Broker state.
The same event-loop rule applies to thumbnail and waveform completion queues
consumed by `AppUiHost::poll_background_tasks`: they may request another tick
when backlog remains, but they must not drain an unbounded worker burst on the
UI thread. Waveform polling drains at most eight results or two milliseconds
per turn; execution lifecycle and publication remain in
`AudioWaveformService`, not `AppUiHost`.
App preview decode timeout is access-mode-specific, not a single global
playback policy. `ScrubCursor` has the shortest caller-release budget because
interactive latest-wins work must not leave the UI waiting behind pathological
seeks. `RandomAccessStillFrame` may wait longer because exact still extraction
is deterministic one-off work. `PlaybackCursor` sits between those modes and
relies on prefetch cancellation and session locality for sustained playback.
Diagnostic environment overrides may tune or disable these watchdogs, but they
must preserve the per-access-mode structure rather than reintroducing one
opaque timeout for every request.
Timeouts are first-class decode failures, not string-only log messages.
App preview diagnostics emit timeout evidence with the asset id, requested
access mode, timeout budget, frame number, and source timestamp. Diagnostics
must separate `decode_timeouts` from aggregate `decode_failures`, and preserve
timeout counts per access mode. A timeout should therefore point directly at the
failing access contract (`PlaybackCursor`,
`ScrubCursor`, or `RandomAccessStillFrame`) instead of only showing a generic
"decode failed" counter.
Forward-scan budget exhaustion is also a structured decode failure. When a
media-layer access policy hits its forward decode frame budget without finding
an acceptable frame, the media crate must return a typed
`DecodeBudgetExhausted` error containing the requested access mode, decoded
frame count, budget, and target PTS. App preview diagnostics must aggregate
these failures globally and per access mode, and perf reports must surface a
budget-exhausted root cause instead of folding the event into an opaque decode
error or pretending the sample merely timed out. App preview reports must also
carry per-access-mode buckets for decode failures, timeouts, and budget
exhaustion, so engineers can diagnose whether pressure is coming from
`PlaybackCursor`, `ScrubCursor`, or `RandomAccessStillFrame`.
The app preview scheduler stores access mode alongside the media-frame key for
pending/in-flight work. A later scrub/current request for the same media frame
must supersede an older playback/prefetch request instead of letting the older
job complete and remove the pending scrub work.
In the app layer this contract lives in `app::preview_access_mode`:
request admission, latest-generation tracking, access-mode promotion, and
completion classification are localized there. The same module owns the bounded
preview decode job queue and worker-lane selection, because those policies are
defined by access mode. It has no dependency on `app_ui`; thumbnail, Viewer,
frame-store, and scheduler-policy Adapters all depend inward on this one module.
The module must not own render plan evaluation, decode
execution, color interpretation, or GPU/CPU frame conversion; those remain in
the preview orchestrator and media/renderer layers.
Worker-lane selection reserves CPU capacity instead of maximizing raw decode
throughput. The media layer exposes a single `PreviewDecodeCpuBudget` that
coordinates app preview worker count with FFmpeg decoder threads per worker.
The app scheduler uses that budget for lane count, while the media decoder uses
the same budget for its default FFmpeg threading request. This avoids the
dangerous `preview workers * FFmpeg decoder threads` over-subscription pattern
that can make software decode starve UI input, audio, and render submission.
CPU scene-linear float materialization and bilinear resize execute serially
inside that already-admitted media worker. They must not fan each worker into
Rayon's process-global pool or any other unbudgeted inner executor; measured
inner parallelism may be added only through an explicit media-domain grant with
cancellation and workload evidence.
Each worker bootstrap carries its lane-specific FFmpeg thread ceiling into the
non-`Send` decode context. Access-mode defaults and environment overrides may
choose any lower value but cannot exceed that ceiling; diagnostics report the
actual decoder threading observed after FFmpeg opens the codec.
Single-worker systems use one `Any` lane; every multi-worker production system
uses one `Playback` lane plus one shared `NonPlayback` lane. The Broker's work
classes and realtime-over-Still preemption keep exact Still work from taking
priority over active playhead dragging, while the physical two-lane bound avoids
opening independent scrub and Still hardware surface pools. Playback prefetch
cannot consume the interactive worker. Preview diagnostics expose the resolved
CPU budget and the actually started worker count so perf reports can distinguish
codec cost from scheduling over-subscription.
Preview completion has separate display and cache semantics. A decode result is
`Current` only when it still matches pending visible work; same-generation
results whose pending request was canceled or whose access mode has been
superseded may be `CacheOnly`, but must not wake the viewer as the current
frame or remove the newer pending request. Obsolete-generation results are
`Stale` and must not populate success/failure caches.
Queued current-frame work is latest-wins for playback and scrubbing. A Playback
Session publishes one active Frame Demand identity; all media keys belonging to
that demand remain valid for multi-layer composition, while synchronization to
a newer demand removes every older unstarted `Current/Playback` payload even
when the execution generation is unchanged. An older compatible in-flight
execution may finish only for decoder/cache locality and cannot publish a
current result or terminal failure. Generation pruning remains the broader
discontinuity boundary. Together these rules prevent old current jobs from
filling the bounded queue and dropping the visible current frame.
`RandomAccessStillFrame` is the lowest real-time current-frame class. It may
spend more time to produce deterministic still output, but it must not block
active playback or interactive scrubbing when the pending window or worker
transport queue is full. Scheduler admission and the job queue may evict queued
still-frame current work for `PlaybackCursor` or `ScrubCursor` current work;
they must not let still-frame work evict those real-time modes. On a shared
non-playback worker lane, `ScrubCursor` jobs are selected ahead of still-frame
jobs even when the still-frame request arrived first. Newly admitted scrub and
still requests retain their Interactive/Still/NonPlayback/Any lane affinity
instead of cold-opening another worker-owned hardware context on an
incompatible idle worker. This preserves pointer latency, exact-seek locality,
and realtime playback isolation without serializing same-generation work across
compatible shared lanes. A newly admitted
`PlaybackCursor` or `ScrubCursor` current request also preempts already-pending
still-frame current work before the pending window or worker transport queue is
full; the app must cancel matching queued jobs immediately so deterministic
still extraction cannot occupy decode capacity while realtime interaction is
waiting. If still-frame work is already running in a worker, the app must keep
its scheduler pending entry until the worker observes cancellation; otherwise
the decode would be mislabeled as obsolete instead of a realtime preemption. If
that running still-frame work sees a different realtime current-frame request
pending, the still-frame decode must cooperatively yield and report a structured
still-preempted-by-realtime-current cancellation. Another still-frame request
alone must not trigger that preemption.
If an existing queued prefetch for the same media key becomes current-frame
work, queue promotion refreshes the Broker binding's access mode, generation,
source timing, and authoritative request timestamp while retaining the physical
payload. `FrameWorkExecution::queue_wait` is derived from that latest binding at
dequeue; the media Adapter projects it into its execution evidence. Current
queue wait therefore starts at promotion, never at the earlier intentional
speculative enqueue.
The result pump records presentation-critical current queue wait only while the
Broker completion still owns the current presentation binding. A physical job
whose demand was already satisfied by an exact staged GPU frame remains in
aggregate and per-access-mode queue-pressure evidence, but cannot contaminate
the current-presentation latency gate after it has become cache-only.
Active Playback demand synchronization is an execution-entry responsibility,
not a Timeline resolver side effect. Every ordinary current GPU request and
every exact staged-frame-to-current binding synchronizes the Broker before
completion pumping or publication. Successor/lookahead requests never do. This
ordering prunes superseded unstarted Current work even when the picture is an
evaluation-cache hit and no Timeline or media interpretation runs.
Scheduler diagnostics keep aggregate skip/drop/stale counters plus reason
breakdowns for missing pending work, access-mode mismatch, obsolete generation,
obsolete request generation, and pending-window backpressure. Access-mode
failures must be diagnosable without inferring from one opaque skipped count.
When scheduler admission evicts prefetch or still-frame pending work to admit
real-time current-frame work, the admission result must return the evicted media
keys. The app layer must immediately cancel matching jobs from the worker
transport queue so already-obsolete work does not sit in the bounded queue until
a worker later discovers the missing pending request. Queue mutations that
remove jobs (`clear`, obsolete-generation pruning, and key cancellation) must
wake waiting lane workers just like enqueue, promotion, and close; otherwise a
worker can remain parked on a stale queue state until unrelated work arrives.
The decode performance summary/report carries the same scheduler diagnostics and
emits stable Scheduling root causes for access-mode mismatch, obsolete
generation churn, and pending-window backpressure.
The app worker transport queue is diagnosed separately from scheduler
admission. `queue_full_drops` and `worker_disconnected_drops` are hard failures
because they mean scheduler-accepted work did not reach a preview worker.
`queue_evicted_prefetch_jobs`, `queue_canceled_jobs`,
`queue_evicted_still_jobs`, `queue_pruned_obsolete_jobs`, and
`queue_promoted_current_jobs` are evidence fields: they should explain how the
system protected real-time current-frame work and kept the worker transport
queue aligned with scheduler cancellation, not be folded into opaque
backpressure. Diagnostics must also expose the current worker transport queue
depth split by priority and access mode (`queued_current_jobs`,
`queued_prefetch_jobs`, `queued_playback_cursor_jobs`,
`queued_scrub_cursor_jobs`, and `queued_random_access_still_jobs`) plus queued
expired work (`queued_expired_jobs`) and its playback-current subset
(`queued_expired_playback_current_jobs`) so a slow preview report can
distinguish active queue backlog, missed display deadlines, and codec/decode
cost without inspecting private queue internals. Expired queued work is an
active scheduling warning, not just passive evidence: dequeue must not dispatch
it into normal decode. The Broker emits one structured `DroppedExpired`
outcome, and the media Adapter maps the binding's priority and work class to the
appropriate cancellation vocabulary without re-evaluating the deadline;
unsupported combinations fail closed as unattributed cancellation evidence.
The worker result preserves this dequeue fact as the explicit
`MediaPreviewQueueDisposition::{Ready, Expired}` contract. It is independent
from cancellation phase: `Ready` means the Broker granted codec execution even
when that execution was later canceled or failed, while `Expired` means the
queued binding never entered codec work.
Diagnostics expose generic and playback-current queued totals plus cumulative
dropped totals; this preserves visibility for prefetch and still work without
misreporting every expiration as a missed playback presentation.
The app preview layer must also expose worker-lane eligibility for the same queued jobs
(`queued_playback_lane_eligible_jobs`, `queued_scrub_lane_eligible_jobs`,
`queued_still_lane_eligible_jobs`, and
`queued_non_playback_lane_eligible_jobs`) so reports can distinguish a backlog
that has an idle compatible lane from one waiting behind an occupied or missing
lane. The UI/report layer must consume these queue diagnostics rather than
recomputing lane acceptance from access-mode conditionals.
In-flight execution-lease residency is exposed in the same Broker diagnostic
snapshot and split by priority and access-mode contracts
(`in_flight_current_jobs`, `in_flight_prefetch_jobs`,
`in_flight_playback_cursor_jobs`, `in_flight_scrub_cursor_jobs`, and
`in_flight_random_access_still_jobs`) so diagnostics can separate queued backlog
from work already owned by playback, scrub, or exact still-frame workers. The
Broker records the selected lane when it creates the execution lease; the App
Adapter must not mirror this lifecycle with atomic activity counters. The same
snapshot includes worker-lane residency and
`in_flight_cross_lane_current_jobs`. In normal multi-lane operation cross-lane
current work is an invariant violation except for the Broker-authorized single
`Current + Playback` failover after cancellation. More than one such lease,
Playback Prefetch on NonPlayback, or any other incompatible cross-lane work
points at an Adapter mapping bug, not spare capacity that should be exploited
before codec, color, or GPU analysis.
An execution remains in flight after the worker sends its result and is released
only when the result consumer resolves freshness/terminal ownership (or the
execution is explicitly abandoned). Worker return alone is not lifecycle
completion and must not make residency evidence disappear early.
It also carries per-access-mode decode profiles for playback, scrub, and
random-access still requests: frame counts, cache/ring/source path counts,
end-to-end duration totals/maxima, worker-queue wait totals/maxima, seek counts,
session-local seek-index availability/use, decoded-frame pressure, and
stage-level timings. A slow preview report must identify the slowest access mode
so engineers can distinguish playback locality failures from scrub seek latency,
missing GOP/index evidence, queue-lane contention, or exact still-frame random
access costs.
Queue-wait latency gates and their p95 histograms consume only `Ready`
dispositions. Queue wait from `Expired` dispositions is retained in a separate,
per-access-mode profile with its own sample count, total, maximum, last value,
current/prefetch maxima, and histogram. The report validates histogram and
access-mode accounting for both populations, but never compares expired cleanup
latency to a codec-admission budget. Startup preroll remains a separate
cold-start population and contributes to neither steady-state profile.
Slowest-frame evidence must stay frame-local. `max_frame_stage_durations`,
`max_frame_queue_wait_us`, and `max_frame_bottleneck` are captured from the same
successful decode result; `queue_wait_max_us` remains an independent worker
pressure counter and must not be mixed into the slowest-frame bottleneck. This
prevents a codec-bound frame and an unrelated queued frame from being reported
as one impossible root cause.
Each access-mode profile must also carry compact fixed latency buckets for
successful decode duration and worker-queue wait. Reports derive a p95 upper
bound from those buckets and emit per-mode p95 checks. This is intentionally a
bounded diagnostic approximation, not an exact retained sample list: the JSONL
should show whether real-media stalls are sustained across most frames or just
single-frame spikes without growing unbounded UI telemetry state.
Completion freshness also gates terminal publication: a result may emit
playback Late/Failed only when it is broker `Current` and its captured identity
equals the Playback Engine's still-pending demand. `CacheOnly`, `Stale`, and
broker-current cleanup for a replaced/completed demand may be cached or
discarded according to their completion contract, but cannot refresh visible
state, create playback pressure, or publish another terminal delivery.
The versioned report must emit access-mode-specific latency checks and root
causes, so perf tooling can fail on `PlaybackCursor`, `ScrubCursor`, or
`RandomAccessStillFrame` regressions without reverse-engineering raw counters.
Perf smokes must gate queue-wait regressions for the access modes they exercise,
because queue-lane contention can make the viewer feel stuck even when codec
decode and color/render work are within budget.
Perf smokes that claim access-mode coverage must pass their required access
modes into the preview decode report builder. The report JSON must then include
`required_access_modes` plus pass/fail coverage checks for each required mode,
and the smoke validator should read those checks instead of reimplementing a
parallel coverage model. Playback-specific validators must surface missing
`PlaybackCursor` coverage as a first-class failure, not only as a generic failed
decode report. General UI diagnostics may leave the required list empty; an
idle or partial user session should not fail merely because it did not exercise
every access contract.
Media preview smoke validators must fail on both decode and post-decode render
report failures, and their error codes should include failing check/root-cause
codes before the generic failed-report code. A real sample whose UI cases fit
their broad wall-clock window can still be unacceptable if access-mode p95,
packet decode, or viewer output-boundary diagnostics exceed the frame budget.
The same computed failure-code arrays must be serialized in the smoke JSON so
CI dashboards and manual runs can inspect failures without parsing panic text.
App media preview smokes must generate real samples for both active
`ScrubCursor` playhead dragging and settled `RandomAccessStillFrame` requests;
coverage is incomplete if the report merely defines both profiles. A common
preview media smoke must fail when either access mode has zero successful
profile samples. App Preview Frame Store hits bypass media decode diagnostics
and therefore cannot fabricate access-mode coverage. Playback session-ring hits
remain valid mode-local Playback evidence because the ring is owned by that
exact decoder Session and is unavailable to Scrub or Still work.
Generated-fixture and external-real-media smokes must share the same
access-mode probe and validation helpers. The external path exists to run 4K
HEVC/HDR and camera-original samples through the exact same `ScrubCursor` and
`RandomAccessStillFrame` gates, not to create a looser ad hoc benchmark.
Playback diagnostics must also expose session reuse and forward reuse evidence.
If playback source decodes repeatedly open sessions or never hit forward reuse,
ring reuse, or cache reuse, the report should flag playback locality separately
from generic codec/GOP pressure.
Decode cancellation crosses the media boundary as a structured Adapter result,
but its authority and policy do not live in the App. Generation invalidation,
preemption, Broker close, and policy-authorized deadline cancellation arrive
from `FrameWorkBroker` as one atomic
disposition carrying the earliest applicable monotonic request instant and its
age. Broker closure follows the same rule: the first close instant is immutable
and `BrokerClosed` carries its age. Process worker-stop/join remains a
media-runtime Adapter concern, but its boolean flag cannot classify or timestamp
cancellation. One persistent observer per worker waits on that Broker lifecycle,
freezes `LogicalCancellationObserved`, and provides the boolean cooperative
predicate consumed by FFmpeg. The worker stamps completion and joins the
observer's decision before any result can publish. Before attempting to publish a worker result
through the bounded App channel, the Adapter stamps completion in the Broker;
queue backpressure or delayed UI polling therefore cannot change whether
execution met its deadline. Successful publication leaves freshness resolution
to the foreground pump. Shutdown abandons an unpublishable lease; a
residency-family transition instead resolves it as non-reusable before dropping
its native payload. When the worker returns, the Adapter contributes one
`FrameCancellationObservation` to the playback-owned collector: semantic work
class, structured cause, total execution lifetime,
worker-start-to-logical-cancellation, and request-to-logical-cancellation.

The Playback Module derives logical-cancellation-to-worker-return and owns exact all-run
aggregation for Playback, Interactive, Still, and their rollup. UI diagnostics
only project that immutable report into legacy decode fields and access-mode
views; they do not keep parallel counters or choose thresholds. The shared
fail-closed policy rejects unknown causes, missing logical-observation
attribution, impossible timestamp ordering,
request-to-logical-cancellation above 5 ms,
Playback/Interactive logical-cancellation-to-return above 50 ms, and Still
return above 500 ms. Concrete `PreviewDecodeCancellation` checkpoint/source
evidence remains an independent media fact and professional recovery gate; a
logical observation never claims that FFmpeg has stopped. Total worker lifetime remains diagnostic only:
expensive work completed before cancellation was requested is not evidence of
slow cancellation. This separation keeps authority propagation, codec
checkpoint placement, and cleanup/return independently diagnosable without
teaching the media layer UI intent.

The professional qualification consumes these as three distinct proofs:
authority-to-logical observation within 5 ms; bounded lifecycle recovery with
zero rejected old publications, one-or-fewer live failovers, latest-frame
presentation, and final Broker quiescence; and physical evidence consisting of
the 50 ms realtime worker-return bound plus a concrete media checkpoint whose
isolated-demux termination evidence, when present, is consistent.

The Broker obtains request ages, expiration, and completion timestamps from
playback's injected `MonotonicRuntimeClock`, with one sample per atomic
lifecycle operation. Immediately before admission the media Adapter pairs its
opaque absolute wall deadline with the remaining duration. The Broker lowers
that duration into its clock once; queueing does not renew it, and the App does
not compare or reconstruct it afterward. Queue expiry and completion
classification always retain this lowered deadline. The pending binding also
retains the typed in-flight expiry policy; rebinding the same in-flight key
replaces both with the latest effective binding, while a lower-priority
prefetch cannot downgrade an existing current request. The once-only worker
completion stamp prevents delayed UI polling from inventing lateness.
Broker clock regressions are clamped, counted by regression episode, projected
by UI diagnostics, and rejected by both decode-performance and professional
playback gates.
Every decoded-frame lookup uses the exact key first, then the media Session's
proven Decoded Presentation Extents after rational time has been lowered once to
the stream time base. Playback performance must come from Playback-cursor
decoder/Session locality, the byte-bounded ring, one bounded retained candidate,
hardware decode, GPU-resident delivery, and the exact Preview Frame Store—not
from treating a nearby timestamp as the requested frame.
Container PTS quantization is not itself temporal degradation. Exact Playback
and Still decode accept the causal frame whose half-open presentation extent
contains the lowered request; valid VFR/CFR interiors therefore need not equal
the frame's start PTS. A non-covering selection is a typed temporal mismatch,
not degraded output. Keyframe-only Scrub is the sole mode that may publish a
nearby non-covering selection, and it must retain Degraded presentation quality.

Current decode residency is intentionally explicit and fail-closed. CPU paths
produce `RgbaFrame`; admitted in-process FFmpeg D3D12VA or D3D11VA playback may
instead produce `PreviewNativeDecodedFrame`. Legacy unowned YUV preview surfaces remain
removed: native output is an owned decoder-resource lease, not a raw plane
container or a handle-kind diagnostic.
`PreviewHardwareDecodeDecision` records the media-layer selection for each
request. A GPU preference must resolve to a structured CPU RGBA reason such as
`CpuRgbaHardwareUnavailable`,
`CpuRgbaBackendUnavailable`, `CpuRgbaCodecUnsupported`,
`CpuRgbaBackendBoundary`, or `HardwareDecodeCpuTransfer` until the selected
decoder actually produces a native surface admitted by the caller's exact media
facts and device-scoped Renderer capability check.
`HardwareDecodeCpuTransfer` means in-process FFmpeg hardware decode was
configured and hardware frames are transferred back to CPU before RGBA preview;
it is active hardware decode, but it is not zero-copy, GPU texture residency, or
a native renderer import contract. `GpuResidentNative` is valid only when the
decoder probe reports active hardware decode, GPU texture residency, and a
native handle kind. Whether Renderer consumption is zero-copy or uses one GPU
bridge copy comes only from the device-scoped Renderer support contract.
Renderer readiness is a separate app
admission fact. External FFmpeg CPU RGBA
is always a backend boundary, even if the CLI used platform hwaccel internally.
`HwAccelProbe` reports the selected hardware backend, decoded frame residency,
`hardware_decode_active`, `zero_copy_active`, optional
`decoded_gpu_frame_handle_kind`, and a stable reason string. It also reports the
platform candidate backend list in priority order,
the candidate backend selected for the current stream plan, the candidate native
handle kind, and preferred native surface formats such as P010/NV12. Candidate
fields are planning evidence only. On Windows the ordered list must prefer
D3D12VA/ID3D12Resource before D3D11VA/ID3D11Texture2D, with legacy
DXVA2/IDirect3DSurface9 only as a lower-priority CPU-transfer fallback. If
FFmpeg, the codec, or the local device cannot use D3D12VA, playback admission
may fall through to D3D11VA and then DXVA2. On macOS the candidate is
VideoToolbox/CVPixelBuffer. On Linux the ordered list must prefer
VA-API/DRM-PRIME DMA-BUF surfaces before legacy VDPAU. Until D3D12VA/D3D11VA,
VideoToolbox, VA-API, VDPAU, DXVA2, or CUDA/NVDEC hardware frames are actually
exported through a concrete media adapter and imported through the renderer
native decoded-frame import contract,
`HwAccelBackend::probe()` must keep `selected_backend=None`,
`decoder_adapter_available=false`, `hardware_decode_active=false`,
`zero_copy_active=false`, `DecodedFrameResidency::CpuRgba`, no active GPU
handle kind. Platform preference alone is
not a valid hardware decode signal. The media Module now has concrete retained
resource Adapters for FFmpeg `AV_PIX_FMT_D3D12`/preferred `AV_PIX_FMT_D3D11`,
VideoToolbox `AV_PIX_FMT_VIDEOTOOLBOX`, and VA-API `AV_PIX_FMT_VAAPI`. The first
borrows the documented D3D resource ABI, VideoToolbox validates the retained
`CVPixelBufferRef`, and VA-API performs one cached read-only mapping to a fully
validated DRM PRIME descriptor. Legacy `AV_PIX_FMT_D3D11VA_VLD`, DXVA2, VDPAU,
and CUDA do not become native automatically; GPU-resident planning skips any
backend without a matching concrete media Adapter on the current OS.
For a concrete video stream, media may also run a read-only FFmpeg hardware
codec config probe with `avcodec_get_hw_config`. That probe records whether the
linked FFmpeg build lists the candidate hardware device type, whether the
stream codec has a decoder, whether that decoder advertises a matching hardware
config, the advertised hardware pixel format, and the setup methods
(`hw_device_ctx`, `hw_frames_ctx`, `internal`, or `ad_hoc`). This probe must not
create an `AVHWDeviceContext`, change decoder format negotiation, allocate
hardware frames, or report active hardware decode. Its purpose is to separate
"FFmpeg/codec cannot use this backend" from "Mondrian has not connected the
decoder adapter yet".
The codec-wide FFmpeg table is not sufficient profile evidence. Before device
creation, the Session rejects H.264 High 10, High 4:2:2, and High 4:4:4 profile
families from hardware admission because Mondrian has no cross-platform native
or CPU-transfer contract that can rely on those device outputs. Preferred
hardware requests proceed directly through the software decoder;
`RequireGpuResident` remains explicitly unsupported. This avoids repeatedly
opening a device-backed decoder that will negotiate a software frame while
preserving exact CPU fallback semantics.
Playback sessions acquire an immutable device-root lease from their injected
`PreviewDecodeWorkerResources::HwDeviceContextPool`, keyed by exact hardware
backend and renderer-selected adapter. The pool owns only its configured idle
generations; every unopened codec receives its own thread-safe `AVBufferRef`
before `avcodec_open2`. Attach, codec-open, or active hardware-execution
failure retires that exact generation from future acquisition while existing
`Arc` leases finish safely. Codec context,
DPB, decoder-created `AVHWFramesContext`, frame pool, and decoded surfaces are
never cached at device scope: they remain session-owned and must retire at the
Playback/Interactive family barrier. Sharing the device removes driver device
teardown/recreation from a transport discontinuity without allowing two native
surface pools or sharing codec state. Pressure or an idle-family boundary may
release idle roots, and the next acquisition creates a later generation. A
device-loss recovery path must retire the failed generation and create a new
immutable device; it must not mutate an initialized context or reuse one across
adapter selectors. The
session also installs a get-format callback that accepts only the advertised
hardware pixel format. `PreferHardwareDecode` always permits materializing
hardware frames through `av_hwframe_transfer_data` into CPU frames before RGBA
scaling.
Static compatibility produces an ordered candidate list, but the decode
Session owns actual selection: acquire/attach/open failure retires and records
that backend generation, applies a bounded owner-local retry delay, and tries
the next compatible backend. `PreferHardwareDecode` and
`PreferGpuResident` fall back to software only after that ordered list is
exhausted; `RequireGpuResident` fails closed. Retry delay expires naturally and
can be invalidated by an explicit adapter/device-generation change, so a
transient D3D12 failure neither hammers the driver nor permanently hides a
working D3D11 path.
`PreferGpuResident` permits the same diagnosed fallback if native
materialization fails. `RequireGpuResident` configures the hardware decoder but
does not permit CPU transfer or software-frame fallback. A failed device-context
probe is `CpuRgbaHardwareUnavailable`; a
successful hardware decode session that still transfers frames to CPU is
`HardwareDecodeCpuTransfer`; a future zero-copy adapter that cannot import into
the renderer should use Renderer import diagnostics instead of this
CPU-transfer state.
The temporary FFmpeg hardware CPU-transfer fallback must report a structured
`PreviewHardwareDecodeCpuTransferStatus`: `NotAttempted`,
`ConfiguredAwaitingFrame`, `SetupFailed`, `DecoderOpenFailed`, or `Observed`.
Setup/open failures must not disappear into trace logs or generic software
decode counters. They remain a diagnostic fallback state only; `Observed` means
hardware frames were transferred back to CPU, not that Mondrian achieved
GPU-resident playback.
Playback hardware-decode admission is an app-layer aggregation contract, not a
media or Renderer responsibility. UI-independent `app::native_video_import`
combines exact media-surface facts and the active device's Renderer native
decoded-frame import support into one
`PlaybackHardwareDecodeAdmission`; Window and Headless composition roots pass
that same value to the concrete Preview Adapter. Preview diagnostics project,
but do not own, its playback request, renderer readiness and supported handle/
source-format counts, and stable blockers `SupportUnknown`,
`HandleSupportMissing`, or `SourceTextureFormatSupportMissing`. The scheduler may
request `PreferGpuResident` only when the active Renderer device is ready for
the exact media surface family and sampling format. Otherwise playback may request
`PreferHardwareDecode` to use FFmpeg hardware decode with CPU-transfer fallback,
and the performance report must still identify the precise native-import
admission blocker. Do not report FFmpeg hardware CPU-transfer as the production
GPU-resident playback path.

Preview path resolution is proxy-aware but does not synchronously generate
proxy media. `mondrian-media::ProxyGenerator` owns the shared proxy freshness
contract through `ProxyStatus` (`Missing`, `Fresh`, `Stale`). If project proxy
playback is enabled for an asset and the expected proxy file is `Fresh`, app
preview decodes that proxy path. If the proxy is `Missing` or `Stale`, preview
falls back to the source path and records proxy hit/miss/stale counters in
`PreviewDiagnostics`. Export continues to use the source/export contract;
proxy selection is a preview playback scheduling decision, not media color
interpretation.
Alpha-bearing sources bypass the current opaque proxy profiles and GPU-native
YUV preview surfaces. They decode from the source into the typed RGBA input
boundary until a proxy/native format has an explicit alpha-capable contract;
the app must not generate or reuse an opaque proxy for such an asset. Hardware
decode preference is downgraded to the CPU RGBA path for these requests, and an
unexpected opaque native surface fails closed instead of silently discarding
coverage.
An unmapped or internally inconsistent persisted pixel format has the same
conservative execution boundary: `VideoStreamInfo::proven_sampling()` is the
only authority for source precision, Alpha absence, and native-surface format.
Without that evidence Preview blocks before decode and refuses proxy generation
or reuse. Continuing through the ordinary CPU RGBA path would still be unsafe:
that path currently materializes non-scene-linear inputs at an 8-bit boundary,
so it cannot prove preservation of an unknown source precision. The serialized
fallback fields remain storage compatibility only and cannot silently authorize
an 8-bit opaque path.
Newly imported video assets enter proxy playback and start background proxy
generation only when the project `ProjectSettings.proxy_enabled` policy is on.
The project policy is the scheduling source of truth; app/UI preferences must
not independently enable proxy generation against a project that has disabled
proxy workflows.
Proxy generation and preview proxy-path resolution must use the same
project-derived `ProxyConfig`: `ProjectSettings.proxy_resolution` selects the
proxy height preset, and `ProjectSettings.cache_dir` places proxy media under
that cache root's `proxy/` directory when configured. Callers must not use
`ProxyConfig::default()` for project media scheduling because that would split
generation and playback lookup across different cache roots or resolutions.
The app admission seam freezes that cache root as a normalized absolute path
before it enters request identity or per-root resource accounting. Media path
planning applies the same normalization for non-service callers. Relative
working-directory state is never part of a proxy artifact identity.
They must also use the same versioned `ProxyColorContract`. The app resolves
that contract from asset interpretation, ingest detection, and the active
missing-metadata policy before it asks media code to locate or generate a
proxy. A rejected input-color decision rejects proxy generation; it must not
be replaced with an implicit sRGB or Rec.709 assumption.
Proxy artifacts are source-referred optimized media. Their identity includes
the effective source color space, source precision, encoded range, spatial
settings, quality setting, and concrete encoding profile, but excludes
timeline working space and monitor/display transforms. Every completed proxy
has a versioned `.color.json` sidecar containing that contract and an exact
source file fingerprint. `ProxyStatus::Fresh` requires both the proxy and an
exactly matching, parseable sidecar; file modification ordering alone is not
proof of color or source identity.
The artifact path also includes the canonical source-path hash. Every observable
physical source is canonicalized before hashing, so lexical, absolute, and
Windows verbatim aliases of the same source select one artifact and manifest.
The versioned hash domain consumes native path units without lossy Unicode
projection: Unix hashes `OsStr` bytes and Windows hashes UTF-16 code units in
fixed little-endian order. Unsupported targets fail closed for non-Unicode
paths instead of aliasing distinct files.
Relinking one
stable `AssetId` to another path therefore cannot reuse the prior path's proxy,
even when the replacement bytes happen to have the same file fingerprint. The
Asset Library remains the source-path authority and Timeline Clips retain only
the stable Asset identity; Relink is an Asset Library transaction, not a
Sequence edit. Preview re-resolves the live record, observes a missing
replacement proxy, and may request a fresh artifact through the same service.
No UI-owned cache invalidation list is required for correctness.
The Golden Proxy/Relink Adapter exercises that production boundary inside the
shared Hero Sequence. It owns one dedicated video Track and two adjacent
placements: CFR H.264 in `200..350` and attested VFR H.264 in `350..500`.
Both imports cross the bounded worker and publish fresh proxies. The CFR source
then proves Proxy→Original→Proxy, asynchronous two-phase offline Relink,
replacement-source proxy generation, exact Asset Library revision, and reload
notification. The report captures both Clip/Asset anchors and the Relink
operation/generation/terminal evidence. Later Recovery and durable reopen may
add unrelated Hero authoring, but they must retain both placements, the
relinked CFR record, project/asset proxy intent, and distinct replacement proxy
identity. These generated code patterns are codec/proxy/time evidence, not
independent Rec.709 color references.
`ProxyCodec::Auto` selects H.264 High 8-bit only for ordinary 8-bit SDR and
selects H.265 Main10 for HDR, camera-log, or greater-than-8-bit sources.
Explicit H.264 requests for high-precision sources fail closed. H.265 Main10
and DNxHR HQX preserve a 10-bit proxy boundary, while standardized source
spaces emit canonical FFmpeg primaries/transfer/matrix tags. Camera-log spaces
do not receive guessed delivery tags: their sidecar contract remains
authoritative. Unknown source range remains an explicit `Unknown` contract
at ingest, but it is not a valid proxy-generation contract. `ProxyColorContract`
is valid by construction only for explicit `Limited` or `Full` range and a
supported 8-16 bit source precision; deserialized contracts are revalidated at
every generator entry point. Proxy generation therefore fails closed instead
of allowing FFmpeg to infer range.
Ingest persists decoder `color_range()` on `VideoStreamInfo`, so ordinary
probed assets propagate `Limited` or `Full` into the proxy contract unless an
asset range override replaces that probe fact. Because range is part of the
proxy contract and sidecar identity, changing the override selects a distinct
proxy path and invalidates reuse of the old artifact. `Unknown`
is reserved for genuinely unspecified metadata, decoder-unavailable records,
and older serialized asset records loaded through the explicit serde default.
The FFmpeg proxy filter graph declares frame metadata with `setparams`, then
uses matching `scale` `in_range`/`out_range` values. The encoded output also
carries an explicit `color_range` plus canonical CICP tags when the source
identity has trustworthy standardized tags. Contract/manifest v2 invalidates
older artifacts generated under implicit range behavior.
Generation must also use `ProxyStatus`: a `Fresh` proxy is reused, while a
`Stale` proxy is regenerated in the background. Failed regeneration must not
delete the previous proxy file, because preview can keep falling back to source
until a fresh proxy is finalized.
`mondrian-storage` is the only proxy/manifest publication implementation. It
allocates a unique direct sibling and records its filesystem object identity,
then releases only the handle while FFmpeg writes that exact reserved object.
After FFmpeg exits, the reservation must reclaim and revalidate the same object
before atomic `ReplaceExisting` publication. Cancellation and pre-namespace
failure may remove only a path that still names that recorded object;
durability-unconfirmed or namespace-indeterminate outcomes preserve all
possible artifacts. Media converts storage errors into the exhaustive
`ProxyPublicationFailureKind` and records the failing `Media` or `Manifest`
phase without exporting storage implementation types. Only
`BeforeNamespace` is eligible for an ordinary retry. `DurabilityUnconfirmed`
and `NamespaceIndeterminate` enter quarantine: automatic and explicit demand
remain suppressed until exact media-plus-manifest freshness revalidation
proves the intended artifact pair. This physical namespace quarantine survives
an App Project-generation rotation; rebinding semantic authority cannot erase
uncertain filesystem state. No diagnostic may describe those states as normally
retryable. The former
rename-old-to-backup/restore sequence and path-only cleanup are forbidden.
The staging suffix is not a valid container extension, so the concrete
`ProxyEncodingProfile` pins both codec/pixel format and muxer (`mp4` for
H.264/HEVC, `mov` for DNxHR), and the command passes that muxer explicitly.
Inferring the container from the temporary file name is forbidden; it can fail
before encoding and would make atomic publication platform/tool-version
dependent.

DNxHR proxy generation and professional Export are separate Modules even
though both use FFmpeg's `dnxhd` Adapter. The packaged runtime now qualifies
all five DNxHR profile tokens plus 10-bit 4:2:2/RGB pixel formats, libx264's
`avcintra-class` and `yuv422p10le`, and the `v210`/`r210` encoders. Registry
presence alone remains insufficient: Export owns real encode/re-probe tests for
every advertised professional row. Media re-import reads the raw FFmpeg profile
integer that the wrapper's generic `Profile` enum omits, classifies DNxHR as
`VideoCodec::DnxHr` instead of DNxHD, preserves exact ProRes variants, and maps
RAWVIDEO/v210/r210 to `VideoCodec::Raw`. This codec identity is evidence for
asset diagnostics and future conservative reuse; it does not by itself grant
Smart Render or vendor-format conformance.

The current FFmpeg MXF muxer can carry H.264 essence but exposes no
XAVC-specific authoring contract. Consequently Media and Export must not infer
XAVC from an H.264 profile, filename, MXF container, or arbitrary codec tag.
External NLE/vendor conformance remains a separate HITL qualification even for
the exact DNxHR and AVC-Intra paths implemented here.

`ProxyConfig.concurrent_jobs` is an execution contract, not a UI preference.
`app::proxy_generation::ProxyGenerationService` acquires cache-root capacity
before an attempt leaves Queued and enters Running; workers therefore cannot
hide a batch of imports behind the media limiter and block a later explicit
user request. `mondrian-media` retains its per-cache-root limiter as a
cross-caller resource safety valve before FFmpeg launch. Fresh proxy reuse
consumes neither application nor media transcode capacity.

The App service is owned by `AppState` and starts workers lazily; it is not a
process-global singleton. Its exact key includes asset, source path and live
file fingerprint, artifact-affecting config, and versioned color contract while
excluding the non-semantic concurrency count. It admits at most 512 attempts,
deduplicates exact work, promotes either Queued or Running work when a
higher-rank Playback Recovery or User request names the same artifact, and
schedules User, Playback-recovery, and Import queues in that order with a forced
Import turn after eight foreground attempts. Up to 256 exact failures suppress
automatic retry storms. An explicit user request clears an ordinary
non-publication failure, while a proven pre-namespace publication failure is
eligible for exact automatic retry. Unknown publication states are never
cleared by priority promotion. Terminal evidence is bounded to 512 attempts
and records publication sequence, origin, generation, priority, source
fingerprint, elapsed time, disposition, durable media/manifest publication
evidence, or the exact typed publication failure phase and kind.

Resource-policy yield is distinct from Project cancellation and retry. Closing
global dispatch asks every Running attempt to yield; closing only automatic
dispatch selects `Import` and `PlaybackRecovery`, never `User`. Only an exact
current attempt that returns `Canceled` while that yield is still requested may
return to Queued. It keeps the same attempt ID, request, current origin, and
generation, receives a fresh token and queue revision, and produces neither
terminal cancellation evidence nor failure-memory state. A higher-rank exact
request may promote either Queued or Running work; if a yielding automatic
attempt becomes `User`, it requeues and resumes as User after cancellation is
acknowledged. If FFmpeg has already crossed its final cancellation check and
publishes successfully, completion wins; the service must not duplicate that
artifact or requeue a second execution. This lets realtime work reclaim
resources while preserving one user-visible Proxy intent.

Terminal evidence has a monotonic publication sequence separate from attempt
identity. The App event-loop Adapter consumes a strict sequence delta, so a
policy-only revision cannot replay the latest retained failure and out-of-order
worker completion cannot make a terminal record invisible. It projects a
failure into user-visible status only when the record still belongs to the
delta snapshot's current Project execution generation; older generations
remain available solely as bounded diagnostic evidence.

Opening, creating, closing, or replacing a project rotates the service
generation. Queued attempts terminate immediately as Canceled; running attempts
receive the same monotonic `ExecutionCancellationToken`. Media observes that
token while waiting for its cache-root safety permit, every 10 ms while FFmpeg
runs, and immediately before artifact publication. Proxy FFmpeg uses the shared
media process supervisor, so stdout is continuously drained without retention,
stderr is continuously drained into a bounded 64 KiB tail, and cancellation
performs kill → wait → pipe join before partial-output cleanup. It returns
Canceled rather than poisoning failure memory.
Only an exact current-generation attempt may publish success into service
evidence. UI/event-loop code never joins a proxy worker or child process.
`MultiLevelCache` must not weaken this contract: L1 memory hits and L2 proxy
index hits are valid only while the referenced proxy still resolves to
`ProxyStatus::Fresh`. A cached source fallback must be re-evaluated when a
fresh proxy later appears so proxy generation can actually improve playback
without requiring an app restart or manual cache clear.

`DecodedGpuFrameHandleKind` belongs to media because it describes the decoder
surface family that FFmpeg/hardware decode produced, such as D3D12 resource,
D3D11 texture, legacy DXVA2 surface, CVPixelBuffer, VA-API surface, legacy
VDPAU surface, or CUDA device memory. It does not imply that the renderer can
import or sample that handle. Device-scoped readiness is reported only by
`mondrian-renderer` through
`GpuNativeDecodedFrameImportSupport` / `GpuNativeDecodedFrameImportPlan`. Ready
support also declares `GpuNativeDecodedFrameImportMode::{ZeroCopy,
GpuBridgeCopy}`; missing transfer mode fails closed. App code must not infer
zero-copy playback from the media handle kind or generic import readiness alone.
An independent platform probe is forbidden because it can bind a different
physical device and cannot prove allocation or synchronization compatibility.
On macOS the Renderer admits CVPixelBuffer only when the active wgpu device is
Metal and a
`CVMetalTextureCache` can be created for that exact device. On Linux the media
Adapter exposes DMA-BUF while Renderer admission requires the active
Vulkan device's `VULKAN_EXTERNAL_MEMORY_DMA_BUF` feature. On Windows the same
rule binds D3D12 resource import to the active DX12 device. CPU-transfer hardware
decode still reports CPU RGBA residency on every platform.

The Windows resident HEVC export boundary follows the same exact-device rule in
the opposite direction. `ResidentHevcEncoderSession` owns the in-process
FFmpeg `hevc_d3d12va` codec, muxer, D3D12 hardware-device reference, and bounded
NV12/P010 hardware-frame pool. An acquired
`D3D12ResidentEncodeInputFrame` is not submit-ready: only the Renderer production
Adapter may turn it into `D3D12ResidentEncodeReadyFrame` after enqueueing the
producer fence signal. Media waits on that fence through FFmpeg's hardware-frame
contract and never maps the surface or stages raw pixels through host memory.
The hand-written FFmpeg 7.1 D3D12 ABI declarations are target-gated and guarded
by compile-time size and offset assertions.

Packet submission and drain distinguish EAGAIN, EOF, and real errors; flush
must reach EOF, and any send failure waits for the producer fence before the
surface can be released. The session writes a video-only mux artifact because
the final Export process may still need to combine normal audio. That final
process uses video stream copy, not rawvideo input, so it is not an
encoder-upload boundary. Resident route diagnostics count surfaces, packets,
and producer-ready submissions while explicitly retaining zero readback,
rawvideo, and CPU upload counters. The current in-process codec call cannot be
forcibly isolated from a wedged vendor driver; bounded surface acquisition and
queue-fence waits do not constitute process-level hang isolation.

The preview decoder's experimental external-process path is named
`PreviewDecodeBackend::ExternalFfmpegCpuRgba` and is enabled only by explicitly
selecting that backend or setting `MONDRIAN_PREVIEW_EXTERNAL_FFMPEG_CPU_RGBA`.
It may ask the `ffmpeg` CLI for platform hwaccel, but its contract is still
`rawvideo` RGBA over stdout, so it is CPU-resident and cannot be reported as
Mondrian hardware decode, zero-copy, low-copy texture residency, or GPU frame
delivery. The previous "GPU assist" terminology is intentionally not used.
This path remains limited to `RandomAccessStillFrame` requests. It runs the
child with independently drained stdout/stderr pipes, polls the request probe,
and kills, waits for, and joins pipe readers on cancellation; a 4K rawvideo pipe
therefore cannot hide an unbounded child-process wait. `PlaybackCursor` and
`ScrubCursor` stay on in-process decode/session paths for locality and native
residency rather than using the process boundary as a realtime shortcut. The
rawvideo protocol currently carries no selected PTS/duration evidence, so this
experimental Adapter cannot publish a successful exact frame. Its completed
raster is dropped and execution continues through the in-process exact Session;
the external duration remains diagnostic evidence instead of becoming a user-visible
temporal failure or a guessed success.
Every frame returned by the preview decode boundary is a
`PreviewDecodeOutcome`: `Frame(RgbaFrame)` for CPU encoded RGBA8 payloads,
`FloatFrame(FloatRgbaFrame)` for CPU scene-linear RGBA f32 payloads, or
`NativeGpuFrame(PreviewNativeDecodedFrame)` for GPU-resident decoder payloads.
Native FFmpeg results use `PreviewDecodePath::InProcessFfmpegNative`; they are
never labeled as the in-process CPU RGBA path.

Scene-linear FFmpeg output must not pass through swscale's RGBA8 boundary.
`GBRPF32LE/BE` and `GBRAPF32LE/BE` frames are unpacked directly from their
declared planar byte order into interleaved `FloatRgbaFrame` storage. Negative
values, values above one, and straight alpha are preserved. Preview resize uses
the float path, and both the playback ring and Preview Frame Store retain the
payload kind without quantization. Unsupported scene-linear decoder formats
fail closed instead of silently quantizing. App preview, thumbnails, and export
route this outcome through the renderer's `LinearFloatSource` input contract.

`mondrian_media::preview::frame_contract` is the single media-layer Adapter for
turning FFmpeg pixel format, matrix, range, chroma location, and bit depth into
Mondrian's decoded-frame facts and applied CPU RGBA conversion contract. It also
configures the exact swscale matrix/range; implicit FFmpeg defaults are not an
allowed fallback. In-process decode and the external still-frame Adapter call
the same resolver. Decode Session state, resizing, pixel copying, hardware-plan
selection, and renderer color interpretation remain outside this Module, so
the color seam is narrow without creating a second frame object model.

`DecodedVideoSurfaceDescriptor` is the canonical physical surface vocabulary.
It models RGB versus YCbCr, 4:2:0/4:2:2/4:4:4, semi-planar/planar/packed layout,
UNORM or float encoding and code alignment, component bit depth, alpha, and
native-payload eligibility. FFmpeg pixel and hardware-frame `sw_format`
Adapters lower NV12; P010/P012/P016; P210/P212/P216; P410/P412/P416;
Y210/Y212; XV30/XV36; BGRA/RGBA; and RGBA16F/RGBA32F into that vocabulary.
Backend preference matrices remain backend-specific: the linked D3D12VA
contract is NV12/P010-only; D3D11VA exposes its actual DXGI-backed subset;
VideoToolbox exposes its available bi-planar families; VA-API/CUDA candidates
remain planning evidence. A shared descriptor never promotes a candidate to a
renderer route.

`mondrian_media::preview::frame_materialization` owns the next execution
boundary. It consumes a decoded FFmpeg frame plus the prepared hardware plan
and produces exactly one CPU RGBA8, CPU scene-linear float, compact CPU YUV, or
native-resource payload. Hardware-to-CPU transfer, native fallback
classification, float-plane unpacking/resizing, swscale execution, row
retention/copying, scaler reuse, and their
stage timings remain inside this Module. The decode Session owns input, seek,
demux, codec continuity, and candidate selection only; it cannot implement a
second pixel-output path. Conversely, materialization cannot seek, read packets,
or decide request scheduling.

`PreviewNativeDecodedFrame` must carry a
`PreviewNativeDecodedFrameHandle` minted by the media backend that owns the
native decoder resource. The handle is a shared lease over an
`Arc<dyn PreviewNativeDecodedFrameResource>`; cloning a frame retains the
backend resource, and dropping the final clone releases it through the concrete
resource implementation. Handles are neither `Copy` nor serializable. Their
process-local kind/id values are diagnostics and backend-routing evidence, not
OS handles or resource ownership by themselves. Equality and hashing use the
lease object identity so recycled diagnostic ids cannot alias live resources.
`mondrian_media::preview::native_frame` is the sole owner of this resource
contract: native payload validation, type-erased lease identity, FFmpeg frame
retention/release, and the D3D11/D3D12 borrowed ABI views live together behind
that deep Module. The parent Preview decoder may choose a hardware plan and
materialize a decoded frame, while renderer code may consume the public handle;
neither may reach into the retained `AVFrame` or duplicate platform ABI parsing.
The stable public Interface remains re-exported from
`mondrian_media::preview`, so this ownership split does not create a second
native-frame object model.
The in-process FFmpeg lease is `FfmpegNativeDecodedFrameResource`. It retains
the decoder frame with `av_frame_clone`, thereby retaining the frame's
`AVBufferRef`-owned hardware surface, and releases that reference with
`av_frame_free` when the final lease drops. D3D12 residency reads FFmpeg's
documented `AVD3D12VAFrame` descriptor from `AVFrame::data[0]`: its borrowed
`ID3D12Resource`, decode-completion `ID3D12Fence`, and fence value remain owned
by the retained AVFrame. A renderer must wait for that value before reading the
resource; media never performs a CPU fence wait. Preferred D3D11 residency
reads only FFmpeg's documented `AV_PIX_FMT_D3D11` ABI: `AVFrame::data[0]` is the borrowed
`ID3D11Texture2D` pointer and `data[1]` is the array-texture slice. Legacy
`AV_PIX_FMT_D3D11VA_VLD`, missing texture pointers, and slice-width overflow
must fail with structured resource errors rather than being reinterpreted as
the preferred layout. Establishing this resource ownership contract does not
by itself report active hardware decode, native output, or zero-copy; those
states remain observed-result facts.
Forward frame selection must retain before/after FFmpeg candidates with
`av_frame_clone` as well. Do not use `ffmpeg-next::Video::clone()` for decoder
candidates: that wrapper allocates an image frame and invokes pixel-copy APIs,
which is neither the correct hardware-surface lifetime operation nor a checked
failure boundary. The media-internal retained candidate type owns the cloned
`AVFrame` until selection/conversion finishes.
A handle-kind-only payload is invalid because it can masquerade as GPU
residency without an importable resource. Native payloads are limited to the
descriptor-qualified families above; unknown and ordinary CPU planar formats
fail closed before reaching the app or renderer. They must also carry
`DecodedVideoSampling` at construction time: range must be explicit, bit depth
must match the native surface contract, and subsampled YCbCr payloads must have
explicit chroma location. 4:4:4 permits unspecified chroma location because no
subsampled grid origin exists. This remains
media payload evidence, not color interpretation; unsupported-but-explicit
chroma siting can be rejected later by the app/renderer admission boundary, but
missing sampling facts must not escape the media native-frame constructor.
For D3D12 and preferred D3D11 frames, media reads `AVFrame::hw_frames_ctx` and
accepts only explicit `AV_PIX_FMT_NV12` or `AV_PIX_FMT_P010LE` software layouts. Planar
`AV_PIX_FMT_YUV420P` / `AV_PIX_FMT_YUV420P10LE` descriptions are not silently
reinterpreted as two-plane GPU textures. A GPU-preferred CPU fallback records
`PreviewNativeDecodeFallback` as `SoftwareFrame`,
`ResourceAdapterUnavailable`, `SurfaceFormatUnavailable`,
`SamplingMetadataIncomplete`, or `ResourceRetentionFailed`. A required-GPU
request returns a decode error for the same condition instead.
Hardware-preferred Session setup already walks compatible backends before
software. If a preferred Session later fails during packet send, frame receive,
seek, or materialization, the worker retires the poisoned codec/device state
and retries the same semantic request once through a fresh software Session.
Success reports `RuntimeHardwareFailure`, and the exact source revision/stream
is quarantined from another hardware attempt for 30 seconds so every frame does
not repeat the same driver failure. `RequireGpuResident` never takes this path.
Sources without a codec/profile-qualified native hint do not attempt
GPU-resident admission. App intersects that hint with the exact route matrix
published by the active renderer generation; a high-bit hint unsupported by
that device/backend remains hardware-decode CPU-transfer eligible but cannot
request GPU residency.
On multi-adapter Windows systems, a native-import admission attaches the typed
`D3D12VaAdapterIndex` selector derived from the renderer's physical DXGI
adapter. The active DX12 Renderer creates an FFmpeg D3D12VA device root over
its exact `ID3D12Device`; App installs that immutable root into the Preview
worker-family device pool before publishing `PreferGpuResident`. Codec Sessions
receive ordinary FFmpeg `AVBufferRef` leases, never Renderer or OS handles.
The selector remains part of Session identity and prevents a backend-family
mismatch, while D3D11VA remains a safe hardware-decode CPU-transfer fallback
when no renderer-qualified root is installed. Worker-family roots and retry
state are keyed by backend plus selector; replacing a root advances the pool
generation without revoking active leases. Existing Sessions refuse reuse once
their generation is no longer current. App simultaneously cancels all old
decode bindings and removes decoder-resource Frame Store/evaluation entries,
so a late result cannot re-enter the new device generation as cache-only data.
The Renderer finally requires exact D3D12 device identity and adapter LUID for
every decoded resource.
GPU-resident decoder setup reserves thirty-two FFmpeg `extra_hw_frames` before
`avcodec_open2` because native frames remain leased after the receive call.
This is requested decoder-pool headroom, not a portable guarantee of how many
surfaces every codec/driver combination can make concurrently available and
not application cache capacity; CPU-transfer
decode leaves the setting at zero because it exports no hardware surfaces. The
external-lease proof is deliberately conservative and bounded: eight queued
App completions plus at most two worker-held publishers, eight Preview Frame
Store resource units, four renderer-completion leases, and two
selector/transient owners. The direct-import Renderer owns zero duplicate YUV
bridge textures. These are ceilings, not expected steady-state occupancy;
changing any ceiling requires revalidating the thirty-two-frame decoder reserve
instead of silently adding another native-frame holder.
GPU-resident requests bypass the session-local RGBA playback ring. Native
decoder surfaces are not inserted into any media-owned CPU cache. During
reverse traversal only, up to four decoder references may be held by the
byte-bounded GOP replay window; this ownership is released before the
decoder/device. Native outputs may enter
the App's playback-owned Preview Frame Store as opaque leases charged one
decoder-resource unit each; the active product resource decision sets that
optional budget independently of the eight-frame temporal prefetch ceiling.
Prefetch planning consumes the Store-produced physical headroom; Current
execution is additionally bounded by its exact demand and the global aggregate
hard grant. This permits useful forward residency without treating a
zero-host-byte surface as free or allowing multiple demands, duplicate
attempts, or external clones to exhaust the decoder pool. CPU fallback payloads
use the same ledger's exact host-byte charge. The shared future-prefix planner
also preserves accepted native resident leases until all nearer missing work
has transferred into Broker ownership; decoder-surface LRU policy therefore
 cannot invert timeline priority.
The App evaluation working set retains only Store-protected frame clones. Every
decoder-family, media-only, and all-residency retirement first drops matching
evaluation entries, then clears the Frame Store, preventing a native lease
from becoming an unaccounted owner after Store eviction.
CPU consumers such as thumbnails and current RGBA fallback paths must explicitly
match `Frame(RgbaFrame)` and fail closed on `NativeGpuFrame`; they must not
reinterpret a native decoder surface as RGBA or silently force a CPU transfer.
The app viewer preview path preserves `NativeGpuFrame` as a native source
payload and passes the complete frame, including its opaque handle token and
residency/sampling facts, into GPU preview admission. App adapters must not
flatten that payload into diagnostics and discard the token. On Windows DX12,
admitted D3D12VA NV12/P010 resources are allocated by the exact Renderer device,
adopted directly by wgpu, and sampled without a bridge texture or pixel copy.
The Renderer queue waits on FFmpeg's decode fence, performs explicit
`COMMON -> shader resource -> COMMON` transitions, and retains the Media frame
lease plus command allocators until its completion fence proves the final read.
On macOS, the Metal Adapter validates NV12/P010 plus qualified
P210/P216/P410/P416 CVPixelBuffer FourCC/plane extents and retains each
`CVMetalTexture` and Media frame lease through GPU submission completion. On Linux, the
Vulkan Adapter accepts typed one-layer NV12/P010/P012 DRM PRIME descriptors
with exactly two bounds-checked planes, duplicates each selected object FD for
Vulkan ownership, preserves offset/pitch/modifier, and retains the Media frame
lease through GPU submission completion; other DRM layouts fail
closed to the declared fallback instead of being reinterpreted. All three enter
the same renderer-owned YUV sampling and source-to-working OCIO execution
Module. Other native handle families remain renderer-readiness blockers; they
are not media decode failures or implicit CPU fallback frames.
The renderer owns the fallible mapping from media `DecodedVideoSurfaceFormat`
to its native texture-format contract and implements its source-descriptor
trait for `PreviewNativeDecodedFrame`. App readiness code delegates to that
mapping instead of copying renderer format policy back into the media/app
layers.
When native payloads reach the renderer import contract, renderer-side video
sampling metadata is mandatory. Every 8/10/12/16-bit 4:2:0/4:2:2/4:4:4 or RGB
identity retains its distinct surface format rather than overloading P010. The
app/readiness layer must pass explicit
limited/full range, YCbCr matrix, transfer characteristic, and chroma-location
facts resolved from media metadata / user interpretation. Platform adapters for
D3D12/D3D11, VideoToolbox/IOSurface, and VA-API/DMABUF must import handles
only; they must not silently decide Rec.709 vs Rec.2020, SDR vs PQ/HLG, or
left vs center chroma siting.
Both payload kinds carry `PreviewDecodeDiagnostics`: concrete path
(`InProcessFfmpegCpuRgba`, `InProcessFfmpegNative`,
`ExternalFfmpegCpuRgba`, or `PlaybackSessionRingHit`),
elapsed microseconds, cache-hit status, requested access mode, external-process
status, CPU-residency evidence, seek status, requested seek strategy,
session-local seek-index availability and source (`None`, `SessionObserved`, or
`ProbeBacked`), anchor-use evidence, decoded frame count, in-process FFmpeg
decoder threading mode/count, and stage-level wall-clock timings for session
open, cache lookup, seek, packet/decode, FFmpeg hardware-frame transfer back to
CPU, software scaling, RGBA copy, and the experimental external-process path.
The same diagnostics carry the current hardware decode contract:
`hardware_decode_request`, `hardware_decode_decision`, `hw_accel_backend`,
`hardware_decode_candidate_backend`, `hardware_decode_candidate_handle_kind`,
`hardware_decode_adapter_available`,
`hardware_decode_ffmpeg_device_type_available`,
`hardware_decode_ffmpeg_codec_config_available`,
`hardware_decode_ffmpeg_hw_pixel_format`,
`hardware_decode_ffmpeg_device_context_attempted`,
`hardware_decode_ffmpeg_device_context_created`,
`hardware_decode_ffmpeg_device_context_error_code`,
`hardware_decode_cpu_transfer_configured`,
`hardware_decode_cpu_transfer_observed`, `hardware_decode_active`,
`zero_copy_active`, `decoded_frame_residency`, `gpu_frame_handle_kind`,
`hardware_decode_blocker`, and `native_decode_fallback`, plus
`decoded_surface_format` for the decoder output
format before CPU RGBA conversion. `Nv12` and `P010` are the primary GPU-native
YUV/P010 residency candidates; they are media facts, not renderer import claims.
These fields are fail-closed; until a real hardware-frame decoder and renderer
import path are connected they must report CPU RGBA residency with
`TextureResidencyNotConnected` and a CPU RGBA `PreviewHardwareDecodeDecision`.
When FFmpeg has no matching codec/backend hardware config, the decision should
be `CpuRgbaCodecUnsupported`. When FFmpeg advertises the config but cannot
create the hardware device context, the decision should be
`CpuRgbaHardwareUnavailable`. When FFmpeg can create the device context and
hardware frames are observed but Mondrian still transfers them to CPU RGBA, the
decision should be `HardwareDecodeCpuTransfer`; this should improve decode CPU
pressure but is not the final residency model. Only after an Adapter produces
native GPU residency should later readiness failures move to Renderer import
diagnostics.
Every Viewer GPU Adapter must preserve those media facts in its frame-residency
telemetry. Media decode diagnostics feed the renderer-owned
`ViewerGpuMediaSource` contract, while a retained native payload feeds
`ViewerGpuNativeSource`. The latter keeps physical source extent separate from
the renderer materialization extent, so a 4K decoder surface need not allocate
and color-transform a 4K working frame for a quarter-resolution Viewer. App
product admission combines decoder residency/handle/format with the active
device's Renderer import support. This does not make CPU RGBA preview hardware
decoded; it prevents the future hardware decoder adapter from being hidden
behind a generic "GPU input upload" label once it starts producing NV12/P010
native surfaces.
Preview sessions seed their seek index from FFmpeg's container/probe stream
index when available, using a small media-layer LRU cache keyed by
path/fingerprint/video-stream. That first production path gives scrub and still
decode real keyframe/GOP evidence without a full packet scan before first frame.
When a container exposes no usable index, sessions continue to learn keyframe
anchors from decoded packets and report `SessionObserved` instead of pretending
the source was probe-backed. Packet-observed anchors use DTS when available and
fall back to PTS; the decode target remains the exact requested presentation
coordinate. A keyframe anchor selects only the input seek origin and must never
rewrite that target, including frame-zero requests whose valid preroll starts at
a negative DTS.
`ScrubCursor` derives its effective selection/decode budget per request from
that evidence: probe-backed anchors use bounded approximate-first-frame selection,
missing or session-only evidence gets a bounded responsiveness-first budget, and exact
playback/still requests keep their larger deterministic budget. The budget
reported in `PreviewDecodeDiagnostics.forward_decode_budget_frames` is the
effective budget that was actually used for that request.

`RandomAccessStillFrame` with deterministic keyframe-before seeking may reduce
long-GOP output work without weakening frame identity. For only the distant
prefix, the decoder uses FFmpeg's `NonReference` discard policy; it restores
full decode at least 64 stream-frame durations before the requested timestamp.
Non-reference pictures cannot be dependencies of later pictures, while the
prefix's reference pictures continue to populate decoder state. Playback and
Scrub never use this optimization. A packet without PTS/DTS restores full
decode immediately, and every submitted video packet counts toward the same
forward-decode budget even when FFmpeg intentionally emits no picture for it.
The selected still must still satisfy the ordinary exact-frame rule:
`temporal_approximation` remains structured diagnostics, and the professional
accurate-seek gate rejects any nonzero approximation count rather than trading
correctness for its 500 ms p95 target.
App-level preview diagnostics aggregate those fields so playback/perf JSON can
show whether a 4K/HDR test is decode-bound, long-GOP seek-bound, cache-bound,
single-thread decode-bound, software-scale/copy-bound, worker-queue-bound, or
GPU-output-bound. The same diagnostics must also preserve the stage timings from
the slowest single decode frame and slowest single post-decode render frame, and
track how long decoded jobs waited in the preview worker queue before decode
started. Performance reports classify their primary bottleneck from max-frame
stage timings plus max queue wait, while aggregate stage totals remain trend
evidence. This avoids blaming a cumulative stage total when an interactive stall
came from one pathological seek, decode, software-scale/copy, composite,
output-boundary frame, or current-frame job waiting behind other decode work.
The in-process preview decoder uses bounded frame threading by default for all
software-decode access modes. Frame threading pipelines coded pictures and
does not assume the source contains enough independent slices to use a declared
slice-thread count. With the retained compact-YUV and explicit GPU staging
paths active, real H.264 High 4:2:2 10-bit
3840x2160@60000/1001 on a 12-thread machine measures about 9.4 ms mean steady
work per frame and repeated 60/60 exact presentation with frame threading.
Slice threading measures about 15 ms in successful runs and 25 ms under an
ordinary load perturbation, draining the bounded preroll and exposing stale
Viewer output. `MONDRIAN_PREVIEW_DECODE_THREADING` and
`MONDRIAN_PREVIEW_DECODE_THREADS` remain explicit qualification overrides;
they are not machine-local correctness state. Spawning a second concurrent
Playback decoder for the same source remains invalid as a generic throughput
fix because duplicated long-GOP seek/demux work destroys sequential locality.
Output resolution pressure (Full -> Half -> Quarter) reduces renderer work but
does not falsely claim to reduce codec work; a source that still cannot meet
cadence requires hardware decode or a separately admitted proxy/optimized-media
path without changing time, color, or source semantics.
The app
preview service runs a conservative decode worker pool from
`PreviewDecodeCpuBudget`: one worker on small CPU budgets, two on common
mid-range machines, and three only on larger workstations. FFmpeg decoder
threads per worker are computed from the same budget, leaving explicit
interactive headroom so current-frame decode can make progress while another
worker is occupied by prefetch or a long-GOP seek without oversubscribing the
UI, renderer, audio, or FFmpeg's own codec threads. When a current-frame request
is scheduled, the job queue also prunes obsolete prefetch jobs from older render
generations before enqueueing the current work. One continuous Playback Epoch
retains its generation across ordinary frame advances: frame identity belongs
to the media request key, while seek/restart or another interpretation
discontinuity rotates the generation. Fresh same-generation prefetch
remains eligible only after the current frame is not pending, no current-frame
job is already queued or running, and queued plus in-flight prefetch is below
the forward window. It then tops up only the unfilled queued-plus-in-flight
prefetch budget while traversing tracks and nested sequences. That lets playback
warm nearby frames without stealing first-frame or recovery budget. This worker
pool and pruning are scheduling guardrails; hardware-resident decode remains a
separate execution concern.
Preview decode exposes a cooperative cancellation boundary for every access
mode: app workers pass a generation-aware request probe to the media decoder.
Each FFmpeg session in the worker-owned context installs that probe as an
`AVIOInterruptCB`, including before input open and stream discovery, and keeps
it active across seek and packet I/O. The media loop also checks it before
opening, seeking, packet decode, frame receive, EOF draining, hardware transfer,
and RGBA conversion. If cancellation fires, the decoder returns a
typed canceled outcome rather than a media failure. The already-open codec and
immutable hardware device may remain allocated, but demux/codec position is
never considered reusable: after the operation returns, the owning worker
flushes the codec, restores default discard policy, clears its session-local
playback ring and reverse GOP replay window, clears EOF/last-PTS state, and forces the next request through
indexed seek. Scrub and exact-still already seek by access policy; Playback now
does so after cancellation as an explicit discontinuity rather than continuing
from a possibly half-submitted packet or partly drained reorder queue. Input/codec open
cancellation cannot publish a half-built session, while a source, fingerprint,
device, geometry, or color-contract change destroys the old session before
opening its replacement. This avoids repeated non-interruptible
`avcodec_open2` gaps during latest-wins seek bursts without weakening frame
exactness or trusting canceled codec position. The App separately releases the prior generation's native source
frame after its final GPU Viewer output is usable. Playback keeps one continuous
decoder; scrub and GPU-resident exact Still share a resource-bounded,
output-lease-aware Interactive Session set rather than accumulating one surface
pool per request or reusing a pool whose prior native output is still owned. A separate CPU Still
slot preserves CPU extraction locality without owning native GPU surfaces.
For every production access mode, the packet source is a reusable isolated
demux Session: the process is the recoverable format-call generation, while the
parent decoder and output lease retain the same slot rules. Successful
seek/read/EOF operations preserve the helper for later targets. A protocol or
FFmpeg failure and a parent-enforced cancellation poison it; the following
request must open a new process before any packet can be published. Playback,
Scrub, and Exact therefore share one packet-source Interface and recovery
semantics without sharing scheduling slots or precision policies.
Slot locality does not authorize Playback and Interactive sessions to remain
hardware-resident at once. A family transition first evicts only native
decoder-resource entries from the shared Frame Store, wakes worker waits through
a Broker-owned revision, and defers new-family admission until every
opposite-family worker confirms it has destroyed its thread-owned
`PreviewDecodeSessionContext`. CPU decoded frames remain cacheable across the
transition, and final Viewer texture/raster ownership is unaffected. Workers
never destroy another worker's FFmpeg context; new work cannot race the
retirement acknowledgement; acknowledgement additionally waits for the shared
worker-family outstanding-native-output count to reach zero. Each Session-local
count independently gates slot reuse, and a revision-mismatched
acknowledgement is ignored.
This destroys the retired family's codec, DPB, `AVHWFramesContext`, and native
surface pool. The injected worker-family pool may retain an idle immutable
FFmpeg device root under its explicit budget; media gives each new codec its
own reference to that backend/adapter generation. Device identity is not
decoder-session state, and no permanent process-owned device reference exists.
This bounds the production path to the active Playback or Interactive native
surface pool, avoids driver device recreation at the discontinuity, and still
does not share seek/codec state between families or across media.
The experimental external-process CPU RGBA path terminates and reaps only its
per-request child when the probe fires; the compatible in-process session may
remain. Stale work is never cached or marked as a failed source.

`mondrian_media::preview::cancellation` is the sole media-layer owner of this
protocol. It contains the public cancellation fact and aggregate evidence, the
request guard, panic-isolated probe invocation, first-observation atomics, and
the FFmpeg C callback. Decode sessions see only three operations: install one
request probe, publish the current checkpoint, and obtain the resulting fact.
They must not inspect atomics, retain probes, or reconstruct a cancellation
source from an FFmpeg error. This keeps the unsafe callback and concurrent
state behind one deep Module while preserving `mondrian_media::preview` as the
stable public Interface.

`mondrian_media::preview::execution_progress` separately owns concrete Adapter
liveness evidence. One worker is the sole writer to a lock-free seqlock-style
observer; Runtime diagnostics and Headless acceptance read a coherent fixed-size
snapshot containing the current stage, request and publication sequences,
FFmpeg interrupt-callback poll/cancel sequences, and the request that last
observed callback cancellation. Callback polls use the same sole-writer
seqlock publication as stages, so a stable `PacketRead` can distinguish “FFmpeg
stopped polling”, “the active probe did not observe Broker cancellation”, and
“FFmpeg observed cancellation but did not return”. Callback observation is not
call-return or execution-lease evidence. The composition root creates a
non-cloneable, `Send` bootstrap plus
the read-only observer; the worker consumes that bootstrap and constructs the
non-`Send` FFmpeg session context on its owner thread. No unsafe thread transfer
or second stage writer is permitted. Stages distinguish input open, stream discovery, hardware-device
attachment, codec open, seek, codec flush, packet read, codec input, codec
output, frame materialization, output-lease wait, and session retirement. Every
potentially blocking FFmpeg call publishes its stage immediately before entry.
This Module deliberately owns no deadline, cancellation, recovery, or restart
policy: the Frame Work Broker remains authoritative, and an observed stalled C
call—even after callback cancellation—is evidence rather than permission to
pretend the execution lease ended.

Codec receive has a strict internal result contract: one frame, need-more-input
backpressure (`EAGAIN`), end of stream, or a structured decode failure. Only the
first two non-frame states may continue normal send/receive coordination; device,
codec, invalid-data, and other fatal errors cannot be collapsed into “no frame”
and leave a poisoned session eligible for reuse.

Speculative prefetch decode is also bounded by a short app-level wall-clock
budget. Current-frame decode is not canceled by this budget, and the deadline
applies only to playback prefetch work. Scrub and still-frame requests are
latest-wins/current-frame work; if future callers try to submit them as
prefetch, scheduler admission must reject them instead of letting the worker
deadline silently reinterpret their access mode. A playback prefetch that is
already in a worker must also yield immediately when another current-frame
request is pending, unless that prefetch is for the same media key that was
promoted to current-frame work. This cancellation is reported separately as
prefetch-preempted-by-current rather than as obsolete work or a deadline miss,
so diagnostics can distinguish intentional current-frame protection from
expired speculative work.
Playback current-frame decode has a separate display deadline. The app layer
assigns that deadline when a `Current + PlaybackCursor` job is admitted or
promoted, because only the app owns viewer/playback-clock intent. The instant is
projected from the exact Playback Frame Demand rather than reconstructed from a
codec timeout. A playback current job that reaches a worker after its deadline is
canceled before FFmpeg work begins. Expired playback-current jobs must not
block fresher current-frame work in worker queue selection, but they must remain
observable long enough to emit a structured deadline cancellation instead of
disappearing as an opaque queue drop. Once a `Current + PlaybackCursor` lease
has started, however, the Broker's `FinishForLocality` policy prevents that
presentation deadline from becoming decoder cancellation. The worker may
finish the now-late request so its Session, DPB, keyframe index, hardware
device, and forward position remain available to later current work. Explicit
generation/epoch invalidation, shutdown, residency-family retirement, and
preemption still cancel through the same media predicate. This policy is
intentionally not a media crate concept: `mondrian-media` receives only an
access-mode request and a cancellation predicate.
Completion polling repeats the same deadline contract as a final guard. If a
`Current + PlaybackCursor` result reaches the app after its display deadline,
the app may still record decode diagnostics and success/failure telemetry, but
it must not cache the frame, mark it visible, or reset sustained-pressure
recovery as a successful current playback frame. This protects the viewer from
decode backends that return after missing a cooperative cancellation check.
Repeated playback-current deadline misses put the app scheduler into sustained
pressure recovery. In that state, a fresh `Current + PlaybackCursor` request
must not pile onto the decode queue while another current or playback decode is
already queued or in flight; it is skipped as a clock-driven drop/proxy/hardware
decision. When no realtime decode work is pending, the scheduler must admit one
current playback request so the viewer has a chance to recover instead of
staying permanently stale.
User transport and close/quit actions are interactive escape paths. When they
arrive while playback or current-frame work is active, the app host must cancel obsolete
preview generations and queued jobs before dispatching the state mutation, and
it must refresh transport controls without synchronously requesting a new
preview frame. A visible pending-close confirmation must be repainted
immediately; an invisible modal or a decode worker that is still finishing an
old frame must never make pause, close, or quit feel locked.
Diagnostics for this path must separately count escape-path requests, the
scheduler pending requests they canceled, and the worker-queue jobs they
cleared. Those counters are distinct from generic obsolete-generation churn so
reports can distinguish healthy user-driven preemption from unstable playback
rescheduling.
Schedule diagnostics also count current playback decode decisions, late-frame
drop decisions, and proxy/hardware recommendations. Those are app scheduler
facts, not media decoder facts, and they are how perf tooling distinguishes
clock-driven playback from best-effort frame extraction. The broad
proxy/hardware recommendation counter must not be the only signal for recovery
policy: current playback frames blocked specifically by renderer/native GPU
import readiness are counted separately so automated fallback can distinguish a
missing GPU-resident path from deadline pressure or proxy-generation pressure.
Preview decode performance reports must surface that case with a specific check,
root-cause evidence, and an action to connect renderer native video import.
Playback hardware-decode admission is also runtime-gated by the app preview
service from the coherent `app::native_video_import` snapshot. The media
request default remains `PreviewHardwareDecodeRequest::Auto`; Window or
Headless composition code may raise playback jobs to `PreferHardwareDecode` for
FFmpeg hardware CPU-transfer fallback, and to `PreferGpuResident` only after the
Renderer native decoded-frame import contract reports ready and the media
surface facts match at least one Renderer-supported native handle and format
with a sampling backend. This prevents CPU-transfer playback from
being mistaken for native GPU residency while still avoiding a pure software
decode default for sustained playback. The same admission state must be
serialized in preview diagnostics and performance reports as separate media,
Renderer, and final-admission facts; a known native-import blocker must produce
a specific gated-admission root cause instead of disappearing as a generic
media-layer software decode.
Playback reports must also distinguish hardware-decode intent from effective
hardware decode. If `PlaybackCursor` requests `PreferHardwareDecode`,
`PreferGpuResident`, or `RequireGpuResident` but the observed result is neither
GPU-resident native decode nor an observed FFmpeg hardware CPU-transfer frame,
the report must surface a `preview_decode_playback_hardware_fallback_not_engaged`
root cause with backend, codec, device-context, decoder-open, setup, and native
import evidence. This is a recovery signal for proxy/optimized-media or backend
repair, not proof that the hardware path is active. The app playback scheduler
records the same condition as
`current_hardware_fallback_not_engaged_decisions` and increments the broad
proxy/hardware recommendation counter at most once for a completed current
playback frame, even when the same frame also reports a native-import blocker.
This keeps automated recovery policy clock-driven without inflating pressure
metrics from overlapping hardware diagnostics.
When playback pressure resolves an asset that is already in proxy mode but the
proxy is missing or stale, the app preview service may request proxy generation
through the instance-owned Proxy Generation Service. Its exact artifact key and
retained failure memory prevent a late playback frame from enqueueing work on
every refresh; Playback recovery may promote matching queued Import work but
cannot bypass cache-root capacity or project generation. Preview must not spawn
FFmpeg directly, change media color interpretation, silently enable proxy mode,
or retain a parallel request registry; the media crate still owns proxy file
generation and status probing.
Interactive scrub uses app-selected adaptive hints rather than a separate decode
API. The app preview service observes recent scrub seek locality and scrub
decode latency, then tags `PreviewDecodeRequest` with a
`PreviewScrubAdaptiveClass`. The media layer preserves the requested frame
semantics but may tighten bounded-any seek windows and forward-scan budgets for
hot or slow scrub regions. This avoids long UI-blocking scrub attempts while
keeping settled still-frame requests exact. Do not implement scrub speedups by
silently changing color interpretation or by shrinking decoded media geometry
unless the renderer has an explicit source-sample extent versus layout extent
contract.
`RgbaFrame` and `FloatRgbaFrame` store their payloads in shared immutable memory
so cache hits can adjust per-request diagnostics without deep-copying a 4K
frame. Callers that need ownership must request it explicitly through the frame
consumption API; renderer color-frame boundaries should prefer shared payloads
where their typed input contract permits it.
Execution truth is separate from per-request cache diagnostics.
`PreviewDecodeExecutionPath` is assigned only from an observed FFmpeg hardware
CPU transfer or a validated native GPU payload and remains unchanged when a
Playback request becomes `PlaybackSessionRingHit` or the payload later re-enters
through the App Preview Frame Store. Prefetch and the Store retain this
provenance, aggregate it across nested/media layers, and bind it to the exact
GPU candidate. Reuse therefore describes the current request without pretending
another hardware decode occurred.
`PreviewNativeDecodedFrame` is the separate GPU-resident payload contract and
must flow toward renderer native decoded-frame import rather than the RGBA cache.
For an admitted opaque NV12/P010 native decode, cache and in-flight identity use
the probed source raster, not the current Viewer presentation scale. Native
surfaces are source-sized and Half/Quarter quality is a later Viewer spatial
operation; including that output extent in the decode key would invalidate
useful prefetch whenever adaptive presentation scale changes. CPU decode keeps
its requested decode extent because scaling is part of that media operation.
Preview decode session reuse is isolated by physical execution family and the
source-decoder contract. Playback has its own slot; GPU-resident scrub and exact
Still share the Interactive slot while deriving policy anew from each request;
CPU Still remains separate. Every slot and every App Preview Frame Store entry
must be keyed by a complete media file fingerprint, not by path alone. Session
reuse additionally requires the exact requested physical video stream,
backend, hardware request/device selector, packet-source execution family, and
source-color contract. Adaptive output geometry is a materialization binding,
not a decoder identity: changing it retains demux/codec/DPB/seek state, rebuilds
the CPU scaler, and clears only output payloads cached at the old extent. Proxy
regeneration finalizes fresh media at
the same proxy path, so same-path Store hits or reused FFmpeg Sessions are valid
only while the newly observed complete object identity and filesystem change
generation match the fingerprint captured at admission. Path, length, and
modification timestamp alone cannot authorize that reuse.
An execution generation superseded before Broker admission is a retryable
Adapter race, not a Timeline execution failure. It may remain visible in
scheduler churn diagnostics, but Viewer presentation projects `Loading` until
a fresh execution snapshot admits or resolves the request; user-facing error
state is reserved for a terminal media or execution contract failure.
The frame-evaluation working set may retain a typed media-producer wait to
deduplicate Timeline resolution, but every acquire must reassert the
execution-pending level after the presentation turn resets it. A retained wait
without that level would falsely project normal decode latency as a Timeline
execution failure. A retry-admission wait has no producer by definition and
therefore remains Loading only until a fresh snapshot can attempt admission.
FFmpeg's default app log level is fatal for product preview decode. Codec-level
warnings and recoverable decoder errors, such as HEVC reference-frame messages
during aggressive seek/scrub, must not leak directly to the user terminal as the
primary diagnostic channel. Developers can opt into noisier FFmpeg output with
`MONDRIAN_FFMPEG_LOG_LEVEL`; product health should use structured decode
diagnostics and explicit frame failure/cancellation reasons instead.
Packaged runtime verification checks FFmpeg's public decoder registry for the
declared baseline video, audio, image, and PCM families, including PNG,
OpenEXR, DPX, and TIFF. It separately interrogates the adjacent CLI tools for
every software
encoder and filter consumed by current Proxy, Audio, Export, and validation
Implementations, including PNG/OpenEXR/DPX/TIFF encoders and the image2 muxer.
TIFF Float32 encoding is a native Export Adapter, but the packaged decoder is
still required as independent product validation evidence. Windows CI, release,
and developer setup therefore install the
explicit `zlib,ffmpeg,ffprobe,gpl,x264,x265,aom` vcpkg profile; a
`libavcodec.pc` file alone is not runtime evidence. The vcpkg step is
idempotent and uses `--recurse`, and its cache identity includes the product
runtime profile so an older partial component set cannot be silently reused.
Preview path resolution already resolves the source/proxy file-revision evidence; app
workers must forward that `MediaFileFingerprint` into the media decode
boundary. The media worker deliberately performs one final revision-evidence check
immediately before Session reuse/open because the earlier App observation also
authorized probe and color semantics and cannot close the scheduling race.
`mondrian-media` captures an initial fingerprint itself only for lower-level
callers that do not already have one; such a value has no earlier authorizing
contract to compare.
`MONDRIAN_PREVIEW_DECODE_THREADING` and `MONDRIAN_PREVIEW_DECODE_THREADS` are
diagnostic overrides, not separate decode semantics. `THREADS` means FFmpeg
decoder threads per app preview worker and remains clamped by the coordinated
CPU budget. App worker count has no environment override: the Viewer Preview
Service derives it directly from `PreviewDecodeCpuBudget` for playback and
interactive lanes. Production sessions live in each worker's explicit
`PreviewDecodeSessionContext`, remain alive across short gaps for locality, and
are cleared automatically after two seconds idle, at an acknowledged
playback/interactive residency-family transition, or at worker shutdown. The
top-level media convenience function retains a thread-local context only for
standalone diagnostic/test callers that do not own a production worker;
`clear_thread_local_preview_decode_session()` exists for those callers and is
not the production worker lifecycle mechanism. Test builds clear that
convenience Session at project close to keep process-local fixtures isolated;
release builds cannot make App lifecycle depend on it. Production worker
contexts retire through their own acknowledged idle/shutdown boundary. Idle
release is resource policy, not a cache-key or generation change.
Cross-family admission may report transient pressure only while a real retry
owner exists. The decoder-residency coordinator publishes one shared Preview
work-watch edge when the final required worker acknowledgement changes its
barrier from blocked to actionable; partial, duplicate, and stale
acknowledgements publish nothing. Each media, visual, and Basic Title worker
also installs an RAII terminal-health notification, including unwind. The
Runtime treats an all-producer media-result disconnect as a one-shot terminal
health failure only when workers were configured and shutdown was not
requested, closes further admission, and exposes the state in diagnostics.
The Thumbnail worker follows the same ownership rule: one explicit
`PreviewDecodeSessionContext` belongs to that bounded worker, every still job
passes through it with its cancellation token and exact physical stream, and
worker exit clears the context. Thumbnail raster and failure residency remains
owned by the Thumbnail Execution Service; decoder residency is never hidden in
thread-local process state.
Thumbnail and Waveform each also publish an exact single-worker physical phase
through the shared App activity ledger. `WaitingForDispatch`, `Running`, and a
completed result awaiting foreground publication are distinct; UI/resource
coordination cannot derive Running as `pending - deferred`. The phase identity
includes the domain request key and execution generation, so rebinding may
remove obsolete publication authority while the old physical work remains
honestly visible until it returns.
Codec safety policy may narrow these diagnostic overrides. OpenEXR contexts are
always serial (`None`, one decoder thread): FFmpeg's frame-threaded EXR path can
hold the single image until EOF and deadlock during codec-context destruction
on Windows. Independent image requests remain parallel at the app decode-pool
level, so this does not serialize the media pipeline globally.

## Offline Timeline Export execution

Export consumes one immutable `TimelineExportSnapshot`; it does not read the
live Asset Library, current Sequence, or UI selection after admission. The App
capture boundary consumes renderer-prepared selected-range visual
Sequence/Asset/Transition evidence plus exact range-selected root/nested audio
Program occurrences and routed Component evidence; it never traverses Tracks
or Clips. The same stable-registry
`PreparedVisualProgram` values that produced visual reachability are retained
as a non-persistent execution attachment. Capture rejects missing and recursive
nested references, retains only the reachable nested Sequence closure, and
resolves each real Asset into one
`ExportMediaDependency`. That record keeps path, `MediaFileFingerprint`,
the exact physical primary-video stream index, exact selected-stream
`PictureSourceExtent`, detected color evidence, authored interpretation, color
diagnostics, and source raster extent together so no independent map can drift
from another. Only Asset Library `StillImage` classification grants `Still`
hold authority. A `Video` Asset remains time-varying even when its selected
stream reports one frame and uses only that stream's non-empty half-open
duration; missing duration fails closed. Container duration is never
substituted. Audio-only dependencies have neither a video stream binding
nor picture extent; any picture plan whose frozen dependency lacks either fails
before decode. The Timeline's pure selected-Transition validator checks those
facts and zero-based frozen nested-Sequence extents without live Asset Library,
filesystem, or FFmpeg access. Stream index zero is never an implicit fallback.
Queue admission reconstructs and validates the attachment for
deserialized/manual snapshots before delivery checks and freezes selected Basic
Title font source bytes. The worker consumes the exact captured visual/audio
Programs, frozen fonts, and frozen Asset identities rather than consulting the
live Effect registry, system font catalog, or recursively deriving
dependencies again. Only frozen `AudioProgramExecutionDemand::ProvenSilent`
may omit PCM execution; processor-only and selected pre-mute paths remain
executable even when no media Component is reachable.

The frame-local Export decode cache authorizes reuse only through one complete
`ExportDecodeCacheKey`. Equality retains Asset identity, exact path, the full
complete `MediaFileFingerprint`, exact physical video stream, source time,
resolved input color and range, alpha interpretation, source and sampled
geometry, and the complete
source-to-working context (working space, Project engine, and per-contribution
input tone-map intent). `HashMap` hashing is only bucket selection; a compact
fingerprint never authorizes reuse. This is important for nested Sequences:
the same physical sample evaluated under a different child working context
cannot receive pixels prepared for its parent.

The decode identity's source coordinate is the complete `SourceSampleTarget`.
`Covering` and `StrictPredecessor` at the same rational time are different cache
keys. The decoder lowers the target to its selected stream time base exactly
once: covering is `floor(t * rate)`, strict predecessor is
`ceil(t * rate) - 1`; a strict predecessor at source origin is invalid. This
single rule is shared by in-process FFmpeg and external fallback seeking. VFR
frame selection still uses the decoded half-open presentation extent and never
replaces this target with nearest-PTS or a nominal-frame epsilon.

Every Export job owns one explicit `PreviewDecodeSessionContext` inside its
`ExportVisualRenderSession`. The same context is passed through root frames,
closure-addressed nested-Sequence materialization, and Transition endpoints;
production Export never uses the convenience thread-local decoder. The context
is a physical decode resource owner, not a recursive Timeline authority. This
preserves codec/DPB locality across adjacent frames without hiding
process-lifetime FFmpeg state. Job completion, failure, cancellation, panic
unwinding, and ordinary session drop all retire that context.
`resident_session_count()` is lifecycle evidence only: it allows worker/job
tests and diagnostics to prove retirement without exposing decoder internals.

Each Export picture request carries the complete
`ExportMediaDependency.source_fingerprint` and exact
`video_stream_index` into `PreviewDecodeRequest`.
`mondrian-media` revalidates it after output-lease waiting and before
session reuse/open, then revalidates the same revision after demux, decode,
conversion, and frame materialization but before returning a successful
outcome. Same-length replacement therefore fails closed at either boundary.
The returned Export layer records that exact revision as its execution
evidence, and cache admission compares it to the complete revision in
`ExportDecodeCacheKey`; neither path, length, nor a compact hash may substitute
for it.

The Export queue is a dedicated offline service with bounded in-flight work and
bounded lightweight terminal history. The immutable Project-sized payload is
consumed once when its worker dispatches; Window and Headless observation clone
only `ExportJobSnapshot`. Preview/Thumbnail/Waveform/Proxy and Export share
`ExecutionCancellationToken`, priority, generation, and terminal-evidence value
semantics, but they intentionally do not share one worker pool or capacity
policy.

Source revision is validated before media preparation, around every real video
decode as described above, and again immediately before publication. Both the
admitted and current observations must contain
complete filesystem object/change evidence; an incomplete observation fails
closed and cannot authorize decode-cache or stream-binding reuse. FFmpeg writes
to a unique sibling temporary file and its
stderr is drained from process creation into a bounded tail. Export transfers
each reusable rendered frame allocation through the supervised stdin pump, so
generation cancellation can terminate and reap FFmpeg even while an OS pipe
write is blocked. Audio source reads use the same generation token rather than
an uncancellable convenience path. Stream/signal validation runs against the
identity-bound temporary deliverable. Export freezes an explicit create-new or
overwrite policy at admission, validates the complete object, and delegates
publication exclusively to `mondrian-storage`; this media document does not
define a second platform replacement implementation. A proven pre-namespace
failure retains the validated partial, while durability-unconfirmed and
namespace-indeterminate outcomes remain distinct terminal evidence and
authorize no blind cleanup. `Completed` is returned only from durable
publication evidence and is therefore authoritative over a cancellation request
that arrives after the commit point. The complete contract is specified in
`docs/specs/export-spec.md` and [Render Pipeline](render-pipeline.md).

The renderer owns one typed GPU input-stage resource contract for all decoded
CPU source representations. RGBA8 uploads to `Rgba8Unorm`; source-encoded float
and scene-linear float upload losslessly to `Rgba32Float` while retaining their
distinct `EncodedFloat` versus `LinearFloat` descriptors. The stage then runs
the resolved OCIO GPU input transform and produces a GPU-resident linear
working frame. This is the bridge for guarded rollout of GPU input transforms.
The app viewer uses it for supported media preview layers before GPU
working-space compositing, falling back per-layer to CPU working-frame upload
only when the GPU input stage cannot be recorded. CPU outcomes still require
one host-to-device upload; native decoder surfaces use the separate zero-copy
import contract and never masquerade as one of these CPU source types. Here
zero-copy means no CPU transfer and no decoder-surface pixel copy before YUV
sampling; YUV-to-RGB and OCIO still deliberately allocate Renderer-owned
encoded and working textures.

An exact probed `Yuv422p10le` source may instead resolve to the explicit
`CompactCpuYuv` Preview representation when the downstream Viewer accepts GPU
materialization. Media retains an independently reference-counted immutable
FFmpeg `AVFrame` with its native 16-bit luma, Cb, and Cr planes and explicit row
strides. It does not repack or interleave roughly 33 MiB for every UHD frame.
The reservation includes conservative FFmpeg row alignment, while actual Frame
Store accounting uses the retained plane allocations.
Half/Quarter recovery resolves to `ReducedCompactCpuYuv`, scales the planes to
the representation extent, and retains that scaled AVFrame under the same exact
YUV sampling contract without a second plane copy.
The representation is part of the decode key but not the compressed-stream
decoder Session identity, so
Frame Store admission reserves the physical plane footprint and zero native
decoder-surface units. A returned layout other than `YUV422P10LE` fails the
request; it cannot silently publish an RGBA allocation under the compact
identity. CPU-addressable analysis, thumbnails, scene-linear sources, and
reduced representations without a compact source contract continue through
their typed RGBA/float paths.

## Asset Classification

`mondrian-assets` classifies admitted file candidates using the stable
`MediaProbeSnapshot`. Audio-only
extensions or media without meaningful video streams become `Audio`. A
recognized picture-file extension becomes `StillImage` only when the probe
proves exactly one picture frame. When container metadata omits its frame count,
the media probe decodes only until the second frame or EOF under a bounded
packet budget: exactly one frame reaching EOF is proof, while a second frame,
budget exhaustion, or decode ambiguity remains `Video`. Incomplete evidence
therefore cannot silently erase animation. The Asset identity is the routing
fact. Preview/Thumbnail/Export do not reclassify by extension, and the Timeline
expresses the actual hold as a zero-rate Media Clip rather than inventing a
source duration.

Generated Assets never construct a fake `MediaProbeSnapshot`. Their closed
`AssetSource::Generated` identity carries no file path, fingerprint, or stream
metadata; Preview and Export route the generated source explicitly.

## Preview Decode Session Evidence

Every successful Preview media result carries an exact
`PreviewDecodeSessionDisposition`: `Opened`, `Replaced`, `Reused`, or
`BypassedCache`. `Unspecified` is a capture-integrity failure for a successful
result, not a synonym for “not reused.” A playback-ring hit remains
`BypassedCache` when the outer Session owner attaches request evidence; it must
not be overwritten with the compatible Session's `Reused` state.

Exact decode also projects one narrow `PreviewDecodeTemporalSelection` from
the full diagnostics at the media-payload boundary. It exists only when
requested PTS, selected PTS, a positive non-overflowing selected duration, and
the duration's evidence source are all present. It preserves physical
presentation identity for validation without making downstream caches depend
on decoder policy/performance diagnostics. Golden signed-retime evidence
requires the requested PTS to lie in the selected end-exclusive interval and
rejects approximation. For VFR, average frame rate is never a physical frame
oracle: the gate compares an exact strict-predecessor request against the
adjacent covering request and proves the decoded 60 ms and 20 ms intervals.

Concrete cancellation evidence remains separate from successful-frame
profiles. It carries the Session disposition reached by that request and the
measured Session-open/replacement duration. Cancellations before Session work
remain unclassified explicitly. This lets the App account for canceled reuse,
cache bypass, open, and replacement without manufacturing a successful frame
or inferring lifecycle from a cancellation checkpoint. Open/replacement
cancellations participate in Session-churn evidence.

## Color Metadata

Media probe separates diagnostic candidates, executable metadata, and policy
assumptions. `VideoStreamInfo.color_interpretation` is the sole persisted
interpretation contract. Its `candidate_color_space` is diagnostic/UI-facing;
`VideoStreamInfo::executable_color_space()` validates the same evidence before
it can drive a transform, so no parallel detected-color field may drift.
Evidence records whether a result came from a camera/log metadata hint, a
complete file-name pair, complete CICP colorimetry, partial CICP tags, ICC,
unsupported CICP tags, or decoder unavailability.
`interpret_video_color_metadata(...)` owns the selection policy so decoder
integrations do not duplicate it. Its Interface requires the probe-proven
optional `VideoSamplingContract` as a separate argument; callers cannot omit
sampling and later infer YUV/RGB obligations from the selected color identity.
Camera/log hints resolve only when they identify both the transfer curve and
the associated camera gamut. For example, `S-Log3 / S-Gamut3.Cine` is a
supported exact identity, while bare `S-Log3` remains unresolved. The same rule
applies to ARRI, Canon, Panasonic, RED, Blackmagic, DJI, Apple, and DaVinci
camera families. File names participate only when they contain the complete
pair, and remain diagnostic suggestions that never execute; bare names such as
`Slog3` or `Log3G10` do not invent a gamut. This prevents a plausible-looking
but incorrect primary conversion from being hidden behind a generic log label.
ICC profile names are display-profile evidence and are never promoted to camera
input identities.
Selection is deterministic: a closed, typed stream declaration outranks a
typed container declaration; either may override conflicting CICP while
retaining a warning. Exact CICP outranks free-form comments and file-name
inference. Partial CICP, mapped ICC profile names, descriptive text, and
complete file-name pairs may select the displayed candidate but never cross the
Auto execution seam. An unmapped ICC profile is
retained as evidence and warning but does not suppress a complete low-confidence
hint.
Warnings preserve machine-readable provenance, not only a resolved color-space
enum: multiple-hint warnings keep the selected and ignored metadata keys,
values, and scopes; hint-vs-CICP warnings keep the selected hint and the raw
CICP triplet that conflicted with it; lower-priority warnings record the method
that won and every conflicting descriptive hint it rejected.
Raw hint records are sorted into one canonical scope/key/value/provenance order
before selection and persistence. Exact declarations that agree merge;
descriptive suggestions that agree remain diagnostic without creating false
ambiguity. Probe dictionary enumeration order therefore cannot alter pixels or
the persisted warning set.
`VideoColorDiagnostic::summary()` is the
stable compact form for logs/export errors and should include those warning
details. `VideoColorDiagnostic::issue_summary()` is the machine-readable
contract for UI, telemetry, smoke JSONL, and export reports; callers must
consume its counters and flags instead of parsing the compact summary string.
The summary schema is strict: new evidence counters are required fields rather
than serde-defaulted compatibility values, so stale reports fail visibly.
`VideoColorDiagnosticIssueAggregate` is the shared rollup for combining many
per-stream diagnostics into one report surface.
Clip-level `MediaInterpretation` can override color space, frame rate, pixel aspect ratio, field order, and alpha interpretation.

### Encoded packet identity

`capture_video_packet_identity_cancellable(...)` is the Media-owned physical
evidence Interface for conservative Smart Render. It opens one stable file
revision, selects one exact absolute video stream, and hashes every ordered
packet as a length-framed SHA-256 sequence while counting packets and payload
bytes. Complete NTFS/ReFS or supported Unix object/change evidence is required
before opening and must remain identical after demux; cancellation is observed
between packets. The first-packet key flag is retained as a necessary full
stream sanity fact, never as proof that an arbitrary trimmed GOP is closed.

After remux, Export invokes the same Interface on the produced primary video
stream and requires identical packet count, payload bytes, and digest. This
proves encoded essence reuse independently from FFmpeg exit status or container
metadata validation. Media does not decide Timeline eligibility, codec
compatibility, color identity, or publication; those remain in their owning
Modules.

Asset library records store persistent user intent separately as
`AssetMediaInterpretation`. Imported media defaults to `Auto` for both color
identity and encoded signal range. Color Auto means "resolve from current
metadata, detector, and project color policy" and must not persist the currently
resolved color space. Range Auto follows the current probe result. User changes
from the asset-library Interpret Footage dialog are stored independently as
`MediaColorInterpretation::Override { color_space }` and
`MediaRangeInterpretation::Override { Full | Limited }`; both remain stable
across metadata re-probes, relinks, and detector upgrades.
Non-color data is represented as asset payload classification, not as an
Interpret Footage color-space mode. It is reserved for masks, mattes, technical
textures, and advanced utility-channel workflows, not the primary input
color-space picker. UI may display the current resolved result, confidence,
method, and warnings beside Auto, but that resolved value comes from
probe/color-management diagnostics rather than the asset record.
Preview and export resolve input color with the same precedence: non-color
asset payload classification first, then clip-level override, then
asset-library interpretation, then validated executable metadata, then the sequence
missing-metadata policy.
Preview, thumbnail, proxy, and export decode contracts resolve encoded range
with one separate precedence rule: explicit asset range override, then probed
range, otherwise `Unknown`. `Unknown` continues to fail closed at YUV conversion
or proxy-generation boundaries.
YUV matrix authority is likewise independent: the decoded frame must carry an
explicit supported matrix. Source `ColorSpace` is never used to infer a missing
matrix; proven RGB sampling has no YCbCr matrix requirement.

Unknown/missing metadata policy is resolved from the Sequence input-color
settings, not by UI panels. A nested Sequence keeps its own media
interpretation policy even when its placement forces evaluation in the parent
working space.
