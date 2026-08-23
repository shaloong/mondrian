//! Checked active-resource admission for one renderer GPU output boundary.
//!
//! The device-scoped color-frame pool governs only idle texture retention.
//! This module instead accounts for the input texture, output texture, and
//! padded readback buffer that remain live while one output boundary is being
//! recorded and completed. The estimate is derived from the already-lowered
//! renderer resource plan, so callers do not need to reinterpret color or
//! precision semantics.

use crate::{GpuColorFrameHandle, RenderGpuOutputStageResourcePlan};

/// Active GPU resources attributed to one output-boundary stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderGpuOutputActiveResourceDemand {
    /// Conservative logical bytes held by the resources.
    pub bytes: u64,
    /// Number of independently allocated GPU resources.
    pub resources: u64,
}

impl RenderGpuOutputActiveResourceDemand {
    fn checked_add(
        &mut self,
        other: Self,
        stage: RenderGpuOutputActiveWorkingSetStage,
    ) -> Result<(), RenderGpuOutputActiveWorkingSetEstimateError> {
        self.bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or(RenderGpuOutputActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        self.resources = self
            .resources
            .checked_add(other.resources)
            .ok_or(RenderGpuOutputActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        Ok(())
    }
}

/// Stable stage attribution for output-boundary resource diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderGpuOutputActiveWorkingSetStage {
    /// GPU working texture consumed by the color transform.
    InputTexture,
    /// Encoded GPU target produced by the color transform.
    OutputTexture,
    /// Padded GPU-to-CPU transfer buffer.
    ReadbackBuffer,
    /// Aggregate of all output-boundary resources.
    Total,
}

/// Conservative active-resource estimate for one exact GPU output plan.
///
/// The input texture is included even when an upstream GPU stage created it:
/// it remains live and referenced by this boundary. Idle pool residency,
/// backend pipeline/LUT caches, decoder surfaces, and presentation surfaces
/// have independent owners and are intentionally excluded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderGpuOutputActiveWorkingSetEstimate {
    /// Working input texture.
    pub input_texture: RenderGpuOutputActiveResourceDemand,
    /// Encoded output texture.
    pub output_texture: RenderGpuOutputActiveResourceDemand,
    /// Optional padded readback buffer.
    pub readback_buffer: RenderGpuOutputActiveResourceDemand,
    total: RenderGpuOutputActiveResourceDemand,
}

impl RenderGpuOutputActiveWorkingSetEstimate {
    /// Aggregate active demand across the complete output boundary.
    pub const fn total(self) -> RenderGpuOutputActiveResourceDemand {
        self.total
    }
}

/// Failure to derive an output-boundary estimate with checked arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RenderGpuOutputActiveWorkingSetEstimateError {
    /// Pixel, byte, or resource-count arithmetic overflowed.
    #[error("GPU output active working-set arithmetic overflowed in {stage:?}")]
    ArithmeticOverflow {
        /// First stage whose exact arithmetic could not be represented.
        stage: RenderGpuOutputActiveWorkingSetStage,
    },
}

/// Hard owner-scoped grant for one active GPU output boundary.
///
/// This grant is independent from idle texture retention. A product owner may
/// trim idle caches under pressure, but it must freeze this active grant for
/// one accepted Preview frame or Export attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderGpuOutputExecutionResourceGrant {
    max_active_bytes: u64,
    max_active_resources: u64,
}

impl RenderGpuOutputExecutionResourceGrant {
    /// Construct exact active-byte and resource-count limits.
    pub const fn new(max_active_bytes: u64, max_active_resources: u64) -> Self {
        Self { max_active_bytes, max_active_resources }
    }

    /// Maximum logical active GPU bytes admitted for one boundary.
    pub const fn max_active_bytes(self) -> u64 {
        self.max_active_bytes
    }

    /// Maximum independently allocated active GPU resources.
    pub const fn max_active_resources(self) -> u64 {
        self.max_active_resources
    }

    /// Admit one already-checked active-resource estimate.
    pub fn admit(
        self,
        estimate: RenderGpuOutputActiveWorkingSetEstimate,
    ) -> Result<(), RenderGpuOutputActiveWorkingSetAdmissionError> {
        let required = estimate.total();
        if required.bytes > self.max_active_bytes || required.resources > self.max_active_resources
        {
            return Err(
                RenderGpuOutputActiveWorkingSetAdmissionError::GrantExceeded {
                    required_bytes: required.bytes,
                    granted_bytes: self.max_active_bytes,
                    required_resources: required.resources,
                    granted_resources: self.max_active_resources,
                },
            );
        }
        Ok(())
    }

    /// Estimate and admit one exact lowered output resource plan.
    pub fn admit_plan(
        self,
        plan: &RenderGpuOutputStageResourcePlan,
    ) -> Result<
        RenderGpuOutputActiveWorkingSetEstimate,
        RenderGpuOutputActiveWorkingSetAdmissionError,
    > {
        let estimate = estimate_render_gpu_output_active_working_set(plan)?;
        self.admit(estimate)?;
        Ok(estimate)
    }
}

impl Default for RenderGpuOutputExecutionResourceGrant {
    fn default() -> Self {
        Self::new(u64::MAX, u64::MAX)
    }
}

