use super::*;
use crate::preset::{
    ExportChromaSampling, ProfessionalDeliveryMetadata, ProfessionalDeliveryOutput,
    ProfessionalDeliveryProfile, Resolution,
};
use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
use mondrian_timeline::sequence::{DeliveryBitDepth, VideoRange};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn output(profile: ProfessionalDeliveryProfile) -> ProfessionalDeliveryOutput {
    ProfessionalDeliveryOutput {
        profile,
        metadata: ProfessionalDeliveryMetadata::default(),
    }
}

#[test]
fn exact_product_rows_resolve_without_generic_profile_inference() {
    let rows = [
        (
            ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25,
            Resolution { width: 1920, height: 1080 },
            Rational::FPS_25,
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv422,
            ColorSpace::Rec709,
            DeliverableLayout::ImmutableDirectory,
            ProfessionalEssenceKind::ProRes422Hq,
        ),
        (
            ProfessionalDeliveryProfile::As11X9NabaHd720p5994,
            Resolution { width: 1280, height: 720 },
            Rational::FPS_5994,
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv422,
            ColorSpace::Rec709,
            DeliverableLayout::SingleMxfFile,
            ProfessionalEssenceKind::AvcHigh422Intra,
        ),
        (
            ProfessionalDeliveryProfile::SmpteDcp2kFlat24,
            Resolution { width: 1998, height: 1080 },
            Rational::FPS_24,
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::LinearRec709,
            DeliverableLayout::ImmutableDirectory,
            ProfessionalEssenceKind::DcdmXyzJpeg2000,
        ),
    ];
    for (profile, resolution, rate, depth, range, chroma, color, layout, essence) in rows {
        let resolved = resolve_professional_delivery(
            &output(profile),
            resolution,
            rate,
            depth,
            range,
            chroma,
            color,
            AudioChannelLayout::Stereo,
        )
        .expect("exact profile row");
        assert_eq!(resolved.layout, layout);
        assert_eq!(resolved.picture_essence, essence);
    }
}

#[test]
fn professional_presets_round_trip_as_closed_author_contracts() {
    for preset in [
        crate::preset::ExportPreset::imf_app_prores_rdd45_1080p25(),
        crate::preset::ExportPreset::as11_x9_naba_hd_720p5994(),
        crate::preset::ExportPreset::smpte_dcp_2k_flat_24(),
    ] {
        let json = serde_json::to_string(&preset).expect("serialize professional preset");
        let decoded: crate::preset::ExportPreset =
            serde_json::from_str(&json).expect("deserialize professional preset");
        assert_eq!(decoded, preset);
    }
}

#[test]
fn profile_admission_fails_closed_on_cadence_signal_color_and_audio_layout() {
    let request = output(ProfessionalDeliveryProfile::SmpteDcp2kFlat24);
    let base = || {
        resolve_professional_delivery(
            &request,
            Resolution { width: 1998, height: 1080 },
            Rational::FPS_24,
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::LinearRec709,
            AudioChannelLayout::Stereo,
        )
    };
    assert!(base().is_ok());
    assert!(matches!(
        resolve_professional_delivery(
            &request,
            Resolution { width: 2048, height: 1080 },
            Rational::FPS_24,
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::LinearRec709,
            AudioChannelLayout::Stereo,
        ),
        Err(ProfessionalDeliveryAdmissionError::Resolution { .. })
    ));
    assert!(matches!(
        resolve_professional_delivery(
            &request,
            Resolution { width: 1998, height: 1080 },
            Rational::FPS_25,
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::LinearRec709,
            AudioChannelLayout::Stereo,
        ),
        Err(ProfessionalDeliveryAdmissionError::FrameRate { .. })
    ));
    assert!(matches!(
        resolve_professional_delivery(
            &request,
            Resolution { width: 1998, height: 1080 },
            Rational::FPS_24,
            DeliveryBitDepth::Ten,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::LinearRec709,
            AudioChannelLayout::Stereo,
        ),
        Err(ProfessionalDeliveryAdmissionError::Signal { .. })
    ));
    assert!(matches!(
        resolve_professional_delivery(
            &request,
            Resolution { width: 1998, height: 1080 },
            Rational::FPS_24,
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::Rec709,
            AudioChannelLayout::Stereo,
        ),
        Err(ProfessionalDeliveryAdmissionError::ColorTarget { .. })
    ));
    assert!(matches!(
        resolve_professional_delivery(
            &request,
            Resolution { width: 1998, height: 1080 },
            Rational::FPS_24,
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Rgb,
            ColorSpace::LinearRec709,
            AudioChannelLayout::Surround51Side,
        ),
        Err(ProfessionalDeliveryAdmissionError::AudioLayout(_))
    ));
}

