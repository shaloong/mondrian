//! Checked active-texture admission for one Viewer GPU frame.
//!
//! Idle texture retention and active command-recording residency are different
//! resource dimensions. The shared color-frame pool governs only textures that
//! are no longer referenced by a frame. This module instead lowers the complete
//! immutable Viewer request into a conservative active-texture estimate before
//! any source upload, native import, effect, composite, spatial, output, scope,
//! monitor, or calibration texture can be created.

use crate::{
    ColorFrameEncoding, GpuColorFrameTextureFormat, ViewerGpuExecutionLayer,
    ViewerGpuExecutionRequest, ViewerGpuSourceLayer, ViewerGpuTransitionInput, ViewerSourceRect,
    GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO,
};
use mondrian_effects::EffectColorDomain;
use mondrian_media::DecodedVideoSurfaceFormat;

/// Texture demand attributed to one Viewer execution stage family.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewerGpuActiveTextureDemand {
    /// Conservative number of simultaneously live texture resources.
    pub textures: u64,
    /// Conservative logical texture bytes, excluding driver allocation padding.
    pub bytes: u64,
}

impl ViewerGpuActiveTextureDemand {
    fn checked_add(
        &mut self,
        other: Self,
        stage: ViewerGpuActiveWorkingSetStage,
    ) -> Result<(), ViewerGpuActiveWorkingSetEstimateError> {
        self.textures = self
            .textures
            .checked_add(other.textures)
            .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        self.bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        Ok(())
    }

    fn checked_texture(
        width: u32,
        height: u32,
        bytes_per_pixel: u64,
        stage: ViewerGpuActiveWorkingSetStage,
    ) -> Result<Self, ViewerGpuActiveWorkingSetEstimateError> {
        let bytes = checked_texture_bytes(width, height, bytes_per_pixel, stage)?;
        Ok(Self { textures: 1, bytes })
    }

    fn checked_repeated_texture(
        width: u32,
        height: u32,
        bytes_per_pixel: u64,
        textures: u64,
        stage: ViewerGpuActiveWorkingSetStage,
    ) -> Result<Self, ViewerGpuActiveWorkingSetEstimateError> {
        let one = checked_texture_bytes(width, height, bytes_per_pixel, stage)?;
        let bytes = one
            .checked_mul(textures)
            .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        Ok(Self { textures, bytes })
    }
}

/// Stable stage attribution for checked Viewer working-set diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerGpuActiveWorkingSetStage {
    /// CPU upload, decoded native import, or heterogeneous continuation input.
    SourcePreparation,
    /// Materialized point-effect and color-domain intermediates.
    Effects,
    /// Two independently materialized Transition endpoints and Transition output.
    Transitions,
    /// Working-linear segment accumulators and adjustment blends.
    WorkingComposite,
    /// Crop, prefilter, and reconstruction intermediates.
    Spatial,
    /// Program Output color-boundary texture.
    ProgramOutput,
    /// Demand-driven histogram, waveform, and vectorscope display textures.
    ProgramScopes,
    /// Preview-only Program Output to local-monitor adaptation.
    MonitorAdaptation,
    /// Display-calibration output and 3D LUT.
    DisplayCalibration,
    /// Outputs detached into live presentation leases from earlier candidates.
    DetachedPresentations,
    /// Headroom that keeps the first admitted output replaceable by an
    /// identical subsequent candidate.
    PresentationContinuityReserve,
    /// Aggregate of all active stage demands.
    Total,
}

/// Conservative owner working set for one new Viewer GPU request.
///
/// The pure request estimator fills candidate stages. Before admission, the
/// Viewer runtime also adds the capacity-one current output detached into a
/// live presentation lease from the same resource owner plus only the
/// shortfall required to make this candidate's final output replaceable by an
/// identical next candidate. More than one detached output is rejected rather
/// than approximated as an unordered aggregate.
/// Idle textures, decoder caches, Effect CPU residency, and OCIO
/// processor/pipeline caches remain independently governed and are
/// intentionally excluded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewerGpuActiveWorkingSetEstimate {
    /// Source upload/native import/heterogeneous continuation textures.
    pub source_preparation: ViewerGpuActiveTextureDemand,
    /// Per-source and Adjustment effect-domain intermediates.
    pub effects: ViewerGpuActiveTextureDemand,
    /// Cross Dissolve endpoint and output textures.
    pub transitions: ViewerGpuActiveTextureDemand,
    /// Working-linear composite accumulators and Adjustment blend outputs.
    pub working_composite: ViewerGpuActiveTextureDemand,
    /// Viewer crop/resize intermediates.
    pub spatial: ViewerGpuActiveTextureDemand,
    /// Program Output color-boundary texture.
    pub program_output: ViewerGpuActiveTextureDemand,
    /// Demand-driven Program Output scope display textures.
    pub program_scopes: ViewerGpuActiveTextureDemand,
    /// Preview-only monitor-adaptation texture.
    pub monitor_adaptation: ViewerGpuActiveTextureDemand,
    /// Display-calibration output and 3D LUT.
    pub display_calibration: ViewerGpuActiveTextureDemand,
    /// Exact final texture that becomes the move-only presentation lease.
    ///
    /// This is a projection of exactly one already-counted Program Output,
    /// monitor-adaptation, or display-calibration texture. It is not summed
    /// directly into [`Self::total`].
    pub presentation_output: ViewerGpuActiveTextureDemand,
    /// Live outputs detached from earlier candidates for presentation.
    pub detached_presentations: ViewerGpuActiveTextureDemand,
    /// Additional owner headroom required for steady-state replacement.
    ///
    /// This is only the component-wise shortfall between
    /// `presentation_output` and the already-live detached demand, so existing
    /// physical ownership is never counted twice.
    pub presentation_continuity_reserve: ViewerGpuActiveTextureDemand,
    total: ViewerGpuActiveTextureDemand,
}

impl ViewerGpuActiveWorkingSetEstimate {
    /// Aggregate active texture demand across the complete request.
    pub const fn total(self) -> ViewerGpuActiveTextureDemand {
        self.total
    }

