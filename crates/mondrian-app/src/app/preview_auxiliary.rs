//! Asset-backed auxiliary requests share the ordinary Preview media authority.
//!
//! Planning performs no decode or pixel work. Even a transparent boundary
//! sample validates the physical source and admitted probe through the same
//! resolver as ordinary media; stale duration facts cannot hide a lost source.

use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::{
    Resolution, SourceSampleTarget, TimelineTime, TimelineTimeRange, VisualSourceReference,
    VisualSourceTimeSample,
};
use mondrian_media::{PreviewRepresentationQuality, ProxyConfig};
use mondrian_timeline::sequence::MediaInputColorContext;
use std::{collections::HashSet, sync::Arc};

use super::preview_access_mode::MediaPreviewKey;
use super::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
use super::preview_media_frame::MediaPreviewFrame;
use super::preview_media_source::{
    resolve_preview_media_source, PreviewMediaSourceOutcome, PreviewMediaSourceRequest,
    PreviewMediaSourceUnavailableReason,
};
use super::preview_timeline_execution::PreviewTimelineMediaRequest;
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};

mod raster;
pub(crate) use raster::{
    plan_auxiliary_registration, preflight_auxiliary_composite, prepare_auxiliary_rasters,
    visual_auxiliary_working_bytes,
};

/// A temporary auxiliary preparation charge in the existing Viewer ledger.
/// The charge remains live through pixel retirement, including worker waits.
#[derive(Debug)]
pub(crate) struct PreviewAuxiliaryWorkReservation {
    bytes: usize,
    _lease: mondrian_playback::ViewerFrameWorkLease,
}

pub(crate) fn reserve_auxiliary_working_set(
    store: &mut super::preview_frame_store::PreviewFrameStoreAdapter,
    required: usize,
) -> Result<Option<PreviewAuxiliaryWorkReservation>, PreviewUnavailability> {
    if required == 0 {
        return Ok(None);
    }
    let lease = store.reserve_working_result(required).ok_or_else(|| {
        PreviewUnavailability::blocked(PreviewOutputStage::InputAdaptation,
            format!("auxiliary working pixels require {required} bytes in the shared Viewer residency grant"))
    })?;
    Ok(Some(PreviewAuxiliaryWorkReservation {
        bytes: required,
        _lease: lease,
    }))
}

pub(crate) fn validate_auxiliary_working_reservation(
    required: usize,
    reservation: Option<&PreviewAuxiliaryWorkReservation>,
) -> Result<(), super::preview_cpu_execution::PreviewCpuExecutionError> {
    if required == 0 || reservation.is_some_and(|reservation| reservation.bytes >= required) {
        Ok(())
    } else {
        Err(super::preview_cpu_execution::PreviewCpuExecutionError::AuxiliaryRegistration {
            detail: format!("auxiliary working pixels require an original {required}-byte residency reservation"),
        })
    }
}

pub(crate) fn collect_auxiliary_bindings<'a>(
    element: &'a super::preview_viewer_plan::ResolvedPreviewElement,
    bindings: &mut Vec<&'a ResolvedPreviewAuxiliaryInputs>,
) {
    if let super::preview_viewer_plan::ResolvedPreviewElement::WithAuxiliary { element, inputs } =
        element
    {
        for input in inputs {
            let same = bindings.iter().any(|existing| {
                Arc::ptr_eq(&existing.graph, &input.graph)
                    && existing.primary_extent == input.primary_extent
                    && existing.primary_sampling == input.primary_sampling
                    && match (&existing.projection_context, &input.projection_context) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
                    && existing.sources.len() == input.sources.len()
                    && existing.sources.iter().zip(&input.sources).all(|(a, b)| {
                        a.slot == b.slot
                            && a.evidence == b.evidence
                            && a.frame.as_ref().map(|frame| frame.identity())
                                == b.frame.as_ref().map(|frame| frame.identity())
                    })
            });
            if !same {
                bindings.push(input);
            }
        }
        collect_auxiliary_bindings(element, bindings);
    }
}

pub(crate) fn resolved_auxiliary_projection(
    elements: &[super::preview_viewer_plan::ResolvedPreviewElement],
) -> Option<
    &Arc<
        super::preview_timeline_execution::auxiliary_projection::PreviewAuxiliaryProjectionContext,
    >,