#[test]
fn package_paths_reject_traversal_and_alternate_separators() {
    for unsafe_path in [
        "",
        "../track.mxf",
        "nested/track.mxf",
        "nested\\track.mxf",
        "/track.mxf",
    ] {
        assert!(matches!(
            PackageRelativePath::new(unsafe_path),
            Err(ProfessionalPackageError::UnsafePath(_))
        ));
    }
    assert_eq!(
        PackageRelativePath::new("picture.mxf").expect("safe path").as_str(),
        "picture.mxf"
    );
}

#[test]
fn dcdm_encoder_uses_st428_transfer_and_msb_aligned_12_bit_words() {
    let black =
        encode_linear_rec709_as_dcdm_xyz12le(&[0.0, 0.0, 0.0, 1.0]).expect("black DCDM pixel");
    assert_eq!(black, vec![0; 6]);

    let white =
        encode_linear_rec709_as_dcdm_xyz12le(&[1.0, 1.0, 1.0, 1.0]).expect("white DCDM pixel");
    let codes: Vec<_> = white
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]) >> 4)
        .collect();
    assert_eq!(codes.len(), 3);
    assert!(codes[0] > 3800 && codes[0] < 4000);
    assert!(codes[1] > 3950 && codes[1] < 4050);
    assert_eq!(codes[2], 4092);
    assert!(white.chunks_exact(2).all(|word| word[0] & 0x0f == 0));
}

#[test]
fn package_build_reimport_and_digest_tamper_are_independently_verified() {
    for profile in [
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25,
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24,
    ] {
        let root = tempfile::tempdir().expect("package root");
        std::fs::write(root.path().join("picture.mxf"), b"qualified-picture-track")
            .expect("picture track");
        std::fs::write(root.path().join("audio.mxf"), b"qualified-audio-track")
            .expect("audio track");
        let request = ProfessionalPackageBuildRequest {
            profile,
            metadata: ProfessionalDeliveryMetadata::default(),
            composition_id: CompositionPlaylistId::new(),
            packing_list_id: PackingListId::new(),
            issued_at: chrono::Utc::now(),
            document_ids: ProfessionalPackageDocumentIds::new(),
            picture: ProfessionalPackageTrack {
                id: PackageAssetId::new(),
                path: PackageRelativePath::new("picture.mxf").expect("picture path"),
                role: PackageAssetRole::PictureTrack,
                imf: (profile == ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25)
                    .then(|| ImfTrackMetadata {
                        essence_descriptor_id: PackageElementId::new(),
                        essence_descriptor_xml: "<r0:CDCIDescriptor xmlns:r0=\"http://www.smpte-ra.org/reg/395/2014/13/1/aaf\"/>".to_owned(),
                        edit_rate: Rational::FPS_25,
                        intrinsic_duration: 48,
                        source_duration: 48,
                    }),
            },
            audio: Some(ProfessionalPackageTrack {
                id: PackageAssetId::new(),
                path: PackageRelativePath::new("audio.mxf").expect("audio path"),
                role: PackageAssetRole::AudioTrack,
                imf: (profile == ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25)
                    .then(|| ImfTrackMetadata {
                        essence_descriptor_id: PackageElementId::new(),
                        essence_descriptor_xml: "<r0:WaveAudioDescriptor xmlns:r0=\"http://www.smpte-ra.org/reg/395/2014/13/1/aaf\"/>".to_owned(),
                        edit_rate: Rational::new(48_000, 1),
                        intrinsic_duration: 92_160,
                        source_duration: 92_160,
                    }),
            }),
            edit_rate: if profile == ProfessionalDeliveryProfile::SmpteDcp2kFlat24 {
                Rational::FPS_24
            } else {
                Rational::FPS_25
            },
            duration: 48,
        };
        let validated = build_and_validate_package(root.path(), &request).expect("valid package");
        assert_eq!(validated.profile(), profile);
        assert_eq!(validated.inventory().assets.len(), 5);
        reimport_and_validate_package(root.path(), profile).expect("reimport package");

        let asset_map_path = root.path().join("ASSETMAP.xml");
        let asset_map = std::fs::read_to_string(&asset_map_path).expect("read AssetMap");
        std::fs::write(
            &asset_map_path,
            asset_map.replacen("<Offset>0</Offset>", "<Offset>1</Offset>", 1),
        )
        .expect("tamper AssetMap offset");
        assert!(matches!(
            reimport_and_validate_package(root.path(), profile),
            Err(ProfessionalPackageError::InvalidGraph(_))
        ));
        std::fs::write(&asset_map_path, asset_map).expect("restore AssetMap");

        let cpl_path = root.path().join("CPL.xml");
        let cpl = std::fs::read_to_string(&cpl_path).expect("read CPL");
        let tampered_cpl = if profile == ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 {
            cpl.replacen(
                "<SourceDuration>48</SourceDuration>",
                "<SourceDuration>47</SourceDuration>",
                1,
            )
        } else {
            cpl.replacen("<Duration>48</Duration>", "<Duration>47</Duration>", 1)
        };
        std::fs::write(&cpl_path, tampered_cpl).expect("tamper CPL timing");
        assert!(matches!(
            reimport_and_validate_package(root.path(), profile),
            Err(ProfessionalPackageError::InvalidGraph(_))
                | Err(ProfessionalPackageError::InvalidInventory(_))
        ));
        std::fs::write(&cpl_path, cpl).expect("restore CPL");

        let mut picture = std::fs::OpenOptions::new()
            .append(true)
            .open(root.path().join("picture.mxf"))
            .expect("open picture");
        picture.write_all(b"tamper").expect("tamper picture");
        assert!(matches!(
            reimport_and_validate_package(root.path(), profile),
            Err(ProfessionalPackageError::InvalidInventory(_))
        ));
    }
}