    pub(crate) fn include_presentation_residency(
        &mut self,
        textures: u64,
        bytes: u64,
    ) -> Result<(), ViewerGpuActiveWorkingSetEstimateError> {
        if textures > 1 {
            return Err(
                ViewerGpuActiveWorkingSetEstimateError::PresentationCapacityExceeded {
                    live_outputs: textures,
                },
            );
        }
        self.detached_presentations.checked_add(
            ViewerGpuActiveTextureDemand { textures, bytes },
            ViewerGpuActiveWorkingSetStage::DetachedPresentations,
        )?;
        self.presentation_continuity_reserve = ViewerGpuActiveTextureDemand {
            textures: self
                .presentation_output
                .textures
                .saturating_sub(self.detached_presentations.textures),
            bytes: self.presentation_output.bytes.saturating_sub(self.detached_presentations.bytes),
        };
        self.total = total_estimate(self)?;
        Ok(())
    }
}

/// Point-in-time active working-set evidence for one Viewer GPU owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerGpuActiveWorkingSetDiagnostics {
    /// Hard grant currently installed on the Viewer runtime.
    pub grant: ViewerGpuExecutionResourceGrant,
    /// Most recent complete request admitted before command recording.
    ///
    /// This remains `None` until a request passes both estimation and grant
    /// admission. A later stage-specific recording failure does not erase the
    /// resource decision that authorized that attempt.
    pub last_admitted: Option<ViewerGpuActiveWorkingSetEstimate>,
}

/// Failure to derive an active-texture estimate without touching the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ViewerGpuActiveWorkingSetEstimateError {
    /// Pixel, texture-count, or aggregate-byte arithmetic overflowed.
    #[error("Viewer GPU active working-set arithmetic overflowed in {stage:?}")]
    ArithmeticOverflow {
        /// First stage whose checked arithmetic could not be represented.
        stage: ViewerGpuActiveWorkingSetStage,
    },
    /// A request geometry invariant was invalid before GPU planning.
    #[error("Viewer GPU active working-set request is invalid: {reason}")]
    InvalidRequest {
        /// Stable invariant label.
        reason: &'static str,
    },
    /// The capacity-one presentation owner exposed more than one live output.
    ///
    /// An unordered aggregate cannot prove which lease the next publication
    /// will replace, so admission must wait until at most one current output
    /// remains.
    #[error(
        "Viewer GPU presentation owner is capacity-one, but {live_outputs} detached outputs are live"
    )]
    PresentationCapacityExceeded {
        /// Live detached output leases observed before command recording.
        live_outputs: u64,
    },
    /// A heterogeneous input address or exclusivity invariant was invalid.
    #[error("Viewer heterogeneous GPU input is invalid: {reason}")]
    InvalidHeterogeneousInput {
        /// Stable rejected invariant.
        reason: &'static str,
    },
}

/// Hard owner-scoped resource grant for Viewer GPU execution.
///
/// Idle texture retention and active frame admission remain independent. A
/// memory-pressure policy may trim the idle pool online, while
/// `max_active_texture_bytes` and `max_active_textures` must remain stable for
/// one machine/quality class so pressure cannot silently change frame
/// semantics or precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerGpuExecutionResourceGrant {
    pub(crate) output_pool: crate::GpuColorFrameWgpuResourcePoolOptions,
    max_active_texture_bytes: u64,
    max_active_textures: u64,
}

impl ViewerGpuExecutionResourceGrant {
    /// Build an idle-pool grant while retaining compatibility with isolated
    /// renderer tests and embedders that do not install a product budget.
    ///
    /// Product Preview owners must call [`Self::with_active_limits`] before
    /// execution.
    pub const fn new(max_idle_per_contract: usize, max_idle_bytes: u64) -> Self {
        Self {
            output_pool: crate::GpuColorFrameWgpuResourcePoolOptions {
                max_per_contract: max_idle_per_contract,
                max_retained_bytes: max_idle_bytes,
            },
            max_active_texture_bytes: u64::MAX,
            max_active_textures: u64::MAX,
        }
    }

    /// Install pressure-stable active-texture limits on this owner grant.
    pub const fn with_active_limits(
        mut self,
        max_active_texture_bytes: u64,
        max_active_textures: u64,
    ) -> Self {
        self.max_active_texture_bytes = max_active_texture_bytes;
        self.max_active_textures = max_active_textures;
        self
    }

    /// Maximum idle textures retained for one exact texture contract.
    pub const fn max_idle_per_contract(self) -> usize {
        self.output_pool.max_per_contract
    }

    /// Maximum approximate idle texture bytes retained across all contracts.
    pub const fn max_idle_bytes(self) -> u64 {
        self.output_pool.max_retained_bytes
    }

    /// Maximum candidate-plus-presentation logical bytes admitted for this owner.
    pub const fn max_active_texture_bytes(self) -> u64 {
        self.max_active_texture_bytes
    }

    /// Maximum candidate-plus-presentation texture resources admitted for this owner.
    pub const fn max_active_textures(self) -> u64 {
        self.max_active_textures
    }

    /// Admit one already-checked active working-set estimate.
    pub fn admit_active_working_set(
        self,
        estimate: ViewerGpuActiveWorkingSetEstimate,
    ) -> Result<(), ViewerGpuActiveWorkingSetAdmissionError> {
        let required = estimate.total();
        if required.bytes > self.max_active_texture_bytes
            || required.textures > self.max_active_textures
        {
            return Err(ViewerGpuActiveWorkingSetAdmissionError::GrantExceeded {
                required_texture_bytes: required.bytes,
                granted_texture_bytes: self.max_active_texture_bytes,
                required_textures: required.textures,
                granted_textures: self.max_active_textures,
            });
        }
        Ok(())
    }
}

impl Default for ViewerGpuExecutionResourceGrant {
    fn default() -> Self {
        let options = crate::GpuColorFrameWgpuResourcePoolOptions::default();
        Self::new(options.max_per_contract, options.max_retained_bytes)
    }
}

