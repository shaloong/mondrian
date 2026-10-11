# Original CLAP NotePort metadata fixture

This original MIT-licensed fixture consumes Mondrian's existing official generated CLAP 1.2.10 ABI. It is an inactive/main-thread metadata oracle, not an instrument or audio effect. It has no audio ports; activation and processing reject. All descriptors are immutable static strings and each create owns one Box until native destroy. No other library, binary or algorithm is copied.

Root must integrate into the actual workspace before building (the scratch mirror deliberately lacks unrelated dependencies). Expected central build command:

`cargo build --manifest-path crates/mondrian-audio/tests/fixtures/clap-note-metadata/Cargo.toml --target-dir <fresh-fixture-target> --message-format=json`

No Cargo.lock has been generated or checked here. The only dependency is the existing path clap-sys; root must retain its actual compiler producer/features/source receipt and resulting DLL fingerprint. No build or native execution was performed by the candidate author.

The 16 definition IDs end in modes0..15: normal raw unknown dialect/high unsigned ID; absent extension; legitimate zero inputs; missing count; missing get; get=false; duplicate IDs; reserved ID; count33 (abort if get called); partially written second result (must start at zero); nonterminated name; restart during init; restart during count; restart during get; restart from serviced main callback; count0 with missing get. These are facts/negative controls, not compatibility workarounds in production.

Normal native state byte1 reverses index order while stable IDs persist; byte2 requests restart; an empty stream is an accepted native state0, distinct from no supplied state in response digest. Host thread-check must report main=true/audio=false, and HostNotePorts must be absent at init. The fixture cannot establish RT, DSP, output event support, author transaction/state fidelity or third-party compatibility.

Native selector: `processor_isolation::tests::native_clap_note_metadata::installed_clap_input_note_metadata_facts_and_refusals`. Requires current normal product `MONDRIAN_CLAP_TEST_HELPER` and current `MONDRIAN_CLAP_NOTE_METADATA_TEST_PLUGIN`, exact --ignored --exact --list inventory, two actual owner Jobs and existing central budgets/controllers. Do not run it directly without the normal fresh product and retained producer evidence.