#[test]
fn package_reimport_rejects_dtd_and_entity_declarations() {
    let root = tempfile::tempdir().expect("package root");
    std::fs::write(root.path().join("ASSETMAP.xml"), "<!DOCTYPE x><AssetMap/>")
        .expect("malicious asset map");
    std::fs::write(root.path().join("PKL.xml"), "<PackingList/>").expect("PKL");
    std::fs::write(root.path().join("CPL.xml"), "<CompositionPlaylist/>").expect("CPL");
    assert!(matches!(
        reimport_and_validate_package(root.path(), ProfessionalDeliveryProfile::SmpteDcp2kFlat24),
        Err(ProfessionalPackageError::InvalidXml(_))
    ));
}

#[test]
#[ignore = "requires the isolated COL-038 BMX, Photon, JDK, and FFmpeg qualification tools"]
fn real_imf_rdd45_track_files_and_package_pass_photon_reimport() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let tool_root = workspace.join("target/col038-tools");
    let raw2bmx = tool_root.join("bmx/bmx-win64-binary-1.6/bin/raw2bmx.exe");
    let mxf2raw = tool_root.join("bmx/bmx-win64-binary-1.6/bin/mxf2raw.exe");
    let java = tool_root.join("microsoft-jdk/jdk-21.0.12.1+1/bin/java.exe");
    let photon_lib = tool_root.join("photon/build/libs");
    if [&raw2bmx, &mxf2raw, &java].iter().any(|path| !path.is_file()) || !photon_lib.is_dir() {
        eprintln!("COL-038 qualification tools are not present; skipping local HITL fixture");
        return;
    }
    let root = tempfile::tempdir().expect("qualification root");
    let package = root.path().join("package");
    let work = root.path().join("work");
    std::fs::create_dir(&package).expect("package directory");
    std::fs::create_dir(&work).expect("work directory");
    let prores = work.join("picture.prores");
    run_qualification_command(
        mondrian_media::ffmpeg_command()
            .expect("admit professional qualification fixture command")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("color=black:s=1920x1080:r=25:d=0.04")
            .args([
                "-frames:v",
                "1",
                "-c:v",
                "prores_ks",
                "-profile:v",
                "3",
                "-pix_fmt",
                "yuv422p10le",
                "-f",
                "rawvideo",
            ])
            .arg(&prores),
        "FFmpeg ProRes fixture",
    );
    let wave = work.join("audio.wav");
    run_qualification_command(
        mondrian_media::ffmpeg_command()
            .expect("admit professional qualification fixture command")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("anullsrc=channel_layout=stereo:sample_rate=48000")
            .args([
                "-t",
                "0.04",
                "-c:a",
                "pcm_s24le",
                "-ar",
                "48000",
                "-ac",
                "2",
            ])
            .arg(&wave),
        "FFmpeg PCM fixture",
    );
    let labels = work.join("mca.txt");
    std::fs::write(
        &labels,
        "0\nchL, chan=0\nchR, chan=1\nsgST, lang=en-US, mcaaudiocontentkind=PRM, mcaaudioelementkind=FCMP, mcatitle=Mondrian, mcatitleversion=1\n",
    )
    .expect("MCA labels");
    let toolchain =
        ProfessionalDeliveryToolchain::from_paths_with_photon(raw2bmx, mxf2raw, java, photon_lib);
    toolchain
        .qualify_for(ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25)
        .expect("qualified IMF toolchain");
    run_qualification_command(
        &mut toolchain.imf_picture_command(
            &prores,
            &package.join("picture_{fp_uuid}.mxf"),
            "Mondrian qualification",
        ),
        "BMX IMF picture",
    );
    run_qualification_command(
        &mut toolchain.imf_audio_command(&wave, &labels, &package.join("audio_{fp_uuid}.mxf")),
        "BMX IMF audio",
    );
    let (picture_path, picture_id) = find_qualification_track(&package, "picture_");
    let (audio_path, audio_id) = find_qualification_track(&package, "audio_");
    let picture_descriptor = photon_descriptor(&toolchain, &picture_path, &work.join("picture"));
    let audio_descriptor = photon_descriptor(&toolchain, &audio_path, &work.join("audio"));
    let request = ProfessionalPackageBuildRequest {
        profile: ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25,
        metadata: ProfessionalDeliveryMetadata::default(),
        composition_id: CompositionPlaylistId::new(),
        packing_list_id: PackingListId::new(),
        issued_at: chrono::DateTime::parse_from_rfc3339("2026-08-30T00:00:00Z")
            .expect("issue date")
            .with_timezone(&chrono::Utc),
        document_ids: ProfessionalPackageDocumentIds::new(),
        picture: ProfessionalPackageTrack {
            id: picture_id,
            path: PackageRelativePath::new(
                picture_path.file_name().and_then(|name| name.to_str()).expect("picture name"),
            )
            .expect("picture path"),
            role: PackageAssetRole::PictureTrack,
            imf: Some(ImfTrackMetadata {
                essence_descriptor_id: PackageElementId::new(),
                essence_descriptor_xml: picture_descriptor,
                edit_rate: Rational::FPS_25,
                intrinsic_duration: 1,
                source_duration: 1,
            }),
        },
        audio: Some(ProfessionalPackageTrack {
            id: audio_id,
            path: PackageRelativePath::new(
                audio_path.file_name().and_then(|name| name.to_str()).expect("audio name"),
            )
            .expect("audio path"),
            role: PackageAssetRole::AudioTrack,
            imf: Some(ImfTrackMetadata {
                essence_descriptor_id: PackageElementId::new(),
                essence_descriptor_xml: audio_descriptor,
                edit_rate: Rational::new(48_000, 1),
                intrinsic_duration: 1_920,
                source_duration: 1_920,
            }),
        }),
        edit_rate: Rational::FPS_25,
        duration: 1,
    };
    build_and_validate_package(&package, &request).expect("Mondrian package validation");
    let photon = run_qualification_command(
        &mut toolchain.photon_imp_validation_command(&package),
        "Photon IMP validation",
    );
    assert!(!photon.contains("FATAL"), "{photon}");
    assert!(!photon.contains("ERROR"), "{photon}");
}