/// Failure to estimate or admit one Viewer GPU active working set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ViewerGpuActiveWorkingSetAdmissionError {
    /// The immutable request could not be estimated with checked arithmetic.
    #[error(transparent)]
    Estimate(#[from] ViewerGpuActiveWorkingSetEstimateError),
    /// Active texture demand exceeded either hard owner limit.
    #[error(
        "Viewer GPU active working set requires {required_texture_bytes} bytes across {required_textures} textures, but the owner grants {granted_texture_bytes} bytes across {granted_textures} textures"
    )]
    GrantExceeded {
        /// Conservative active texture-byte demand.
        required_texture_bytes: u64,
        /// Owner-scoped active texture-byte grant.
        granted_texture_bytes: u64,
        /// Conservative active texture-resource demand.
        required_textures: u64,
        /// Owner-scoped active texture-resource grant.
        granted_textures: u64,
    },
}

/// Estimate one complete Viewer request before any GPU texture is created.
///
/// The estimate intentionally sums all explicitly supplied native/GPU/CPU
/// source alternatives. A failed native or input-transform attempt may have
/// created partial resources before the correctness fallback is selected, so
/// admitting only the successful branch would not be a hard upper bound.
pub fn estimate_viewer_gpu_active_working_set(
    request: &ViewerGpuExecutionRequest<'_>,
) -> Result<ViewerGpuActiveWorkingSetEstimate, ViewerGpuActiveWorkingSetEstimateError> {
    validate_request_geometry(request)?;
    let mut estimate = ViewerGpuActiveWorkingSetEstimate::default();
    let mut seen_heterogeneous = vec![false; request.heterogeneous_inputs.len()];
    let mut external_adjustments = 0_u64;

    for layer in request.layers {
        match layer {
            ViewerGpuExecutionLayer::Source(source) => {
                estimate_source(source, request, &mut seen_heterogeneous, &mut estimate)?;
            }
            ViewerGpuExecutionLayer::Adjustment { effect_plan, opacity, .. } => {
                if opacity.clamp(0.0, 1.0) == 0.0 {
                    continue;
                }
                if effect_plan.processing_domain() != EffectColorDomain::SceneLinearRgb {
                    external_adjustments = external_adjustments.checked_add(1).ok_or(
                        ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
                            stage: ViewerGpuActiveWorkingSetStage::Effects,
                        },
                    )?;
                    estimate.effects.checked_add(
                        ViewerGpuActiveTextureDemand::checked_repeated_texture(
                            request.width,
                            request.height,
                            16,
                            3,
                            ViewerGpuActiveWorkingSetStage::Effects,
                        )?,
                        ViewerGpuActiveWorkingSetStage::Effects,
                    )?;
                }
            }
            ViewerGpuExecutionLayer::CrossDissolve(transition) => {
                let crate::ViewerGpuCrossDissolveLayer { left, right, progress } =
                    transition.as_ref();
                if !progress.is_finite() {
                    return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
                        reason: "Cross Dissolve progress is non-finite",
                    });
                }
                let progress = progress.clamp(0.0, 1.0);
                let left_contributes = estimate_transition_input(
                    left,
                    1.0 - progress,
                    request,
                    &mut seen_heterogeneous,
                    &mut estimate,
                )?;
                let right_contributes = estimate_transition_input(
                    right,
                    progress,
                    request,
                    &mut seen_heterogeneous,
                    &mut estimate,
                )?;
                let endpoint_textures = u64::from(left_contributes) + u64::from(right_contributes);
                if endpoint_textures != 0 {
                    // Each endpoint is independently materialized through a
                    // conservative two-texture accumulator. When both exist,
                    // one additional working texture carries the dissolve.
                    let textures = endpoint_textures
                        .checked_mul(2)
                        .and_then(|value| {
                            value.checked_add(u64::from(left_contributes && right_contributes))
                        })
                        .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
                            stage: ViewerGpuActiveWorkingSetStage::Transitions,
                        })?;
                    estimate.transitions.checked_add(
                        ViewerGpuActiveTextureDemand::checked_repeated_texture(
                            request.width,
                            request.height,
                            16,
                            textures,
                            ViewerGpuActiveWorkingSetStage::Transitions,
                        )?,
                        ViewerGpuActiveWorkingSetStage::Transitions,
                    )?;
                }
            }
        }
    }

    // An empty frame still records a transparent working canvas. Every
    // external-domain Adjustment closes a segment and records one blend before
    // the final segment. Two accumulators per segment plus one blend is a
    // conservative bound; effect-domain textures are attributed above.
    let composite_textures = external_adjustments
        .checked_mul(3)
        .and_then(|extra| extra.checked_add(2))
        .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
            stage: ViewerGpuActiveWorkingSetStage::WorkingComposite,
        })?;
    estimate.working_composite = ViewerGpuActiveTextureDemand::checked_repeated_texture(
        request.width,
        request.height,
        16,
        composite_textures,
        ViewerGpuActiveWorkingSetStage::WorkingComposite,
    )?;

    estimate.spatial = estimate_spatial(request)?;
    estimate.program_output = ViewerGpuActiveTextureDemand::checked_texture(
        request.output_width,
        request.output_height,
        u64::from(program_output_texture_format(request).bytes_per_pixel()),
        ViewerGpuActiveWorkingSetStage::ProgramOutput,
    )?;
    estimate.program_scopes = estimate_program_scopes(request)?;
    if request.monitor_adaptation.requires_pass() {
        estimate.monitor_adaptation = ViewerGpuActiveTextureDemand::checked_texture(
            request.output_width,
            request.output_height,
            8,
            ViewerGpuActiveWorkingSetStage::MonitorAdaptation,
        )?;
    }
    estimate.display_calibration = estimate_display_calibration(request)?;
    estimate.presentation_output = estimate_presentation_output(request)?;
    estimate.total = total_estimate(&estimate)?;
    Ok(estimate)
}

