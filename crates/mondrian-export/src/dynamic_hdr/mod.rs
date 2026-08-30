//! Dynamic HDR delivery contract and execution Adapter.
//!
//! The first qualified path preserves one complete source file byte-for-byte.
//! Regeneration is intentionally gated behind a future qualified toolchain;
//! open-source syntax tools alone do not establish HDR10+ branding or Dolby
//! Vision licensing/validation authority.

use crate::artifact_identity::sha256_file;
use crate::delivery::{ResolvedExportArtifactEncoding, ResolvedExportDeliveryContract};
use crate::preset::{AudioCodecConfig, ExportConfig, TimelineExportSnapshot};
use crate::queue::{ExportDynamicHdrKind, ExportDynamicHdrPreservationEvidence};
use crate::smart_render::{
    qualify_exact_source_file_preservation, SmartRenderBlocker, SmartRenderPlan,
};
use mondrian_core::{
    DynamicHdrMetadataFamily, ExecutionCancellationToken, TimelineTimeRange, VideoHdrSideDataKind,
};
use mondrian_timeline::{DynamicHdrDeliveryIntent, PreparedDynamicHdrProgram};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;

/// Immutable Dynamic HDR path resolved at queue admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedDynamicHdrDelivery {
    Omit,
    PreserveSourceExact {
        family: DynamicHdrMetadataFamily,
    },
    Remake {
        program: Box<PreparedDynamicHdrProgram>,
    },
}

/// Resolve author intent against the frozen Program picture and delivery row.
pub(crate) fn resolve_dynamic_hdr_delivery(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    selected_range: TimelineTimeRange,
) -> Result<ResolvedDynamicHdrDelivery, String> {
    match timeline.sequence.dynamic_hdr.delivery_intent() {
        DynamicHdrDeliveryIntent::Omit => Ok(ResolvedDynamicHdrDelivery::Omit),
        DynamicHdrDeliveryIntent::PreserveSourceExact { family } => {
            validate_preservation_delivery_row(timeline, delivery)?;
            Ok(ResolvedDynamicHdrDelivery::PreserveSourceExact { family: *family })
        }
        DynamicHdrDeliveryIntent::Remake { program_id } => {
            validate_remake_delivery_row(timeline, delivery)?;
            let visual = timeline
                .prepared_execution()
                .and_then(|execution| execution.visual().program_by_id(timeline.sequence.id))
                .ok_or_else(|| {
                    "Dynamic HDR Remake requires the frozen final Prepared Visual Program"
                        .to_owned()
                })?;
            let program = timeline
                .sequence
                .dynamic_hdr
                .project_delivery_range(
                    *program_id,
                    selected_range,
                    visual.visual_author_fingerprint(),
                )
                .map_err(|error| error.to_string())?;
            Ok(ResolvedDynamicHdrDelivery::Remake { program: Box::new(program) })
        }
    }
}