#[test]
#[ignore = "requires the isolated COL-038 asdcplib, DCP-o-matic, and FFmpeg qualification tools"]
fn real_smpte_dcp_tracks_and_package_pass_independent_verifier() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let tool_root = workspace.join("target/col038-tools");
    let asdcp_wrap = tool_root.join("asdcplib-build/src/asdcp-wrap.exe");
    let asdcp_info = tool_root.join("asdcplib-build/src/asdcp-info.exe");
    let dcp_verifier = tool_root.join("dcpomatic/bin/dcpomatic2_verify_cli.exe");
    if [&asdcp_wrap, &asdcp_info, &dcp_verifier].iter().any(|path| !path.is_file()) {
        eprintln!("COL-038 DCP qualification tools are not present; skipping local HITL fixture");
        return;
    }
    let toolchain =
        ProfessionalDeliveryToolchain::from_paths_with_dcp(asdcp_wrap, asdcp_info, dcp_verifier);
    toolchain
        .qualify_for(ProfessionalDeliveryProfile::SmpteDcp2kFlat24)
        .expect("qualified DCP toolchain");
    let root = tempfile::tempdir().expect("qualification root");
    let package = root.path().join("package");
    let work = root.path().join("work");
    let j2c = work.join("j2c");
    std::fs::create_dir(&package).expect("package directory");
    std::fs::create_dir_all(&j2c).expect("J2C directory");
    run_qualification_command(
        mondrian_media::ffmpeg_command()
            .expect("admit professional qualification fixture command")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("color=black:s=1998x1080:r=24:d=1")
            .args([
                "-frames:v",
                "24",
                "-vf",
                "format=xyz12le",
                "-c:v",
                "libopenjpeg",
                "-format",
                "j2k",
                "-profile:v",
                "cinema2k",
                "-cinema_mode",
                "2k_24",
                "-pix_fmt",
                "xyz12le",
                "-f",
                "image2",
            ])
            .arg(j2c.join("frame_%06d.j2c")),
        "FFmpeg DCDM JPEG 2000 fixture",
    );
    let wave = work.join("audio.wav");
    run_qualification_command(
        mondrian_media::ffmpeg_command()
            .expect("admit professional qualification fixture command")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("anullsrc=channel_layout=stereo:sample_rate=48000")
            .args(["-t", "1", "-c:a", "pcm_s24le", "-ar", "48000", "-ac", "2"])
            .arg(&wave),
        "FFmpeg DCP PCM fixture",
    );
    let picture_id = PackageAssetId::new();
    let audio_id = PackageAssetId::new();
    let picture = package.join("picture.mxf");
    let audio = package.join("audio.mxf");
    run_qualification_command(
        &mut toolchain.dcp_picture_command(&j2c, &picture, picture_id, 24),
        "AS-DCP picture wrapping",
    );
    run_qualification_command(
        &mut toolchain.dcp_audio_command(&wave, &audio, audio_id, 24, "en-US"),
        "AS-DCP audio wrapping",
    );
    for track in [&picture, &audio] {
        run_qualification_command(
            &mut toolchain.asdcp_reimport_command(track),
            "AS-DCP Track File reimport",
        );
    }
    let request = ProfessionalPackageBuildRequest {
        profile: ProfessionalDeliveryProfile::SmpteDcp2kFlat24,
        metadata: ProfessionalDeliveryMetadata::default(),
        composition_id: CompositionPlaylistId::new(),
        packing_list_id: PackingListId::new(),
        issued_at: chrono::DateTime::parse_from_rfc3339("2026-08-30T00:00:00Z")
            .expect("issue date")
            .with_timezone(&chrono::Utc),
        document_ids: ProfessionalPackageDocumentIds::new(),
        picture: ProfessionalPackageTrack {
            id: picture_id,
            path: PackageRelativePath::new("picture.mxf").expect("picture path"),
            role: PackageAssetRole::PictureTrack,
            imf: None,
        },
        audio: Some(ProfessionalPackageTrack {
            id: audio_id,
            path: PackageRelativePath::new("audio.mxf").expect("audio path"),
            role: PackageAssetRole::AudioTrack,
            imf: None,
        }),
        edit_rate: Rational::FPS_24,
        duration: 24,
    };
    build_and_validate_package(&package, &request).expect("Mondrian DCP package validation");
    let verifier = run_dcp_validation_command(
        &mut toolchain.dcp_package_validation_command(&package),
        "DCP-o-matic package validation",
    );
    assert!(
        !verifier.lines().any(|line| line.trim_start().starts_with("Error:")),
        "{verifier}"
    );
}