fn estimate_source(
    source: &ViewerGpuSourceLayer,
    request: &ViewerGpuExecutionRequest<'_>,
    seen_heterogeneous: &mut [bool],
    estimate: &mut ViewerGpuActiveWorkingSetEstimate,
) -> Result<(), ViewerGpuActiveWorkingSetEstimateError> {
    if source_opacity(source).clamp(0.0, 1.0) == 0.0 {
        return Ok(());
    }
    match source {
        ViewerGpuSourceLayer::Media {
            frame,
            gpu_source,
            native_source,
            heterogeneous_input,
            effect_plan,
            frame_seed,
            ..
        } => {
            let mut effect_extent = None::<(u32, u32, u64)>;
            if let Some(address) = heterogeneous_input {
                if frame.is_some() || gpu_source.is_some() || native_source.is_some() {
                    return Err(
                        ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                            reason: "heterogeneous media source is not exclusive",
                        },
                    );
                }
                if !effect_plan.is_identity() {
                    return Err(
                        ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                            reason: "heterogeneous media source retained a second Effect plan",
                        },
                    );
                }
                let index = usize::try_from(*address).map_err(|_| {
                    ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                        reason: "heterogeneous media input address is not representable",
                    }
                })?;
                let already_seen = seen_heterogeneous.get_mut(index).ok_or(
                    ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                        reason: "heterogeneous media input address is missing or duplicated",
                    },
                )?;
                if *already_seen {
                    return Err(
                        ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                            reason: "heterogeneous media input address is missing or duplicated",
                        },
                    );
                }
                *already_seen = true;
                let input = &request.heterogeneous_inputs[index];
                let binding = input.request.binding();
                if binding.working_color_space() != request.working_color_space
                    || binding.frame_seed() != *frame_seed
                {
                    return Err(
                        ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                            reason:
                                "heterogeneous media input does not match the Viewer working contract",
                        },
                    );
                }
                let plan = input.completion.execution_plan();
                if binding.frame_extent() != plan.frame_extent() {
                    return Err(
                        ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput {
                            reason:
                                "heterogeneous media input extent does not match its execution plan",
                        },
                    );
                }
                let peak_bytes = plan.peak_device_bytes();
                // The current heterogeneous Effect execution contract lowers
                // every live GPU value materialization to exactly one RGBA
                // texture. Keep the count explicit: if a future representation
                // introduces planes or auxiliary images, the Effect plan must
                // expose that physical resource demand instead of reviving a
                // byte-derived approximation here.
                estimate.source_preparation.checked_add(
                    ViewerGpuActiveTextureDemand {
                        textures: plan.peak_device_materializations(),
                        bytes: peak_bytes,
                    },
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                return Ok(());
            }

            if let Some(source) = native_source {
                let width = source.native_frame.width;
                let height = source.native_frame.height;
                let native_bytes = native_surface_texture_bytes(
                    width,
                    height,
                    source.native_frame.surface_format,
                )?;
                let encoded_rgb_bytes = checked_texture_bytes(
                    width,
                    height,
                    8,
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                let working_bytes = checked_texture_bytes(
                    width,
                    height,
                    16,
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                // The media Frame Store separately governs the already-created
                // decoder surface. Renderer native-import validation limits
                // the bridge allocation to this same visible-byte envelope,
                // followed by visible RGBA16F and RGBA32F outputs.
                let bytes = native_bytes
                    .checked_mul(GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO)
                    .and_then(|bytes| bytes.checked_add(encoded_rgb_bytes))
                    .and_then(|bytes| bytes.checked_add(working_bytes))
                    .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
                        stage: ViewerGpuActiveWorkingSetStage::SourcePreparation,
                    })?;
                estimate.source_preparation.checked_add(
                    ViewerGpuActiveTextureDemand { textures: 3, bytes },
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                observe_effect_extent(&mut effect_extent, width, height)?;
            }
            if let Some(source) = gpu_source {
                let descriptor = source.source.descriptor();
                let source_bpp = match descriptor.encoding {
                    ColorFrameEncoding::EncodedRgba8 => 4,
                    ColorFrameEncoding::LinearFloat | ColorFrameEncoding::EncodedFloat => 16,
                    ColorFrameEncoding::DeviceFloat => 16,
                };
                estimate.source_preparation.checked_add(
                    ViewerGpuActiveTextureDemand::checked_texture(
                        descriptor.width,
                        descriptor.height,
                        source_bpp,
                        ViewerGpuActiveWorkingSetStage::SourcePreparation,
                    )?,
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                estimate.source_preparation.checked_add(
                    ViewerGpuActiveTextureDemand::checked_texture(
                        descriptor.width,
                        descriptor.height,
                        16,
                        ViewerGpuActiveWorkingSetStage::SourcePreparation,
                    )?,
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                observe_effect_extent(&mut effect_extent, descriptor.width, descriptor.height)?;
            }
            if let Some(frame) = frame {
                let descriptor = frame.descriptor();
                estimate.source_preparation.checked_add(
                    ViewerGpuActiveTextureDemand::checked_texture(
                        descriptor.width,
                        descriptor.height,
                        16,
                        ViewerGpuActiveWorkingSetStage::SourcePreparation,
                    )?,
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                observe_effect_extent(&mut effect_extent, descriptor.width, descriptor.height)?;
            }
            let Some((effect_width, effect_height, _)) = effect_extent else {
                return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
                    reason: "media layer has no GPU, native, heterogeneous, or CPU source",
                });
            };
            if effect_plan.processing_domain() != EffectColorDomain::SceneLinearRgb {
                estimate.effects.checked_add(
                    ViewerGpuActiveTextureDemand::checked_repeated_texture(
                        effect_width,
                        effect_height,
                        16,
                        3,
                        ViewerGpuActiveWorkingSetStage::Effects,
                    )?,
                    ViewerGpuActiveWorkingSetStage::Effects,
                )?;
            }
        }
        ViewerGpuSourceLayer::SolidColor { effect_plan, .. } => {
            if effect_plan.processing_domain() != EffectColorDomain::SceneLinearRgb {
                // Solid materialization, working→effect, point effect, effect→working.
                estimate.source_preparation.checked_add(
                    ViewerGpuActiveTextureDemand::checked_texture(
                        request.width,
                        request.height,
                        16,
                        ViewerGpuActiveWorkingSetStage::SourcePreparation,
                    )?,
                    ViewerGpuActiveWorkingSetStage::SourcePreparation,
                )?;
                estimate.effects.checked_add(
                    ViewerGpuActiveTextureDemand::checked_repeated_texture(
                        request.width,
                        request.height,
                        16,
                        3,
                        ViewerGpuActiveWorkingSetStage::Effects,
                    )?,
                    ViewerGpuActiveWorkingSetStage::Effects,
                )?;
            }
        }
    }
    Ok(())
}