/// Execute the selected Dynamic HDR path before ordinary Smart Render/render.
///
/// `Ok(None)` means ordinary rendered output is explicitly selected. Exact
/// preservation failure is always an error and never falls back to rendering.
pub(crate) fn execute_dynamic_hdr_delivery(
    resolved: &ResolvedDynamicHdrDelivery,
    config: &ExportConfig,
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    selected_range: TimelineTimeRange,
    total_frames: u64,
    output_path: &Path,
    cancel: &ExecutionCancellationToken,
) -> Result<Option<ExportDynamicHdrPreservationEvidence>, String> {
    match resolved {
        ResolvedDynamicHdrDelivery::Omit => Ok(None),
        ResolvedDynamicHdrDelivery::Remake { program } => Err(format!(
            "Dynamic HDR Remake for {} is blocked: no adopter-qualified/licensed analysis, generation, independent validation, and human-QC Adapter is installed",
            program.standard.diagnostic_label()
        )),
        ResolvedDynamicHdrDelivery::PreserveSourceExact { family } => {
            let plan = qualify_exact_source_file_preservation(
                config,
                timeline,
                delivery,
                selected_range,
                total_frames,
            )
            .map_err(preservation_blocker)?;
            validate_source_family(timeline, &plan, *family)?;
            if mondrian_media::MediaFileFingerprint::capture(&plan.path)
                != plan.source_fingerprint
            {
                return Err(
                    "Dynamic HDR preservation source changed after queue capture".to_owned(),
                );
            }
            let (source_bytes, source_sha256) =
                copy_file_cancellable(&plan.path, output_path, cancel)?;
            if Some(source_bytes) != plan.source_fingerprint.len {
                return Err("Dynamic HDR preservation source length changed during copy".to_owned());
            }
            if mondrian_media::MediaFileFingerprint::capture(&plan.path)
                != plan.source_fingerprint
            {
                return Err("Dynamic HDR preservation source changed during copy".to_owned());
            }
            let output_sha256 = decode_sha256(&sha256_file(
                output_path,
                cancel,
                "Dynamic HDR preserved output",
            )?)?;
            if source_sha256 != output_sha256 {
                return Err(
                    "Dynamic HDR exact preservation failed byte-identity verification".to_owned(),
                );
            }
            let output_probe = mondrian_media::probe_media_info(output_path)
                .map_err(|error| format!("re-probe preserved Dynamic HDR output: {error}"))?;
            let output_stream = output_probe
                .video_streams
                .iter()
                .find(|stream| stream.index == plan.video_stream_index)
                .ok_or_else(|| {
                    "preserved Dynamic HDR output lost the selected video stream".to_owned()
                })?;
            if !stream_has_family(output_stream, *family) {
                return Err(format!(
                    "preserved output re-probe did not find {}",
                    family.diagnostic_label()
                ));
            }
            Ok(Some(ExportDynamicHdrPreservationEvidence {
                source_asset_id: plan.asset_id,
                kind: export_kind(*family),
                source_bytes,
                source_sha256,
                output_sha256,
                byte_identity_verified: true,
                output_metadata_reprobed: true,
            }))
        }
    }
}

fn validate_preservation_delivery_row(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
) -> Result<(), String> {
    let ResolvedExportArtifactEncoding::MediaFile { audio, .. } = &delivery.artifact else {
        return Err(
            "Dynamic HDR exact preservation requires one ordinary media-file artifact".to_owned(),
        );
    };
    if !matches!(audio, AudioCodecConfig::Disabled) {
        return Err(
            "Dynamic HDR exact preservation currently requires a video-only source and disabled Program audio"
                .to_owned(),
        );
    }
    if timeline
        .sequence
        .settings
        .delivery
        .static_hdr_metadata_policy
        .writes_authored_metadata()
    {
        return Err(
            "Dynamic HDR exact preservation cannot rewrite authored static HDR metadata".to_owned(),
        );
    }
    if delivery.legalizer.is_active() {
        return Err("Dynamic HDR exact preservation cannot apply a Legalizer".to_owned());
    }
    Ok(())
}

