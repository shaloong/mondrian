# AJA NTV2 Reference Output owner

This is a real Windows C++ adapter built against the official AJA
`libajantv2` SDK 18.1.0 at commit
`3a23acd800cd05f40434ede8c714bd0271c94f53`. It does not install a driver.
The Rust `native-aja` feature loads its versioned C ABI at runtime, so a normal
Cargo build does not require the SDK or native DLL.

Build and optionally copy beside the validation executable:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/validation/build-aja-reference-bridge.ps1 -PackageDirectory target/debug
```

The script checks out the pinned official SDK under `target/native-deps`, or
accepts an existing exact clean checkout through `-SdkDirectory`. It builds with
Visual Studio x64, the static MSVC runtime, and no SDK plugins, demos, tools, or
driver. The DLL, an ABI probe, `native-build-receipt.json`, and
`native-abi-probe.json` live under `target/native/aja-reference-output`.
The probe only enumerates hardware and exercises invalid arguments and 128
rejected-open consuming shutdowns; its receipt always says physical NotRun.

The current provider supports SDI1, progressive 1080p and 720p, Rec.709 SDR,
legal-range v210, and 48 kHz signed 24-bit program Audio padded to the device's
8/16 embedded slots. Exact supported modes are enumerated from each card.
Admission uses serial identity, driver version, the device capability
fingerprint, and the existing configured external reference. It never silently
sets a missing reference, changes format to a nearby mode, or shares an active
card. Acquisition rejects an active AutoCirculate owner, enabled secondary
FrameStores, multiformat mode, and existing SDI color overrides. Configuration
is captured before mutation, read back after applying, and restored on all
consuming shutdown paths, including partial open failures.

A bounded native worker transfers canonical video and Audio together through
AutoCirculate. A completion requires the frame's user cookie to have actually
appeared in the hardware active-frame stamp and then been replaced; its clock
is the driver's 48 kHz device Audio clock. DMA submission is never completion.
Missed active-frame observations fail closed as drops. This is output-driver
evidence, not an independent SDI receiver or optical monitor measurement.

Normal discovery does not advertise ANC. Explicit validation wire configuration
can enable exact progressive luma VANC and independent SDI readback for one
matching signal only after discovering a distinct second card, verifying its
generation, observing its exact live SDI1 input, and checking the marker's actual
SMPTE line/offset against the SDK raster geometry. The capture card must already
have a live matching SDI route at admission. Missing cards or route stay NotRun.

The implementation writes complete canonical ST291 ten-bit words directly into
the full v210 VANC raster, preserving the exact luma horizontal coordinate;
the independent receiver DMA-captures a full VANC raster and scans actual words,
parity, checksum, count, and placement. It does not use GUMP, whose received data
omits horizontal offsets, or the SDK WriteVANCData helper, which ignores offsets.
HANC, chroma-channel ANC, HDR, 4K, interlaced formats, HDMI and IP remain outside
this provider's exact supported scope. DeckLink remains a separate provider item.

The phase factory creates a nonce and a canonical validation marker owner.
Every actual output frame carries its explicit nonce/frame packet; the input
must decode that same marker before a frame can be associated. Hardware DMA or
callback completion alone cannot supply the readback digest. The Rust join
requires exact equality of every received packet and rejects missing, extra,
reordered or repeated packets/frames and another campaign's marker. Create-only
bounded JSONL journals retain complete scheduled output words and independently
captured words, both digests and both hardware timestamps; consuming shutdown
syncs the journal and includes receiver/configuration-release failures. Native
capture starts with output playback, after preroll, so preroll rendering cannot
overflow an already running receiver. Raster probes are software-only evidence.
On 2026-09-06 the official Blackmagic developer page offered Desktop Video 16.0
SDK with download ID `d60730b1035946f388928c256a4a03f9`; its published download
metadata required both registration and terms. No DeckLink SDK is present on
this machine. An authorized SDK download is needed to implement and compile
that separate provider; an AJA successful build does not satisfy it.

The Rust DLL owner holds a read lease and SHA-256 identity for the loaded
image, resolves every expected symbol, and keeps the image alive through
session shutdown. An unproved callback-worker join retains the image instead
of unloading executing code. The outer Module applies its bounded consuming
shutdown coordinator and retains failures rather than reporting a timed-out
native owner as closed.

References: [official SDK](https://github.com/aja-video/libajantv2),
[AutoCirculate](https://sdkdocs.aja.com/public/ntv2/current/d9/d9a/recordplaytechniques.html).
Reference lock register interpretation comes from the pinned official
`driver/ntv2genlock.c` and `driver/ntv2genlock2.c`; only genlock versions 2/3
with supported readback are advertised.