fn estimate_transition_input(
    input: &ViewerGpuTransitionInput,
    weight: f32,
    request: &ViewerGpuExecutionRequest<'_>,
    seen_heterogeneous: &mut [bool],
    estimate: &mut ViewerGpuActiveWorkingSetEstimate,
) -> Result<bool, ViewerGpuActiveWorkingSetEstimateError> {
    if weight <= 0.0 {
        return Ok(false);
    }
    match input {
        ViewerGpuTransitionInput::Transparent => Ok(false),
        ViewerGpuTransitionInput::Source(source)
            if source_opacity(source).clamp(0.0, 1.0) == 0.0 =>
        {
            Ok(false)
        }
        ViewerGpuTransitionInput::Source(source) => {
            estimate_source(source, request, seen_heterogeneous, estimate)?;
            Ok(true)
        }
    }
}

fn source_opacity(source: &ViewerGpuSourceLayer) -> f32 {
    match source {
        ViewerGpuSourceLayer::Media { opacity, .. } => *opacity,
        ViewerGpuSourceLayer::SolidColor { layer, .. } => layer.opacity,
    }
}

fn observe_effect_extent(
    current: &mut Option<(u32, u32, u64)>,
    width: u32,
    height: u32,
) -> Result<(), ViewerGpuActiveWorkingSetEstimateError> {
    let bytes = checked_texture_bytes(width, height, 16, ViewerGpuActiveWorkingSetStage::Effects)?;
    if current.is_none_or(|(_, _, current_bytes)| bytes > current_bytes) {
        *current = Some((width, height, bytes));
    }
    Ok(())
}

fn estimate_spatial(
    request: &ViewerGpuExecutionRequest<'_>,
) -> Result<ViewerGpuActiveTextureDemand, ViewerGpuActiveWorkingSetEstimateError> {
    if request.source_rect == ViewerSourceRect::FULL
        && request.width == request.output_width
        && request.height == request.output_height
    {
        return Ok(ViewerGpuActiveTextureDemand::default());
    }
    let mut selected_width = request.width;
    let mut selected_height = request.height;
    let mut demand = ViewerGpuActiveTextureDemand::default();
    while should_prefilter(
        selected_width,
        selected_height,
        request.source_rect,
        request.output_width,
        request.output_height,
    ) {
        selected_width = selected_width.div_ceil(2);
        selected_height = selected_height.div_ceil(2);
        demand.checked_add(
            ViewerGpuActiveTextureDemand::checked_texture(
                selected_width,
                selected_height,
                16,
                ViewerGpuActiveWorkingSetStage::Spatial,
            )?,
            ViewerGpuActiveWorkingSetStage::Spatial,
        )?;
    }
    demand.checked_add(
        ViewerGpuActiveTextureDemand::checked_texture(
            request.output_width,
            selected_height,
            16,
            ViewerGpuActiveWorkingSetStage::Spatial,
        )?,
        ViewerGpuActiveWorkingSetStage::Spatial,
    )?;
    demand.checked_add(
        ViewerGpuActiveTextureDemand::checked_texture(
            request.output_width,
            request.output_height,
            16,
            ViewerGpuActiveWorkingSetStage::Spatial,
        )?,
        ViewerGpuActiveWorkingSetStage::Spatial,
    )?;
    Ok(demand)
}

fn should_prefilter(
    input_width: u32,
    input_height: u32,
    source_rect: ViewerSourceRect,
    output_width: u32,
    output_height: u32,
) -> bool {
    let source_width = f64::from(input_width) * f64::from(source_rect.width);
    let source_height = f64::from(input_height) * f64::from(source_rect.height);
    (source_width > f64::from(output_width) * 4.0 || source_height > f64::from(output_height) * 4.0)
        && input_width > 1
        && input_height > 1
}

fn estimate_program_scopes(
    request: &ViewerGpuExecutionRequest<'_>,
) -> Result<ViewerGpuActiveTextureDemand, ViewerGpuActiveWorkingSetEstimateError> {
    let Some(scopes) = request.program_scopes else {
        return Ok(ViewerGpuActiveTextureDemand::default());
    };
    let mut demand = ViewerGpuActiveTextureDemand::default();
    for (width, height) in [
        (scopes.bins(), 128),
        (scopes.waveform_width(), scopes.bins()),
        (256, 256),
    ] {
        demand.checked_add(
            ViewerGpuActiveTextureDemand::checked_texture(
                width,
                height,
                4,
                ViewerGpuActiveWorkingSetStage::ProgramScopes,
            )?,
            ViewerGpuActiveWorkingSetStage::ProgramScopes,
        )?;
    }
    Ok(demand)
}

fn estimate_display_calibration(
    request: &ViewerGpuExecutionRequest<'_>,
) -> Result<ViewerGpuActiveTextureDemand, ViewerGpuActiveWorkingSetEstimateError> {
    let Some(calibration) = request.display_calibration.as_ref() else {
        return Ok(ViewerGpuActiveTextureDemand::default());
    };
    let mut demand = ViewerGpuActiveTextureDemand::checked_texture(
        request.output_width,
        request.output_height,
        8,
        ViewerGpuActiveWorkingSetStage::DisplayCalibration,
    )?;
    let edge = u64::from(calibration.edge_size());
    let lut_bytes = edge
        .checked_mul(edge)
        .and_then(|texels| texels.checked_mul(edge))
        .and_then(|texels| texels.checked_mul(16))
        .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
            stage: ViewerGpuActiveWorkingSetStage::DisplayCalibration,
        })?;
    demand.checked_add(
        ViewerGpuActiveTextureDemand { textures: 1, bytes: lut_bytes },
        ViewerGpuActiveWorkingSetStage::DisplayCalibration,
    )?;
    Ok(demand)
}