fn validate_remake_delivery_row(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
) -> Result<(), String> {
    use crate::preset::{ExportChromaSampling, HevcProfile, VideoCodecConfig};
    use mondrian_core::{timeline_data::FieldOrder, ColorSpace};
    use mondrian_timeline::sequence::{DeliveryBitDepth, VideoRange};

    let ResolvedExportArtifactEncoding::MediaFile { video, .. } = &delivery.artifact else {
        return Err("Dynamic HDR Remake is not qualified for this artifact family".to_owned());
    };
    if !matches!(
        video,
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. }
    ) || delivery.bit_depth != DeliveryBitDepth::Ten
        || delivery.chroma_sampling != ExportChromaSampling::Yuv420
        || delivery.field_order != FieldOrder::Progressive
        || delivery.video_range != VideoRange::Legal
        || delivery.color_target.color_space != ColorSpace::Rec2100Pq
        || !timeline
            .sequence
            .settings
            .delivery
            .static_hdr_metadata_policy
            .writes_authored_metadata()
    {
        return Err(
            "Dynamic HDR Remake first-stage matrix requires progressive Rec.2100 PQ Legal HEVC Main10 10-bit 4:2:0 with authored ST 2086/MaxCLL/MaxFALL"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_source_family(
    timeline: &TimelineExportSnapshot,
    plan: &SmartRenderPlan,
    family: DynamicHdrMetadataFamily,
) -> Result<(), String> {
    let dependency = timeline.media.get(&plan.asset_id).ok_or_else(|| {
        "Dynamic HDR preservation source dependency disappeared from the snapshot".to_owned()
    })?;
    let stream = dependency.source_video_stream.as_ref().ok_or_else(|| {
        "Dynamic HDR preservation source has no frozen video-stream probe".to_owned()
    })?;
    if stream.index != plan.video_stream_index || !stream_has_family(stream, family) {
        return Err(format!(
            "source probe does not prove {} on the exact selected stream",
            family.diagnostic_label()
        ));
    }
    Ok(())
}

fn stream_has_family(
    stream: &mondrian_core::VideoStreamInfo,
    family: DynamicHdrMetadataFamily,
) -> bool {
    let expected = match family {
        DynamicHdrMetadataFamily::St2094_40Application4 => VideoHdrSideDataKind::DynamicHdr10Plus,
        DynamicHdrMetadataFamily::DolbyVision => VideoHdrSideDataKind::DolbyVisionConfig,
    };
    stream.hdr_metadata.iter().any(|metadata| metadata.kind == expected)
}

fn copy_file_cancellable(
    source: &Path,
    output: &Path,
    cancel: &ExecutionCancellationToken,
) -> Result<(u64, [u8; 32]), String> {
    let source = File::open(source)
        .map_err(|error| format!("open Dynamic HDR preservation source: {error}"))?;
    let output = File::create(output)
        .map_err(|error| format!("create Dynamic HDR preservation staging file: {error}"))?;
    let mut reader = BufReader::with_capacity(COPY_BUFFER_BYTES, source);
    let mut writer = BufWriter::with_capacity(COPY_BUFFER_BYTES, output);
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut digest = Sha256::new();
    let mut source_bytes = 0_u64;
    loop {
        if cancel.is_canceled() {
            return Err("Dynamic HDR exact preservation cancelled".to_owned());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read Dynamic HDR source: {error}"))?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|error| format!("write Dynamic HDR staging file: {error}"))?;
        digest.update(&buffer[..read]);
        source_bytes = source_bytes
            .checked_add(read as u64)
            .ok_or_else(|| "Dynamic HDR source byte count overflowed".to_owned())?;
    }
    writer
        .flush()
        .map_err(|error| format!("flush Dynamic HDR staging file: {error}"))?;
    Ok((source_bytes, digest.finalize().into()))
}

fn export_kind(family: DynamicHdrMetadataFamily) -> ExportDynamicHdrKind {
    match family {
        DynamicHdrMetadataFamily::St2094_40Application4 => {
            ExportDynamicHdrKind::St2094_40Application4
        }
        DynamicHdrMetadataFamily::DolbyVision => ExportDynamicHdrKind::DolbyVision,
    }
}

fn preservation_blocker(blocker: SmartRenderBlocker) -> String {
    format!(
        "Dynamic HDR exact preservation is not eligible ({blocker:?}); rendered fallback is forbidden"
    )
}

fn decode_sha256(encoded: &str) -> Result<[u8; 32], String> {
    if encoded.len() != 64 {
        return Err("SHA-256 evidence has an invalid encoded length".to_owned());
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&encoded[offset..offset + 2], 16)
            .map_err(|_| "SHA-256 evidence is not hexadecimal".to_owned())?;
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_COPY_FIXTURE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn sha256_evidence_decoder_is_exact() {
        let encoded = "07".repeat(32);
        assert_eq!(decode_sha256(&encoded).expect("digest"), [7; 32]);
        assert!(decode_sha256("07").is_err());
    }

    #[test]
    fn exact_preservation_copy_is_byte_identical_and_cancellable() {
        let root = std::env::temp_dir().join(format!(
            "dynamic-hdr-copy-{}-{}",
            std::process::id(),
            NEXT_COPY_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create copy fixture");
        let source = root.join("source.bin");
        let output = root.join("output.bin");
        std::fs::write(
            &source,
            (0_u8..=255).cycle().take(1_048_777).collect::<Vec<_>>(),
        )
        .expect("write source");
        let active = ExecutionCancellationToken::new();
        let (source_bytes, source_sha256) =
            copy_file_cancellable(&source, &output, &active).expect("copy exact source");
        assert_eq!(source_bytes, 1_048_777);
        assert_eq!(
            source_sha256,
            decode_sha256(&sha256_file(&source, &active, "fixture").expect("hash source"))
                .expect("decode source hash")
        );
        assert_eq!(
            std::fs::read(&source).expect("read source"),
            std::fs::read(&output).expect("read output")
        );

        let cancelled = ExecutionCancellationToken::new();
        cancelled.cancel();
        assert!(
            copy_file_cancellable(&source, &root.join("cancelled.bin"), &cancelled)
                .expect_err("cancel must fail closed")
                .contains("cancelled")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