/// Failure to estimate or admit one renderer GPU output working set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RenderGpuOutputActiveWorkingSetAdmissionError {
    /// The lowered resource plan could not be represented exactly.
    #[error(transparent)]
    Estimate(#[from] RenderGpuOutputActiveWorkingSetEstimateError),
    /// The exact plan exceeded either owner-scoped limit.
    #[error(
        "GPU output active working set requires {required_bytes} bytes across {required_resources} resources, but the owner grants {granted_bytes} bytes across {granted_resources} resources"
    )]
    GrantExceeded {
        /// Conservative active-byte demand.
        required_bytes: u64,
        /// Owner-scoped active-byte grant.
        granted_bytes: u64,
        /// Conservative active-resource demand.
        required_resources: u64,
        /// Owner-scoped active-resource grant.
        granted_resources: u64,
    },
}

/// Estimate one exact lowered output plan without creating GPU resources.
pub fn estimate_render_gpu_output_active_working_set(
    plan: &RenderGpuOutputStageResourcePlan,
) -> Result<RenderGpuOutputActiveWorkingSetEstimate, RenderGpuOutputActiveWorkingSetEstimateError> {
    let input_texture = texture_demand(
        &plan.input,
        RenderGpuOutputActiveWorkingSetStage::InputTexture,
    )?;
    let output_texture = texture_demand(
        &plan.output,
        RenderGpuOutputActiveWorkingSetStage::OutputTexture,
    )?;
    let readback_buffer =
        plan.readback
            .as_ref()
            .map_or(RenderGpuOutputActiveResourceDemand::default(), |readback| {
                RenderGpuOutputActiveResourceDemand { bytes: readback.buffer_size, resources: 1 }
            });
    assemble_estimate(input_texture, output_texture, readback_buffer)
}

fn texture_demand(
    handle: &GpuColorFrameHandle,
    stage: RenderGpuOutputActiveWorkingSetStage,
) -> Result<RenderGpuOutputActiveResourceDemand, RenderGpuOutputActiveWorkingSetEstimateError> {
    let descriptor = handle.descriptor();
    let bytes = u64::from(descriptor.width)
        .checked_mul(u64::from(descriptor.height))
        .and_then(|pixels| pixels.checked_mul(u64::from(handle.texture_format().bytes_per_pixel())))
        .ok_or(RenderGpuOutputActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
    Ok(RenderGpuOutputActiveResourceDemand { bytes, resources: 1 })
}

fn assemble_estimate(
    input_texture: RenderGpuOutputActiveResourceDemand,
    output_texture: RenderGpuOutputActiveResourceDemand,
    readback_buffer: RenderGpuOutputActiveResourceDemand,
) -> Result<RenderGpuOutputActiveWorkingSetEstimate, RenderGpuOutputActiveWorkingSetEstimateError> {
    let mut total = RenderGpuOutputActiveResourceDemand::default();
    total.checked_add(input_texture, RenderGpuOutputActiveWorkingSetStage::Total)?;
    total.checked_add(output_texture, RenderGpuOutputActiveWorkingSetStage::Total)?;
    total.checked_add(readback_buffer, RenderGpuOutputActiveWorkingSetStage::Total)?;
    Ok(RenderGpuOutputActiveWorkingSetEstimate {
        input_texture,
        output_texture,
        readback_buffer,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_accounts_for_textures_and_padded_readback() {
        let input = RenderGpuOutputActiveResourceDemand { bytes: 3840 * 2160 * 16, resources: 1 };
        let output = RenderGpuOutputActiveResourceDemand { bytes: 3840 * 2160 * 8, resources: 1 };
        let readback = RenderGpuOutputActiveResourceDemand { bytes: 3840 * 2160 * 8, resources: 1 };
        let estimate = assemble_estimate(input, output, readback).expect("checked estimate");

        assert_eq!(estimate.input_texture, input);
        assert_eq!(estimate.output_texture, output);
        assert_eq!(estimate.readback_buffer, readback);
        assert_eq!(
            estimate.total(),
            RenderGpuOutputActiveResourceDemand { bytes: 3840 * 2160 * 32, resources: 3 }
        );
    }

    #[test]
    fn grant_checks_bytes_and_resource_count_independently() {
        let estimate = assemble_estimate(
            RenderGpuOutputActiveResourceDemand { bytes: 64, resources: 1 },
            RenderGpuOutputActiveResourceDemand { bytes: 32, resources: 1 },
            RenderGpuOutputActiveResourceDemand { bytes: 16, resources: 1 },
        )
        .expect("checked estimate");

        RenderGpuOutputExecutionResourceGrant::new(112, 3)
            .admit(estimate)
            .expect("exact grant");
        assert!(matches!(
            RenderGpuOutputExecutionResourceGrant::new(111, 3).admit(estimate),
            Err(
                RenderGpuOutputActiveWorkingSetAdmissionError::GrantExceeded {
                    required_bytes: 112,
                    granted_bytes: 111,
                    required_resources: 3,
                    granted_resources: 3,
                }
            )
        ));
        assert!(matches!(
            RenderGpuOutputExecutionResourceGrant::new(112, 2).admit(estimate),
            Err(
                RenderGpuOutputActiveWorkingSetAdmissionError::GrantExceeded {
                    required_bytes: 112,
                    granted_bytes: 112,
                    required_resources: 3,
                    granted_resources: 2,
                }
            )
        ));
    }

    #[test]
    fn aggregate_overflow_fails_closed() {
        let error = assemble_estimate(
            RenderGpuOutputActiveResourceDemand { bytes: u64::MAX, resources: 1 },
            RenderGpuOutputActiveResourceDemand { bytes: 1, resources: 1 },
            RenderGpuOutputActiveResourceDemand::default(),
        )
        .expect_err("overflow must fail closed");
        assert_eq!(
            error,
            RenderGpuOutputActiveWorkingSetEstimateError::ArithmeticOverflow {
                stage: RenderGpuOutputActiveWorkingSetStage::Total,
            }
        );
    }
}
