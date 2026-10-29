# CLAP output callback probe

This owned MIT fixture declares one stereo audio input/output and one note output
supporting CLAP and MIDI. In each process call it sends distinct notes (CLAP key
60, MIDI key 61), followed by their note-offs. Every callback result contributes
one bit to every PCM output sample. A host which returns true and discards all
four events therefore produces exactly 15; this host slice, which has no note
or MIDI output consumer, must return false and produce exactly 0.

This is an independent native producer oracle for callback acceptance, not a
reference musical synthesizer, performance PASS, or claim that note/MIDI support
has been implemented. The two protocols do not duplicate the same note. CLAP
note IDs advance without wrapping in the processor, and the declared output
port is real metadata rather than fabricated host capability.

Build with the canonical vendored Clack workspace only:

```powershell
cargo build --manifest-path crates/mondrian-audio/tests/fixtures/clap-output-probe/Cargo.toml --message-format=json-render-diagnostics
```

Let Cargo normally create this fixture's lockfile. Select the actual cdylib from
Cargo compiler-artifact JSON. Run the ignored native gate with a matching freshly
built `mondrian` helper and this exact artifact; record their hashes and commands.
Copyright (c) Shalom Wang. SPDX-License-Identifier: MIT.