fn program_output_texture_format(
    request: &ViewerGpuExecutionRequest<'_>,
) -> GpuColorFrameTextureFormat {
    if request.monitor_adaptation.requires_pass() {
        GpuColorFrameTextureFormat::Rgba16Float
    } else {
        match request.output_precision {
            crate::ViewerGpuOutputPrecision::Encoded8 => GpuColorFrameTextureFormat::Rgba8Unorm,
            crate::ViewerGpuOutputPrecision::EncodedFloat16 => {
                GpuColorFrameTextureFormat::Rgba16Float
            }
        }
    }
}

fn estimate_presentation_output(
    request: &ViewerGpuExecutionRequest<'_>,
) -> Result<ViewerGpuActiveTextureDemand, ViewerGpuActiveWorkingSetEstimateError> {
    let texture_format =
        if request.display_calibration.is_some() || request.monitor_adaptation.requires_pass() {
            // The calibrated output and the monitor-adaptation output both
            // become RGBA16F presentation leases.
            GpuColorFrameTextureFormat::Rgba16Float
        } else {
            match request.output_precision {
                crate::ViewerGpuOutputPrecision::Encoded8 => GpuColorFrameTextureFormat::Rgba8Unorm,
                crate::ViewerGpuOutputPrecision::EncodedFloat16 => {
                    GpuColorFrameTextureFormat::Rgba16Float
                }
            }
        };
    ViewerGpuActiveTextureDemand::checked_texture(
        request.output_width,
        request.output_height,
        u64::from(texture_format.bytes_per_pixel()),
        ViewerGpuActiveWorkingSetStage::PresentationContinuityReserve,
    )
}

fn total_estimate(
    estimate: &ViewerGpuActiveWorkingSetEstimate,
) -> Result<ViewerGpuActiveTextureDemand, ViewerGpuActiveWorkingSetEstimateError> {
    let mut total = ViewerGpuActiveTextureDemand::default();
    for stage in [
        estimate.source_preparation,
        estimate.effects,
        estimate.transitions,
        estimate.working_composite,
        estimate.spatial,
        estimate.program_output,
        estimate.program_scopes,
        estimate.monitor_adaptation,
        estimate.display_calibration,
        estimate.detached_presentations,
        estimate.presentation_continuity_reserve,
    ] {
        total.checked_add(stage, ViewerGpuActiveWorkingSetStage::Total)?;
    }
    Ok(total)
}

fn validate_request_geometry(
    request: &ViewerGpuExecutionRequest<'_>,
) -> Result<(), ViewerGpuActiveWorkingSetEstimateError> {
    if request.width == 0 || request.height == 0 {
        return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
            reason: "working extent is empty",
        });
    }
    if request.output_width == 0 || request.output_height == 0 {
        return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
            reason: "Viewer output extent is empty",
        });
    }
    let rect = request.source_rect;
    if ![rect.x, rect.y, rect.width, rect.height].into_iter().all(f32::is_finite) {
        return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
            reason: "Viewer source rect contains a non-finite component",
        });
    }
    if rect.x < 0.0
        || rect.y < 0.0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x + rect.width > 1.0
        || rect.y + rect.height > 1.0
    {
        return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
            reason: "Viewer source rect is outside normalized bounds",
        });
    }
    let source_width = f64::from(request.width) * f64::from(rect.width);
    let source_height = f64::from(request.height) * f64::from(rect.height);
    let scale_x = f64::from(request.output_width) / source_width;
    let scale_y = f64::from(request.output_height) / source_height;
    let anisotropy = scale_x.max(scale_y) / scale_x.min(scale_y);
    if !anisotropy.is_finite() || anisotropy > 2.0 {
        return Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
            reason: "Viewer spatial scale anisotropy exceeds 2:1",
        });
    }
    Ok(())
}

fn checked_texture_bytes(
    width: u32,
    height: u32,
    bytes_per_pixel: u64,
    stage: ViewerGpuActiveWorkingSetStage,
) -> Result<u64, ViewerGpuActiveWorkingSetEstimateError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage })
}