#[test]
#[ignore = "requires the isolated COL-038 BMX and FFmpeg qualification tools"]
fn real_as11_x9_file_passes_bmx_structural_and_metadata_reimport() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let tool_root = workspace.join("target/col038-tools");
    let raw2bmx = tool_root.join("bmx/bmx-win64-binary-1.6/bin/raw2bmx.exe");
    let mxf2raw = tool_root.join("bmx/bmx-win64-binary-1.6/bin/mxf2raw.exe");
    if [&raw2bmx, &mxf2raw].iter().any(|path| !path.is_file()) {
        eprintln!("COL-038 AS-11 qualification tools are not present; skipping local HITL fixture");
        return;
    }
    let toolchain =
        ProfessionalDeliveryToolchain::from_paths(raw2bmx, mxf2raw, PathBuf::new(), PathBuf::new());
    toolchain
        .qualify_for(ProfessionalDeliveryProfile::As11X9NabaHd720p5994)
        .expect("qualified AS-11 toolchain");
    let root = tempfile::tempdir().expect("qualification root");
    let avc = root.path().join("picture.h264");
    run_qualification_command(
        mondrian_media::ffmpeg_command()
            .expect("admit professional qualification fixture command")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("color=black:s=1280x720:r=60000/1001:d=1.001")
            .args([
                "-frames:v",
                "60",
                "-vf",
                "format=yuv422p10le",
                "-c:v",
                "libx264",
                "-profile:v",
                "high422",
                "-level:v",
                "4.1",
                "-g",
                "1",
                "-keyint_min",
                "1",
                "-sc_threshold",
                "0",
                "-bf",
                "0",
                "-f",
                "h264",
            ])
            .arg(&avc),
        "FFmpeg AS-11 AVC fixture",
    );
    let wave = root.path().join("audio.wav");
    run_qualification_command(
        mondrian_media::ffmpeg_command()
            .expect("admit professional qualification fixture command")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("anullsrc=channel_layout=stereo:sample_rate=48000")
            .args([
                "-t",
                "1.001",
                "-c:a",
                "pcm_s24le",
                "-ar",
                "48000",
                "-ac",
                "2",
            ])
            .arg(&wave),
        "FFmpeg AS-11 PCM fixture",
    );
    let labels = root.path().join("mca.txt");
    std::fs::write(
        &labels,
        "0\nchL, chan=0\nchR, chan=1\nsgST, lang=en-US, mcaaudiocontentkind=PRM, mcaaudioelementkind=FCMP, mcatitle=Mondrian, mcatitleversion=1\n",
    )
    .expect("MCA labels");
    let output = root.path().join("delivery.mxf");
    run_qualification_command(
        &mut toolchain.as11_x9_command(&avc, &wave, &labels, &output, "Mondrian qualification"),
        "BMX AS-11 X9 wrapping",
    );
    let inspection = run_qualification_command(
        &mut toolchain.bmx_reimport_command(&output, true),
        "BMX AS-11 X9 reimport",
    );
    for required in [
        "op_label        : OP1A",
        "edit_rate       : 60000/1001",
        "essence_type    : AVC_High_422_Intra",
        "component_depth : 10",
        "channel_count        : 2",
        "bits_per_sample      : 24",
        "spec_identifier : urn:smpte:ul:060e2b34.04010101.0d010801.05090000",
        "is_complete     : true",
        "last_frame      : true",
    ] {
        assert!(
            inspection.contains(required),
            "missing {required:?}: {inspection}"
        );
    }
}

