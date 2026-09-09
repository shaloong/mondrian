# DeckLink API 12.0 native reference bridge

Build explicitly with `scripts/validation/build-decklink-reference-bridge.ps1`.
Ordinary Cargo builds do not run MIDL, require Desktop Video, or link this DLL.
The script verifies all 27 original BMD interface hashes/notices and uses
Microsoft MIDL and MSVC x64, then runs three nonqualifying native probes.
No SDK download, registration, driver install or system configuration is done.
The source pin and redistribution notices are in `vendor/README.md`.

`bridge.h` is C ABI 1. Discovery returns real COM persistent IDs, physical-card
group IDs, current profile generations, separate input/output mode masks, and
installed runtime API version. Missing COM registration is status `-2`;
missing API 12.0 interfaces are `-4`; a vanished exact device is `-3`.
Discovery with zero devices never admits a physical session.

Each output and receiver has a dedicated MTA owner thread. Scheduled output
uses native v210, progressive HD SDR Rec709, single-link SDI, exact timestamps,
48kHz signed PCM24 promoted to SDK PCM32, and unity digital-audio scale.
It preserves/restores every changed nonpersistent configuration value and
checks readback. Audio short writes, late/dropped frames, callback overflow,
generation changes and required reference-lock loss fail the owner.
Actual SDK frame-completion and capture clocks use 48000 ticks per second.

Independent input callbacks retain actual SDK frame references. Raw captured
v210 VANC rows supply original luma ADF/header/data/checksum words and offsets.
The supported carriage scope is progressive HD pre-active VANC (1080 lines
1–41 or 720 lines 1–25, subject to SDK line availability), type-2 packets,
64 packets/frame, 262 words/packet. HANC, chroma ANC, interlace, UHD, HDR,
automatic pixel conversion and vendor packet reserialization are rejected.
The Rust receiver binding must compare physical group IDs, not merely port IDs.
Raw DMA or scheduling success is never reception evidence: qualification needs
an independently wired input and the owner's in-band nonce/frame marker.

Every non-null failed-open owner must be consumed. Initialization has a five
second caller deadline and retains its promise/owner if the SDK outlives that
deadline. Stop requests only set an atomic signal: SDK waits run on the owner
thread. The Rust Module coordinator independently bounds consuming shutdown
and retains the complete owner when its wait expires.

Shutdown reports stopped, callback/worker termination, released, outstanding
frames/resources, and errors. If native execution does not quiesce, COM,
callbacks, frame/audio leases and the MTA thread are quarantined together;
the thread parks on a condition variable without polling. The executable
mapping remains retained and the receipt is explicitly failed. Deterministic
production-owner fault probes cover unstarted owners, initialization timeout,
nonblocking stop, blocked-stop retention and parked quarantine. These tests,
the no-device probe and byte-codec probe assert no hardware qualification.