> {
    elements.iter().find_map(|element| match element {
        super::preview_viewer_plan::ResolvedPreviewElement::WithAuxiliary { inputs, .. } => {
            inputs.iter().find_map(|input| input.projection_context.as_ref())
        }
        _ => None,
    })
}

pub(crate) fn resolved_auxiliary_working_bytes(
    elements: &[super::preview_viewer_plan::ResolvedPreviewElement],
) -> Result<usize, super::preview_cpu_execution::PreviewCpuExecutionError> {
    let mut bindings = Vec::new();
    for element in elements {
        collect_auxiliary_bindings(element, &mut bindings);
    }
    let mut total = 0usize;
    let mut registered = HashSet::new();
    for binding in bindings {
        for source in &binding.sources {
            let registration = binding.registration(source)?;
            if registered.insert(registration.key) {
                total = total.checked_add(registration.working_bytes()?).ok_or_else(|| {
                    super::preview_cpu_execution::PreviewCpuExecutionError::AuxiliaryRegistration {
                        detail: "auxiliary working pixel size overflowed".into(),
                    }
                })?;
            }
        }
    }
    Ok(total)
}

pub(crate) fn admit_resolved_auxiliary_projection(
    elements: &[super::preview_viewer_plan::ResolvedPreviewElement],
    store: &mut super::preview_frame_store::PreviewFrameStoreAdapter,
) -> Result<(), PreviewUnavailability> {
    let mut bindings = Vec::new();
    for element in elements {
        collect_auxiliary_bindings(element, &mut bindings);
    }
    for binding in bindings {
        if let Some(context) = &binding.projection_context {
            context.admit(store)?;
        }
    }
    Ok(())
}

/// Exact Sequence-local source intent, independent from a consumer's pixel graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PreviewAuxiliaryAssetRequest {
    pub(crate) reference: VisualSourceReference,
    pub(crate) sequence_time: TimelineTime,
    /// Decode hint only; MatchPrimary registration uses the actual primary later.
    pub(crate) target_resolution: Resolution,
    pub(crate) input_color: MediaInputColorContext,
}

/// A frozen source key and explicit boundary result, without owning decoded pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewAuxiliaryAssetPlan {
    /// The ordinary media authority's complete key, including current revision.
    /// For transparency, this proves the source at ZERO without requesting decode.
    pub(crate) validated_key: MediaPreviewKey,
    pub(crate) time: VisualSourceTimeSample,
    pub(crate) media_request: Option<PreviewTimelineMediaRequest>,
}

/// Source-ready invocation binding. Original frames retain the Store leases;
/// registration happens only after the actual primary grid is known.
pub(crate) struct ResolvedPreviewAuxiliaryInputs {
    pub(crate) graph: Arc<mondrian_effects::CompiledEffectGraph>,
    pub(crate) side: mondrian_renderer::TimelineAuxiliarySide,
    pub(crate) primary_extent: mondrian_effects::EffectFrameExtent,
    pub(crate) primary_sampling: mondrian_core::ImageSampling,
    pub(crate) sources: Box<[ResolvedPreviewAuxiliarySource]>,
    pub(crate) projection_context: Option<Arc<super::preview_timeline_execution::auxiliary_projection::PreviewAuxiliaryProjectionContext>>,
}

pub(crate) struct ResolvedPreviewAuxiliarySource {
    pub(crate) slot: u32,
    pub(crate) evidence: PreviewAuxiliaryEvidence,
    /// None is exclusively the explicit Transparent boundary result.
    pub(crate) frame: Option<MediaPreviewFrame>,
}

#[derive(PartialEq, Eq)]
pub(crate) enum PreviewAuxiliaryEvidence {
    Asset(Box<ResolvedAuxiliaryAssetEvidence>),
    Visual(ResolvedAuxiliaryVisualEvidence),
}