fn native_surface_texture_bytes(
    width: u32,
    height: u32,
    format: DecodedVideoSurfaceFormat,
) -> Result<u64, ViewerGpuActiveWorkingSetEstimateError> {
    let stage = ViewerGpuActiveWorkingSetStage::SourcePreparation;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
    match format {
        DecodedVideoSurfaceFormat::Nv12 | DecodedVideoSurfaceFormat::Yuv420p => pixels
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(1))
            .map(|bytes| bytes / 2)
            .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage }),
        DecodedVideoSurfaceFormat::P010 | DecodedVideoSurfaceFormat::Yuv420p10le => pixels
            .checked_mul(3)
            .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage }),
        DecodedVideoSurfaceFormat::Rgba8 | DecodedVideoSurfaceFormat::Bgra8 => pixels
            .checked_mul(4)
            .ok_or(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow { stage }),
        DecodedVideoSurfaceFormat::Unknown | DecodedVideoSurfaceFormat::Other => {
            Err(ViewerGpuActiveWorkingSetEstimateError::InvalidRequest {
                reason: "native source has an unsupported decoded surface format",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CpuColorFrame, RenderMonitorAdaptation, RenderOutputColorBoundary, TimelineSolidColorLayer,
        ViewerGpuOutputPrecision,
    };
    use mondrian_core::display_calibration::{DisplayCalibrationLut3d, IccProfileFingerprint};
    use mondrian_core::{
        ensure_mondrian_default_ocio_loaded,
        types::{BlendMode, Color, ColorSpace, SequenceId},
        ColorEngine, WorkingColorSpace, WorkingRgbaF32Frame,
    };
    use mondrian_effects::{
        compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
    };
    use std::sync::Arc;

    fn identity_effect() -> (
        Arc<mondrian_effects::CompiledEffectGraph>,
        Arc<mondrian_effects::CompiledEffectGpuPlan>,
    ) {
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("identity graph");
        let plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("identity GPU effect plan"));
        (graph, plan)
    }

    fn with_request<R>(
        width: u32,
        height: u32,
        layers: &[ViewerGpuExecutionLayer],
        run: impl FnOnce(&ViewerGpuExecutionRequest<'_>) -> R,
    ) -> R {
        let boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("identity monitor adaptation");
        run(&ViewerGpuExecutionRequest {
            sequence_id: SequenceId::new(),
            timeline_frame: 0,
            width,
            height,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers,
            heterogeneous_inputs: Vec::new(),
            program_output_boundary: &boundary,
            monitor_adaptation: &monitor,
            source_rect: ViewerSourceRect::FULL,
            output_width: width,
            output_height: height,
            output_precision: ViewerGpuOutputPrecision::Encoded8,
            display_calibration: None,
            program_scopes: None,
        })
    }

    fn identity_display_calibration() -> Arc<DisplayCalibrationLut3d> {
        let edge = 17_u16;
        let denominator = f32::from(edge - 1);
        let mut samples = Vec::with_capacity(usize::from(edge).pow(3) * 4);
        for blue in 0..edge {
            for green in 0..edge {
                for red in 0..edge {
                    samples.extend_from_slice(&[
                        f32::from(red) / denominator,
                        f32::from(green) / denominator,
                        f32::from(blue) / denominator,
                        1.0,
                    ]);
                }
            }
        }
        Arc::new(
            DisplayCalibrationLut3d::from_rgba32f_samples(
                ColorSpace::Rec709,
                IccProfileFingerprint::from_bytes(b"viewer-working-set-calibration"),
                edge,
                samples,
            )
            .expect("identity display calibration"),
        )
    }

    #[test]
    fn empty_frame_still_accounts_for_transparent_composite_and_program_output() {
        with_request(4, 4, &[], |request| {
            let estimate =
                estimate_viewer_gpu_active_working_set(request).expect("empty frame estimate");
            assert_eq!(
                estimate.working_composite,
                ViewerGpuActiveTextureDemand { textures: 2, bytes: 4 * 4 * 16 * 2 }
            );
            assert_eq!(
                estimate.program_output,
                ViewerGpuActiveTextureDemand { textures: 1, bytes: 4 * 4 * 4 }
            );
            assert_eq!(
                estimate.total(),
                ViewerGpuActiveTextureDemand { textures: 3, bytes: 4 * 4 * (16 * 2 + 4) }
            );
        });
    }

    #[test]
    fn multiple_cpu_layers_add_independent_upload_residency() {
        let (_, effect_plan) = identity_effect();
        let frame = || {
            CpuColorFrame::working(WorkingRgbaF32Frame {
                width: 4,
                height: 4,
                data: vec![[0.0, 0.0, 0.0, 1.0]; 16],
                color_space: WorkingColorSpace::LinearRec709,
            })
        };
        let layers = [
            ViewerGpuExecutionLayer::Source(ViewerGpuSourceLayer::Media {
                frame: Some(frame()),
                gpu_source: None,
                native_source: None,
                heterogeneous_input: None,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: Arc::clone(&effect_plan),
                frame_seed: 0,
            }),
            ViewerGpuExecutionLayer::Source(ViewerGpuSourceLayer::Media {
                frame: Some(frame()),
                gpu_source: None,
                native_source: None,
                heterogeneous_input: None,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan,
                frame_seed: 0,
            }),
        ];

        with_request(4, 4, &layers, |request| {
            let estimate =
                estimate_viewer_gpu_active_working_set(request).expect("multi-layer estimate");
            assert_eq!(
                estimate.source_preparation,
                ViewerGpuActiveTextureDemand { textures: 2, bytes: 4 * 4 * 16 * 2 }
            );
            assert_eq!(estimate.total().textures, 5);
        });
    }

    #[test]
    fn cross_dissolve_accounts_for_both_endpoint_accumulators_and_output() {
        let (graph, effect_plan) = identity_effect();
        let source = |color, seed| {
            ViewerGpuTransitionInput::Source(ViewerGpuSourceLayer::SolidColor {
                layer: TimelineSolidColorLayer {
                    color,
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: Arc::clone(&graph),
                    frame_seed: seed,
                },
                effect_plan: Arc::clone(&effect_plan),
            })
        };
        let layers = [ViewerGpuExecutionLayer::CrossDissolve(Box::new(
            crate::ViewerGpuCrossDissolveLayer {
                left: source(Color::BLACK, 1),
                right: source(Color::WHITE, 2),
                progress: 0.5,
            },
        ))];

        with_request(4, 4, &layers, |request| {
            let estimate =
                estimate_viewer_gpu_active_working_set(request).expect("Cross Dissolve estimate");
            assert_eq!(
                estimate.transitions,
                ViewerGpuActiveTextureDemand { textures: 5, bytes: 4 * 4 * 16 * 5 }
            );
        });
    }

    #[test]
    fn oversized_extent_fails_checked_arithmetic() {
        with_request(u32::MAX, u32::MAX, &[], |request| {
            assert_eq!(
                estimate_viewer_gpu_active_working_set(request),
                Err(ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
                    stage: ViewerGpuActiveWorkingSetStage::WorkingComposite,
                })
            );
        });
    }

    #[test]
    fn active_grant_rejects_bytes_or_resource_count_before_recording() {
        with_request(4, 4, &[], |request| {
            let estimate =
                estimate_viewer_gpu_active_working_set(request).expect("empty frame estimate");
            let required = estimate.total();
            let bytes_error = ViewerGpuExecutionResourceGrant::new(0, 0)
                .with_active_limits(required.bytes - 1, required.textures)
                .admit_active_working_set(estimate)
                .expect_err("byte grant must reject");
            assert!(matches!(
                bytes_error,
                ViewerGpuActiveWorkingSetAdmissionError::GrantExceeded {
                    required_texture_bytes,
                    granted_texture_bytes,
                    ..
                } if required_texture_bytes == required.bytes
                    && granted_texture_bytes == required.bytes - 1
            ));
            let count_error = ViewerGpuExecutionResourceGrant::new(0, 0)
                .with_active_limits(required.bytes, required.textures - 1)
                .admit_active_working_set(estimate)
                .expect_err("texture-count grant must reject");
            assert!(matches!(
                count_error,
                ViewerGpuActiveWorkingSetAdmissionError::GrantExceeded {
                    required_textures,
                    granted_textures,
                    ..
                } if required_textures == required.textures
                    && granted_textures == required.textures - 1
            ));
        });
    }

    #[test]
    fn presentation_output_projection_matches_the_exact_final_lease_branch() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let identity_monitor = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("identity monitor adaptation");
        let adapted_monitor = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            ColorEngine::mondrian_standard(),
        )
        .expect("display-P3 monitor adaptation");
        let estimate_for =
            |monitor_adaptation: &RenderMonitorAdaptation,
             output_precision: ViewerGpuOutputPrecision,
             display_calibration: Option<Arc<DisplayCalibrationLut3d>>| {
                estimate_viewer_gpu_active_working_set(&ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 0,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[],
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &boundary,
                    monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision,
                    display_calibration,
                    program_scopes: None,
                })
                .expect("Viewer working-set estimate")
            };

        let encoded = estimate_for(&identity_monitor, ViewerGpuOutputPrecision::Encoded8, None);
        assert_eq!(encoded.presentation_output, encoded.program_output);
        assert_eq!(encoded.presentation_output.bytes, 4 * 4 * 4);

        let adapted = estimate_for(&adapted_monitor, ViewerGpuOutputPrecision::Encoded8, None);
        assert_eq!(adapted.presentation_output, adapted.monitor_adaptation);
        assert_eq!(adapted.presentation_output.bytes, 4 * 4 * 8);

        let calibrated = estimate_for(
            &identity_monitor,
            ViewerGpuOutputPrecision::EncodedFloat16,
            Some(identity_display_calibration()),
        );
        assert_eq!(calibrated.presentation_output.textures, 1);
        assert_eq!(calibrated.presentation_output.bytes, 4 * 4 * 8);
        assert!(
            calibrated.display_calibration.bytes > calibrated.presentation_output.bytes,
            "the persistent calibration LUT is not part of the detached output lease"
        );
    }

    #[test]
    fn presentation_reserve_makes_first_and_identical_second_admission_equal() {
        with_request(4, 4, &[], |request| {
            let candidate =
                estimate_viewer_gpu_active_working_set(request).expect("empty frame estimate");
            let output = candidate.presentation_output;

            let mut first = candidate;
            first
                .include_presentation_residency(0, 0)
                .expect("first-frame continuity reserve");
            assert_eq!(
                first.detached_presentations,
                ViewerGpuActiveTextureDemand::default()
            );
            assert_eq!(first.presentation_continuity_reserve, output);

            let exact_steady_state_grant = ViewerGpuExecutionResourceGrant::new(0, 0)
                .with_active_limits(first.total().bytes, first.total().textures);
            exact_steady_state_grant
                .admit_active_working_set(first)
                .expect("first frame fits the exact steady-state grant");

            let mut second = candidate;
            second
                .include_presentation_residency(output.textures, output.bytes)
                .expect("second-frame detached output");
            assert_eq!(second.detached_presentations, output);
            assert_eq!(
                second.presentation_continuity_reserve,
                ViewerGpuActiveTextureDemand::default()
            );
            assert_eq!(second.total(), first.total());
            exact_steady_state_grant
                .admit_active_working_set(second)
                .expect("identical second frame fits the same exact grant");
        });
    }

    #[test]
    fn grant_without_continuity_headroom_rejects_the_first_frame() {
        with_request(4, 4, &[], |request| {
            let candidate =
                estimate_viewer_gpu_active_working_set(request).expect("empty frame estimate");
            let candidate_only = candidate.total();
            let mut first = candidate;
            first
                .include_presentation_residency(0, 0)
                .expect("first-frame continuity reserve");

            let error = ViewerGpuExecutionResourceGrant::new(0, 0)
                .with_active_limits(candidate_only.bytes, candidate_only.textures)
                .admit_active_working_set(first)
                .expect_err("a non-replaceable first frame must fail before recording");
            assert!(matches!(
                error,
                ViewerGpuActiveWorkingSetAdmissionError::GrantExceeded {
                    required_texture_bytes,
                    required_textures,
                    ..
                } if required_texture_bytes
                    == candidate_only.bytes + candidate.presentation_output.bytes
                    && required_textures
                        == candidate_only.textures + candidate.presentation_output.textures
            ));
        });
    }

    #[test]
    fn multiple_live_detached_outputs_fail_closed_before_aggregate_estimation() {
        with_request(4, 4, &[], |request| {
            let candidate =
                estimate_viewer_gpu_active_working_set(request).expect("empty frame estimate");
            let output = candidate.presentation_output;
            let detached = ViewerGpuActiveTextureDemand {
                textures: output.textures * 2,
                bytes: output.bytes * 2,
            };
            let mut estimate = candidate;
            let error = estimate
                .include_presentation_residency(detached.textures, detached.bytes)
                .expect_err("capacity-one presentation ownership must reject multiple outputs");

            assert_eq!(
                error,
                ViewerGpuActiveWorkingSetEstimateError::PresentationCapacityExceeded {
                    live_outputs: 2
                }
            );
            assert_eq!(
                estimate, candidate,
                "failed admission must not publish a misleading aggregate estimate"
            );
        });
    }
}
