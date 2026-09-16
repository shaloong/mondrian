use super::*;

fn binding(frames: u64) -> CaptionImportBinding {
    CaptionImportBinding {
        source_start: TimelineTime::ZERO,
        output_frame_rate: Rational::new(60000, 1001),
        frame_count: frames,
        timecode_origin: TimelineTime::ZERO,
        placement: AncillaryPlacement::new(
            AncillarySpace::Vanc,
            AncillaryField::Progressive,
            20,
            0,
        )
        .expect("placement"),
    }
}
fn scc(text: &str, frames: u64) -> Result<FrozenAncillaryProgram, CaptionImportError> {
    import_caption_program(
        text.as_bytes(),
        CaptionSourceFormat::ScenaristSccV1,
        binding(frames),
    )
}
fn parity(value: u8) -> u8 {
    value
        | if value.count_ones().is_multiple_of(2) {
            0x80
        } else {
            0
        }
}
fn pair(a: u8, b: u8) -> [u8; 2] {
    [parity(a), parity(b)]
}
fn fix_sum(bytes: &mut [u8]) {
    let end = bytes.len() - 1;
    bytes[end] =
        0u8.wrapping_sub(bytes[..end].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
}

#[test]
fn scc_timed_pop_on_preserves_pairs_and_exact_output_grid_and_st436() {
    let input = "Scenarist_SCC V1.0\r\n\r\n00:00:00:00\t9420 9420 c849 942f 942f\r\n";
    let program = scc(input, 10).expect("SCC");
    program.validate().expect("source receipt revalidation");
    let receipt = program.caption_source().expect("provenance");
    assert_eq!(
        receipt.source_sha256,
        <[u8; 32]>::from(Sha256::digest(input.as_bytes()))
    );
    assert_eq!(
        (
            receipt.cea608_channels,
            receipt.cea608_pairs,
            receipt.cea708_packets
        ),
        (1, 5, 0)
    );
    for index in 0..10 {
        let frame = program.frame(index).expect("frame");
        assert_eq!(
            frame.packets()[0].validation,
            AncillaryValidationLevel::Transport
        );
        assert_eq!(frame.packets()[0].origin, AncillaryOrigin::Derived);
        let bytes = crate::encode_st436_ancillary(&frame).expect("ST436");
        assert_eq!(
            crate::reimport_st436_canonical(&frame, &bytes).expect("actual words").sha256(),
            frame.sha256()
        );
    }
    let text_pair = program.frame(4).expect("frame").packets()[0]
        .packet
        .payload_bytes()
        .expect("payload");
    assert_eq!(&text_pair[9..12], &[0xfc, 0xc8, 0x49]);
    assert!(scc(input, 8).is_err());
    let mut forged = serde_json::to_value(&program).expect("JSON");
    forged["caption_source"]["cea608_channels"] = 15.into();
    let forged: FrozenAncillaryProgram = serde_json::from_value(forged).expect("untrusted schema");
    assert!(forged.validate().is_err());
}

#[test]
fn scc_rejects_bad_parity_uninitialized_text_overlap_unknown_commands_and_versions() {
    for input in [
        "Scenarist_SCC V1.1\n00:00:00:00\t9420",
        "Scenarist_SCC V1.0\n00:00:00:00\t1420",
        "Scenarist_SCC V1.0\n00:00:00:00\tc849",
        "Scenarist_SCC V1.0\n00:00:00:00\t9420\n00:00:00:00\t9420",
        "Scenarist_SCC V1.0\n00:00:00:00\t942a",
        "Scenarist_SCC V1.0\n00:00:00:00 9420",
        "Scenarist_SCC V1.0\n00:00:00:00\t94200",
        "Scenarist_SCC V1.0\n00:01:00;00\t9420",
        "Scenarist_SCC V1.0\n00:00:00:00\t9420\n00:00:00;01\t9420",
    ] {
        assert!(scc(input, 120).is_err(), "accepted {input:?}");
    }
    let mut unbounded = binding(MAX_FRAMES + 1);
    assert!(import_caption_program(b"x", CaptionSourceFormat::ScenaristSccV1, unbounded).is_err());
    unbounded = binding(1);
    unbounded.output_frame_rate = Rational::new(24000, 1001);
    assert!(import_caption_program(b"x", CaptionSourceFormat::ScenaristSccV1, unbounded).is_err());
    assert!(import_caption_program(
        &vec![0; MAX_BYTES + 1],
        CaptionSourceFormat::ScenaristSccV1,
        binding(1)
    )
    .is_err());
}

#[test]
fn scc_dropframe_and_origin_are_exact_without_rounding_or_silent_trimming() {
    let input = b"Scenarist_SCC V1.0\n00:01:00;02\t9420";
    let mut selected = binding(2);
    selected.source_start =
        TimelineTime::from_frame_position(FramePosition::new(1800, Rational::new(1001, 30000)))
            .expect("exact");
    import_caption_program(input, CaptionSourceFormat::ScenaristSccV1, selected)
        .expect("first valid drop label");
    selected.timecode_origin = selected.source_start;
    selected.source_start = TimelineTime::ZERO;
    import_caption_program(input, CaptionSourceFormat::ScenaristSccV1, selected)
        .expect("timecode origin");
    selected.source_start = TimelineTime::new(1, 60000).expect("off-grid");
    assert!(import_caption_program(input, CaptionSourceFormat::ScenaristSccV1, selected).is_err());
}

#[test]
fn cea608_channel_control_duplicate_and_column_boundaries() {
    let mut decoder = cea608::Decoder::default();
    decoder.push(0, pair(0x1c, 0x20)).expect("CC2 RCL");
    assert_eq!(
        decoder.push(0, pair(0x41, 0x42)).expect("CC2 text"),
        Cea608Operation::Text { channel: 2, characters: [0x41, 0x42] }
    );
    decoder.push(1, pair(0x1d, 0x25)).expect("CC4 rollup");
    decoder.push(1, pair(0x41, 0x42)).expect("text");
    decoder.push(1, pair(0x1d, 0x2d)).expect("CR");
    assert_eq!(decoder.channels, 0b1010);
    let mut decoder = cea608::Decoder::default();
    decoder.push(0, pair(0x14, 0x20)).expect("RCL");
    for _ in 0..16 {
        decoder.push(0, pair(0x41, 0x42)).expect("32 columns");
    }
    assert!(decoder.push(0, pair(0x41, 0)).is_err());
    let mut decoder = cea608::Decoder::default();
    decoder.push(0, pair(0x14, 0x20)).expect("RCL");
    decoder.push(0, pair(0x41, 0x42)).expect("AB");
    decoder.push(0, pair(0x14, 0x21)).expect("BS");
    decoder.push(0, pair(0x14, 0x21)).expect("repeated BS ignored");
    decoder.push(0, pair(0x12, 0x20)).expect("extended replaces remaining A");
    assert!(cea608::Decoder::default().push(0, pair(0x14, 0x2f)).is_err());
    assert!(cea608::Decoder::default().push(0, pair(0x01, 0x02)).is_err());
}

#[test]
fn raw_cdp_reassembles_708_across_frames_wraps_sequences_and_retains_services() {
    let first = make_cdp(u16::MAX, 0, [0x80, 0x80], &[[0xff, 0xc2, 0x21]]);
    let second = make_cdp(
        0,
        1,
        [0x80, 0x80],
        &[
            [0xfe, 0x41, 0],
            [0xff, 0x03, 0xe1],
            [0xfe, 0x3f, 0x42],
            [0xfe, 0, 0],
        ],
    );
    let program = import_caption_program(
        &[first, second].concat(),
        CaptionSourceFormat::RawCdpSt334_2_2015,
        binding(2),
    )
    .expect("real reassembly");
    program.validate().expect("recheck");
    let receipt = program.caption_source().expect("source");
    assert_eq!(receipt.cea708_packets, 2);
    assert_eq!(receipt.cea708_services, [1, 63]);
    assert_eq!(
        program.frame(0).expect("frame").packets()[0].origin,
        AncillaryOrigin::Preserved
    );
}

#[test]
fn cdp_valid_envelope_cannot_hide_bad_sections_phase_marker_or_footer() {
    let original = make_cdp(0, 0, [0x80, 0x80], &[]);
    for (offset, value) in [
        (3, 0x6f),
        (4, 0xc3),
        (7, 0x73),
        (8, 0xe9),
        (9, 0x7c),
        (12, 0xff),
        (39, 0x75),
        (40, 1),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        fix_sum(&mut bytes);
        assert!(
            CaptionDistributionPacket::from_bytes(bytes.clone()).is_ok(),
            "outer envelope"
        );
        assert!(
            import_caption_program(&bytes, CaptionSourceFormat::RawCdpSt334_2_2015, binding(1))
                .is_err(),
            "offset {offset}"
        );
    }
    for second in [
        make_cdp(2, 1, [0x80, 0x80], &[]),
        make_cdp(1, 0, [0x80, 0x80], &[]),
    ] {
        assert!(import_caption_program(
            &[original.clone(), second].concat(),
            CaptionSourceFormat::RawCdpSt334_2_2015,
            binding(2)
        )
        .is_err());
    }
    for end in 0..original.len() {
        assert!(import_caption_program(
            &original[..end],
            CaptionSourceFormat::RawCdpSt334_2_2015,
            binding(1)
        )
        .is_err());
    }
}

#[test]
fn cea708_rejects_discontinuity_truncation_service_overflow_and_unknown_commands() {
    for triplets in [
        vec![[0xfe, 0, 0]],                                      // orphan continuation
        vec![[0xff, 2, 0x21]],                                   // unfinished packet
        vec![[0xff, 2, 0x21], [0xff, 0x42, 0x21]],               // new start before completion
        vec![[0xff, 2, 0x23], [0xfe, 0x41, 0]],                  // block exceeds packet
        vec![[0xff, 2, 0x21], [0xfe, 0x98, 0]], // window command truncated within block
        vec![[0xff, 2, 0x21], [0xfe, 0x10, 0]], // unsupported EXT1
        vec![[0xff, 3, 0xe1], [0xfe, 0x06, 0x41], [0xfe, 0, 0]], // invalid extended service
        vec![
            [0xff, 2, 0x21],
            [0xfe, 0x41, 0],
            [0xff, 0x82, 0x21],
            [0xfe, 0x42, 0],
        ], // missed sequence
    ] {
        let bytes = make_cdp(0, 0, [0x80, 0x80], &triplets);
        assert!(
            import_caption_program(&bytes, CaptionSourceFormat::RawCdpSt334_2_2015, binding(1))
                .is_err(),
            "{triplets:?}"
        );
    }
    let mut max = cea708::Decoder::default();
    max.push(3, [0, 0]).expect("128 byte size encoding");
    for _ in 0..63 {
        max.push(2, [0, 0]).expect("bounded padding packet");
    }
    max.finish().expect("exact 128 bytes");
    assert_eq!(max.packets, 1);
    assert!(max.push(2, [0, 0]).is_err());
}

#[test]
#[ignore = "requires official BMX 1.6 via MONDRIAN_ST436_BMX_TOOL_DIR"]
fn caption_scc_and_708_cdp_survive_official_bmx_st436_final_mxf_rescan() {
    use std::process::Command;
    let directory = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_ST436_BMX_TOOL_DIR").expect("explicit BMX"),
    );
    let wrapper = directory.join(format!("raw2bmx{}", std::env::consts::EXE_SUFFIX));
    let version = Command::new(&wrapper).arg("--version").output().expect("version");
    assert!(version.status.success());
    assert!(format!(
        "{}{}",
        String::from_utf8_lossy(&version.stdout),
        String::from_utf8_lossy(&version.stderr)
    )
    .contains("bmx v1.6.0"));
    let programs = [
        scc("Scenarist_SCC V1.0\n00:00:00:00\t9420 c849 942f", 6).expect("SCC"),
        import_caption_program(
            &make_cdp(0, 0, [0x80, 0x80], &[[0xff, 2, 0x21], [0xfe, 0x41, 0]]),
            CaptionSourceFormat::RawCdpSt334_2_2015,
            binding(1),
        )
        .expect("CDP"),
    ];
    for program in programs {
        let work = tempfile::tempdir().expect("work");
        let input = work.path().join("caption.klv");
        let output = work.path().join("caption.mxf");
        let mut file = std::fs::File::create_new(&input).expect("new input");
        for index in 0..program.frame_count() {
            crate::write_st436_klv_frame(&mut file, &program.frame(index).expect("canonical"))
                .expect("KLV");
        }
        drop(file);
        let wrapped = Command::new(&wrapper)
            .args([
                "-t",
                "op1a",
                "-f",
                "5994",
                "--dur",
                &program.frame_count().to_string(),
                "-o",
            ])
            .arg(&output)
            .args(["--klv", "s", "--anc"])
            .arg(&input)
            .output()
            .expect("wrapper");
        assert!(
            wrapped.status.success(),
            "{}",
            String::from_utf8_lossy(&wrapped.stderr)
        );
        let actual = std::fs::read(output).expect("actual MXF");
        assert_eq!(
            program
                .verify_mxf(&mut actual.as_slice(), actual.len() as u64)
                .expect("independent actual packet words"),
            program.frame_count()
        );
    }
}
