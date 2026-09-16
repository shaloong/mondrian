//! Shared final-file broadcast and regulatory PSE publication gate.
use super::*;

pub(super) fn validate_admission(config: &ExportConfig) -> Result<(), String> {
    if config.regulatory_pse.is_some()
        && config
            .broadcast_qc
            .as_ref()
            .is_none_or(|profile| !profile.require_regulatory_flash_analysis)
    {
        return Err(
            "an external PSE provider requires a matching regulatory Broadcast QC profile"
                .to_owned(),
        );
    }
    let Some(profile) = &config.broadcast_qc else {
        return Ok(());
    };
    let delivery = crate::delivery::resolve_export_delivery(
        &config.preset,
        &config.timeline.sequence.settings,
        &config.timeline.color_environment,
    )
    .map_err(|error| error.to_string())?;
    match delivery.artifact {
        ResolvedExportArtifactEncoding::MediaFile { .. }
        | ResolvedExportArtifactEncoding::ProfessionalDelivery {profile:ProfessionalDeliveryProfile::As11X9NabaHd720p5994,..} => {}
        _ => return Err("Broadcast QC requires an implemented final-file scan path (media file or AS-11 MXF); this artifact family has no final broadcast rescan".to_owned()),
    }
    profile.validate().map_err(|error| error.to_string())?;
    if profile.observation_tap
        != mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer
        || profile.active_picture.raster_width != delivery.resolution.width
        || profile.active_picture.raster_height != delivery.resolution.height
        || profile.signal_color_space != delivery.color_target.color_space
    {
        return Err("Broadcast QC profile does not match the exact export delivery raster, color or observation tap".to_owned());
    }
    Ok(())
}

pub(super) fn verify(
    job: &RenderJob,
    owner: &OwnedPublicationFile,
    expectations: &ExportValidationExpectations,
    expected_frames: u64,
    cancel: &ExecutionCancellationToken,
    diagnostics: &mut ExportJobDiagnostics,
    report: &mut dyn FnMut(ExportJobDiagnostics),
) -> Result<(), JobExecutionResult> {
    let Some(profile) = &job.config.broadcast_qc else {
        return Ok(());
    };
    let deadline = Instant::now().checked_add(Duration::from_secs(60 * 60)).ok_or_else(|| {
        JobExecutionResult::Failed("final broadcast QC deadline overflow".to_owned())
    })?;
    let receipt = match crate::broadcast_artifact_qc::verify_finished_broadcast_artifact_until(
        owner.path(),
        expectations,
        profile.clone(),
        expected_frames,
        1_u64 << 40,
        deadline,
        cancel,
    ) {
        Ok(receipt) => receipt,
        Err(error) => {
            diagnostics.broadcast_artifact_qc_failure = Some(error.diagnostics());
            report(diagnostics.clone());
            if cancel.is_canceled() {
                return Err(JobExecutionResult::Cancelled);
            }
            return Err(JobExecutionResult::Failed(format!(
                "final encoded broadcast QC failed: {error:#}"
            )));
        }
    };
    diagnostics.broadcast_artifact_qc = Some(receipt.clone());
    report(diagnostics.clone());
    let mut final_qc = receipt.artifact.scan.clone();
    if profile.require_regulatory_flash_analysis {
        let Some(provider) = job.regulatory_pse.lock().take() else {
            return Err(JobExecutionResult::Failed(
                "required regulatory PSE has no retained admitted provider owner".to_owned(),
            ));
        };
        match provider.verify_owned_final_artifact(
            owner,
            &receipt.artifact,
            1_u64 << 40,
            deadline,
            cancel,
        ) {
            Ok(regulatory) => {
                let resolved = regulatory.resolved_qc(&receipt.artifact);
                diagnostics.regulatory_pse = Some(regulatory.into_evidence());
                report(diagnostics.clone());
                final_qc = resolved.ok_or_else(|| {
                    JobExecutionResult::Failed(
                        "regulatory PSE did not bind the exact final artifact scan".to_owned(),
                    )
                })?;
            }
            Err(failure) => {
                let detail = format!("final artifact regulatory PSE failed: {failure:#}");
                diagnostics.regulatory_pse = Some(*failure.evidence);
                report(diagnostics.clone());
                if cancel.is_canceled() {
                    return Err(JobExecutionResult::Cancelled);
                }
                return Err(JobExecutionResult::Failed(detail));
            }
        }
    }
    let verdict = final_qc.verdict;
    diagnostics.broadcast_final_qc = Some(final_qc);
    report(diagnostics.clone());
    if matches!(
        verdict,
        mondrian_broadcast::BroadcastQcVerdict::Fail
            | mondrian_broadcast::BroadcastQcVerdict::Incomplete
    ) {
        return Err(JobExecutionResult::Failed(format!(
            "final encoded broadcast QC rejected publication: {verdict:?}"
        )));
    }
    Ok(())
}

pub(super) fn as11_expectations(
    contract: &crate::professional_delivery::ResolvedProfessionalDeliveryContract,
    settings: &mondrian_timeline::sequence::SequenceSettings,
    delivery: &ResolvedExportDeliveryContract,
    frames: u64,
) -> Result<ExportValidationExpectations, String> {
    if contract.profile != ProfessionalDeliveryProfile::As11X9NabaHd720p5994 || frames == 0 {
        return Err("final AS-11 scan requires its exact nonempty delivery contract".to_owned());
    }
    let mut signal = expected_export_video_signal(settings, delivery)?;
    // Decode proves progressive scan even when the MXF stream omits field_order.
    signal.field_order = None;
    Ok(ExportValidationExpectations {
        container: Container::Mxf,
        video: ExpectedStream::Required(ExpectedVideoConstraints {
            encoding: Some(crate::validator::ExpectedVideoEncoding::H264High422Intra),
            bit_depth: Some(delivery_bit_depth_value(contract.bit_depth)),
            width: Some(contract.resolution.width),
            height: Some(contract.resolution.height),
            fps_num: Some(contract.edit_rate.num),
            fps_den: Some(contract.edit_rate.den),
            signal: Some(signal),
            coding: Some(crate::video_encoding::ResolvedVideoCodingStructure::IntraOnly),
            require_progressive_frame: true,
            ..Default::default()
        }),
        audio: ExpectedStream::Required(crate::validator::ExpectedAudioConstraints {
            encoding: crate::validator::ExpectedAudioEncoding::PcmS24Le,
            sample_rate: contract.audio_sample_rate,
            channel_layout: contract.audio_layout,
        }),
        expected_duration_secs: Some(
            frames as f64 * contract.edit_rate.den as f64 / contract.edit_rate.num as f64,
        ),
    })
}