#[derive(PartialEq, Eq)]
pub(crate) struct ResolvedAuxiliaryAssetEvidence {
    pub(crate) request: PreviewAuxiliaryAssetRequest,
    pub(crate) plan: PreviewAuxiliaryAssetPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ResolvedAuxiliaryVisualEvidence {
    pub(crate) reference: VisualSourceReference,
    pub(crate) sequence_id: mondrian_core::SequenceId,
    pub(crate) revision: mondrian_core::SequenceRevision,
    pub(crate) time: VisualSourceTimeSample,
    pub(crate) domain: (
        mondrian_effects::EffectFrameExtent,
        mondrian_core::ImageSampling,
    ),
    pub(crate) working_color_space: mondrian_core::WorkingColorSpace,
}

impl ResolvedPreviewAuxiliaryInputs {
    pub(crate) fn hash_identity(&self, hasher: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.graph.semantic_fingerprint().hash(hasher);
        self.primary_extent.hash(hasher);
        self.primary_sampling.hash(hasher);
        self.projection_context.is_some().hash(hasher);
        if let Some(context) = &self.projection_context {
            context.hash_identity(hasher);
        }
        self.sources.len().hash(hasher);
        for source in &self.sources {
            source.slot.hash(hasher);
            match &source.evidence {
                PreviewAuxiliaryEvidence::Asset(asset) => {
                    asset.request.hash(hasher);
                    asset.plan.validated_key.hash(hasher);
                    asset.plan.time.hash(hasher);
                    asset.plan.media_request.hash(hasher);
                }
                PreviewAuxiliaryEvidence::Visual(visual) => {
                    b"auxiliary-visual".hash(hasher);
                    visual.hash(hasher);
                }
            }
            source.frame.as_ref().map(MediaPreviewFrame::identity).hash(hasher);
            source.frame.as_ref().and_then(MediaPreviewFrame::cpu_image_domain).hash(hasher);
        }
    }

    pub(crate) fn permits_cross_call_reuse(&self) -> bool {
        self.sources.iter().all(|source| {
            source.frame.as_ref().is_none_or(MediaPreviewFrame::permits_cross_call_reuse)
        })
    }
}

/// Only unavailable source facts authorize consumer substitution. Invalid
/// author/format/color/protocol results remain terminal errors.
#[derive(Debug, Clone)]
pub(crate) enum PreviewAuxiliaryAssetError {
    SourceUnavailable(PreviewUnavailability),
    Invalid(PreviewUnavailability),
}

impl PreviewAuxiliaryAssetError {
    pub(crate) fn permits_missing_source_fallback(&self) -> bool {
        matches!(self, Self::SourceUnavailable(reason)
            if reason.disposition() != super::preview_unavailability::PreviewUnavailabilityDisposition::Stopped)
    }
}

impl From<PreviewAuxiliaryAssetError> for PreviewUnavailability {
    fn from(error: PreviewAuxiliaryAssetError) -> Self {
        match error {
            PreviewAuxiliaryAssetError::SourceUnavailable(reason)
            | PreviewAuxiliaryAssetError::Invalid(reason) => reason,
        }
    }
}

impl From<PreviewUnavailability> for PreviewAuxiliaryAssetError {
    fn from(reason: PreviewUnavailability) -> Self {
        Self::Invalid(reason)
    }
}

pub(crate) type PreviewAuxiliaryAssetResolver<'a> = dyn FnMut(
        &PreviewAuxiliaryAssetRequest,
    ) -> Result<PreviewAuxiliaryAssetPlan, PreviewAuxiliaryAssetError>
    + 'a;