fn run_qualification_command(command: &mut Command, label: &str) -> String {
    let output = command.output().unwrap_or_else(|error| panic!("{label} spawn failed: {error}"));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{label} failed: {text}");
    text
}

fn run_dcp_validation_command(command: &mut Command, label: &str) -> String {
    let output = command.output().unwrap_or_else(|error| panic!("{label} spawn failed: {error}"));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success()
            || text.lines().all(|line| {
                let line = line.trim_start();
                line.is_empty()
                    || line.starts_with("Bv2.1 error:")
                    || line.starts_with("Warning:")
                    || line.starts_with('┣')
                    || line.starts_with('┗')
            }),
        "{label} failed: {text}"
    );
    text
}

fn find_qualification_track(root: &Path, prefix: &str) -> (PathBuf, PackageAssetId) {
    let mut matches = std::fs::read_dir(root)
        .expect("track directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".mxf"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    assert_eq!(matches.len(), 1);
    let path = matches.remove(0);
    let name = path.file_stem().and_then(|name| name.to_str()).expect("track stem");
    let id = uuid::Uuid::parse_str(name.trim_start_matches(prefix)).expect("BMX fp_uuid");
    (path, PackageAssetId::from_uuid(id))
}

fn photon_descriptor(
    toolchain: &ProfessionalDeliveryToolchain,
    track: &Path,
    work: &Path,
) -> String {
    std::fs::create_dir(work).expect("Photon work directory");
    let output = run_qualification_command(
        &mut toolchain.photon_track_descriptor_command(track, work),
        "Photon Track File validation",
    );
    assert!(
        output.contains("No errors were detected in the IMFTrackFile"),
        "{output}"
    );
    std::fs::read_to_string(work.join("EssenceDescriptor.xml")).expect("Photon descriptor")
}
