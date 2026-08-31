# Reference Output

The Reference Output Module is Mondrian's machine-local scheduled clean-feed
boundary for professional video I/O. It is separate from Viewer presentation,
Export publication, and platform utility services.

```text
Prepared Visual full-raster working composite     Audio Program (48 kHz)
                    |                                      |
                    v                                      v
      Program Output [ReferenceOutput role]       exact frame interval
                    |                                      |
                    +------------------+-------------------+
                                       v
           v210 10-bit 4:2:2 or RGB 12-bit + s24 PCM + ANC frame
                                       |
                                       v
                  bounded Reference Output scheduler Module
                                       |
                                       v
              delayed DeckLink COM / AJA NTV2 vendor bridge
                                       |
                                       v
                         physical device / SDI connector
```

## Ownership and clean-feed semantics

`mondrian-reference-output` is the deep execution Module. Its public Interface
owns exact signal admission, packed video, embedded-audio, and ancillary
payloads, bounded
scheduled playback, provider events, lifecycle diagnostics, and the Adapter
Seam. It has no Timeline, Renderer, App, UI, wgpu, FFmpeg, COM, or C++
dependency. Vendor handles, callback threads, profile ownership, and ABI details
remain behind `VendorReferenceOutputBridge` Implementations.

Renderer is the only owner of picture lowering. `ReferenceOutputProgram`
starts with one canonical full-resolution working composite, resolves the
Sequence's `ProgramColorContext`, and applies the distinct
`ProgramOutputRole::ReferenceOutput` boundary. It never enters Viewer spatial
scaling, comparison, monitor adaptation, ICC calibration, Scopes, false color,
zebra, gamut alarm, or canvas background. Alpha is discarded only at the
physical carrier boundary.

Embedded audio comes from the selected public Audio Program. It never consumes
Monitor Path PCM, device-volume state, or a convenience downmix. The first
product matrix requires exact 48 kHz channel semantics and at most 16 channels.

App is the composition and lifecycle Adapter. A Session binds to exact
`SequenceId`, `SequenceRevision`, and Project author generation. Discovery,
open, schedule, start, poll, and stop are machine-local operations and never
enter Project state or Undo/Redo. Any author edit or active-Sequence change
revokes the binding and stops queued output. Project close stops and releases
device ownership.

## Exact signal contract

The request closes raster, rational cadence, scan, pixel carrier, encoded color
identity, code range, HDR signalling record, audio layout, reference policy,
preroll, and queue bound. A provider must advertise and read back the identical
mode. Implicit scaling, frame-rate conversion, scan conversion, range changes,
chroma changes, bit-depth downshift, color relabelling, audio remap, and silent
free-run fallback are forbidden.

The portable host carriers are compact little-endian v210 10-bit Y'CbCr 4:2:2
and full-range 12-bit RGB stored in 16-bit lanes. The vendor bridge performs any
last device-specific 12-bit packing only after exact admission. Float Program
Output remains extended until this final quantization. Embedded float PCM is
rounded once to signed 24-bit values in interleaved 32-bit lanes.

Audio windows use exact rational frame boundaries. At 30000/1001 fps the first
five 48 kHz intervals contain `1601, 1602, 1601, 1602, 1602` sample frames;
per-frame rounding is never used. One atomic bundle owns the video frame and
the corresponding audio interval. Frame gaps, duplicates, signal mismatch,
queue overflow, and out-of-order completion fail closed. The same atomic bundle
also carries one canonical `mondrian-broadcast::AncillaryFrame`; a packet can
never be scheduled independently from its exact video/audio frame identity.

Mode admission declares `Disabled`, `Required`, or `RequiredWithReadback`
ancillary policy. A nonempty inventory is rejected when ancillary is disabled.
Required readback needs provider evidence distinct from scheduling support.
Every completed frame then carries the provider's actual ancillary-inventory
digest; the Module compares it with the scheduled digest and fails the Session
on omission or mismatch. Simulated matching remains non-hardware evidence.
ST 291 packet construction, parity/checksum, ATC, AFD, and CDP transport remain
owned by the Broadcast Module; this Module owns only exact scheduling and
provider evidence.

HDR transfer/colorimetry requires an explicit `ReferenceHdrSignal` even when
no static fields are authored. Mode evidence distinguishes HDR signalling
(for example an ST 352 VPID) from complete mastering-display and content-light
transport. A provider that proves the former but not the latter cannot admit a
request carrying static metadata.

Interlaced hardware output remains unqualified under ADR-0008 and is rejected
by the App seam even though the platform-neutral value type can represent TFF.

## Runtime and failure semantics

Loading user preferences never loads an SDK or acquires a device. Preferences
store only optional provider/device identity, carrier preference, and reference
policy. The composition root installs a physical Adapter explicitly.

DeckLink and AJA integration is delayed behind the vendor bridge so Mondrian
does not vendor restricted SDK material. `UnavailableVendorReferenceOutputBridge`
reports missing runtime, no devices, or a version mismatch without inventing a
device. A physical Session is accepted only when evidence is hardware-backed
and provider, request readback, and device generation all match.

Callbacks publish only bounded low-frequency completion/status events. The
controlling Module owns ordering and accounting. Required external-reference
playout cannot start until a positive lock event is observed; subsequent lock
loss, device removal, or profile change stops the Session and enters `Blocked`;
provider execution failure enters `Failed`. Stop clears queued authority and
releases ownership.

The deterministic simulated Adapter qualifies Module semantics and fault
injection only. Its evidence permanently has `hardware_backed = false`, so it
cannot satisfy DeckLink/AJA, connector, wire-level, reference-lock, monitor, or
broadcast qualification.

## Performance and qualification

The queue is bounded to 64 complete bundles and reports scheduled high-water,
completed/late/dropped/flushed counts, exact audio-frame accounting, provider
versions, device generation, reference status, ancillary packet count, and
ancillary word/readback-verification counts. Long-duration evidence additionally
retains current outstanding and explicitly aborted frames, callback count,
positive-to-negative reference-lock transitions, and typed provider hardware
time (`ticks` plus `ticks_per_second`). Rate changes or non-monotonic hardware
ticks fail the Session; stop/block/failure classifies every queued frame so
`scheduled = completed + late + dropped + flushed + aborted + outstanding`
always remains auditable. Provider poll/stop failures and out-of-order
callbacks also enter stable failure and abort the complete remaining queue.
The current Renderer seam
has a correct CPU Float32 Program Output and packing path. It deliberately does
not claim a GPU-to-device resident path; future work should deepen the same
Module with reusable pinned buffers or device-resident transfers rather than
creating a second signal interpretation.

Software tests prove exact mode admission, carrier packing, cadence, ordering,
reference-loss behavior, runtime-unavailable behavior, clean-feed color
identity, App author binding, and preference persistence. Commercial hardware
qualification additionally requires licensed vendor bridges, supported
DeckLink/AJA hardware and drivers, SDI capture/monitor loopback including ANC
line/field readback, external
reference equipment, platform/driver matrices, and long-duration soak. Those
facts belong to COL-042 HITL plus COL-043, COL-046, and COL-047; software
simulation cannot close them.

Vendor integration should be implemented against the official
[DeckLink SDK manual](https://documents.blackmagicdesign.com/UserManuals/DeckLinkSDKManual.pdf),
[DeckLink scheduled output API](https://sdk-doc.blackmagicdesign.com/decklink-sdk/decklinkapi.html),
[AJA NTV2 SDK](https://github.com/aja-video/libajantv2), and
[AJA AutoCirculate guidance](https://sdkdocs.aja.com/public/ntv2/current/d9/d9a/recordplaytechniques.html).