/// Resolve one strongly referenced record, including retired Library members.
pub(crate) fn resolve_preview_auxiliary_asset(
    request: &PreviewAuxiliaryAssetRequest,
    asset: &AssetRecord,
    hardware_admission: PreviewHardwareDecodeAdmissionState,
) -> Result<PreviewAuxiliaryAssetPlan, PreviewAuxiliaryAssetError> {
    let refused = |reason: String| {
        PreviewAuxiliaryAssetError::Invalid(PreviewUnavailability::blocked(
            PreviewOutputStage::MediaResolution,
            reason,
        ))
    };
    let VisualSourceReference::Asset { asset_id, retime } = request.reference else {
        return Err(refused("auxiliary picture source is unbound".into()));
    };
    if asset.id != asset_id {
        return Err(refused(
            "auxiliary Asset lookup returned a different identity".into(),
        ));
    }
    if !matches!(asset.kind, AssetKind::Video | AssetKind::StillImage)
        || asset.file_path().is_none()
        || asset.interpretation.payload.is_non_color_data()
    {
        return Err(refused(format!(
            "auxiliary Asset {asset_id} requires a file-backed color-managed picture"
        )));
    }
    let video = asset.media_probe().and_then(|probe| probe.primary_video()).ok_or_else(|| {
        refused(format!(
            "auxiliary Asset {asset_id} has no admitted picture probe"
        ))
    })?;
    let time = match asset.kind {
        AssetKind::StillImage => VisualSourceTimeSample {
            requested_time: request
                .sequence_time
                .checked_add(retime.time_offset)
                .map_err(|error| refused(error.to_string()))?,
            source_sample: Some(SourceSampleTarget::covering(TimelineTime::ZERO)),
            out_of_range: false,
        },
        AssetKind::Video => {
            let duration = video.duration.ok_or_else(|| {
                refused(format!(
                    "auxiliary Asset {asset_id} has no proven video duration"
                ))
            })?;
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| refused("auxiliary video duration overflows exact time".into()))?;
            let exact_duration = TimelineTime::new(seconds, 1)
                .and_then(|whole| {
                    whole.checked_add(TimelineTime::new(
                        i64::from(duration.subsec_nanos()),
                        1_000_000_000,
                    )?)
                })
                .map_err(|error| refused(error.to_string()))?;
            retime
                .sample(
                    request.sequence_time,
                    TimelineTimeRange::new(TimelineTime::ZERO, exact_duration)
                        .map_err(|error| refused(error.to_string()))?,
                )
                .map_err(|error| refused(error.to_string()))?
        }
        AssetKind::Audio | AssetKind::AdjustmentLayer | AssetKind::SolidColor => {
            return Err(refused("auxiliary source is not picture media".into()));
        }
    };
    let media = PreviewTimelineMediaRequest {
        image_scale_requirement: mondrian_effects::EffectImageScaleRequirement::Unit,
        asset_id,
        color_space_override: None,
        alpha_interpretation: mondrian_core::timeline_data::AlphaInterpretation::Straight,
        picture_overrides: mondrian_core::PictureInterpretationOverrides::default(),
        source_sample: time
            .source_sample
            .unwrap_or_else(|| SourceSampleTarget::covering(TimelineTime::ZERO)),
        target_resolution: request.target_resolution,
        input_color: request.input_color.clone(),
        cpu_working_required: true,
    };
    let proxy_config = ProxyConfig::default();
    let resolved = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset,
        color_space_override: media.color_space_override,
        alpha_interpretation: media.alpha_interpretation,
        picture_overrides: media.picture_overrides,
        source_sample: media.source_sample,
        input_color: &media.input_color,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &proxy_config,
        proxy_color: None,
        hardware_admission,
        cpu_working_required: true,
        representation_quality: PreviewRepresentationQuality::Full,
    });
    let validated_key = match resolved {
        PreviewMediaSourceOutcome::Ready(resolved) => resolved.key,
        PreviewMediaSourceOutcome::ColorRejected(_) => {
            return Err(refused(format!(
                "auxiliary Asset {asset_id} color contract was rejected"
            )));
        }
        PreviewMediaSourceOutcome::Unavailable(error) => {
            let missing = matches!(
                error.reason,
                PreviewMediaSourceUnavailableReason::SourceMetadataUnavailable { .. }
            );
            let reason = PreviewUnavailability::blocked(
                PreviewOutputStage::MediaResolution,
                format!("auxiliary Asset {asset_id}: {}", error.reason),
            );
            if missing {
                return Err(PreviewAuxiliaryAssetError::SourceUnavailable(reason));
            }
            return Err(PreviewAuxiliaryAssetError::Invalid(reason));
        }
    };
    Ok(PreviewAuxiliaryAssetPlan {
        validated_key,
        time,
        media_request: time.source_sample.map(|_| media),
    })
}

#[cfg(test)]
#[path = "preview_auxiliary/tests.rs"]
pub(crate) mod tests;
