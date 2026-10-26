//! One actual processor invocation owns observation, budget and Source service.
use super::*;
use crate::openfx_host::{
    map_openfx_input_error, OpenFxInputRequest, OpenFxRenderObservation, OpenFxRenderOwner,
    OpenFxResolvedInput,
};
use mondrian_effects::{EffectProcessorInputRequest, EffectProcessorQuestionState};

pub(super) struct ProcessorRenderOwner<'a, 'bank, 'callback> {
    pub control: &'a mut EffectProcessorControl<'callback>,
    pub resources: &'a mut mondrian_effects::EffectProcessorResources<'bank>,
    pub memory_refusal: &'a mut Option<mondrian_effects::EffectProcessorTransientMemoryRefusal>,
    pub source: bool,
    pub frame_extent: mondrian_effects::EffectFrameExtent,
    pub format: mondrian_effects::EffectValueFormat,
    pub sampling: mondrian_core::ImageSampling,
    pub author_frame_rate: mondrian_core::Rational,
}

fn input_unavailable(reason: impl Into<String>) -> OpenFxHostError {
    OpenFxHostError::InputFailed(Box::new(
        mondrian_effects::EffectExecutionError::InputUnavailable { reason: reason.into() },
    ))
}

fn observation_error(
    error: impl std::fmt::Display,
    control: &EffectProcessorControl<'_>,
) -> OpenFxHostError {
    match control.stop_reason() {
        Some(EffectProcessorStopReason::DeadlineExpired) => OpenFxHostError::DeadlineExceeded,
        Some(EffectProcessorStopReason::Canceled) => OpenFxHostError::Canceled,
        None => OpenFxHostError::Unavailable(error.to_string()),
    }
}

impl OpenFxRenderOwner for ProcessorRenderOwner<'_, '_, '_> {
    fn observe(
        &mut self,
        observation: OpenFxRenderObservation<'_>,
    ) -> Result<EffectProcessorQuestionState, OpenFxHostError> {
        match observation {
            OpenFxRenderObservation::Question(question) => self
                .control
                .observe_question(question)
                .map_err(|error| observation_error(error, self.control)),
            OpenFxRenderObservation::QuestionEnded(question) => {
                self.control.finish_question(question);
                Ok(EffectProcessorQuestionState::Unavailable)
            }
            OpenFxRenderObservation::Progress(progress) => {
                self.control
                    .observe_progress(progress)
                    .map_err(|error| observation_error(error, self.control))?;
                processor_checkpoint(self.control)?;
                Ok(EffectProcessorQuestionState::Unavailable)
            }
            OpenFxRenderObservation::Checkpoint => {
                processor_checkpoint(self.control)?;
                Ok(EffectProcessorQuestionState::Unavailable)
            }
        }
    }

    fn acquire_memory(&mut self, bytes: usize) -> Result<bool, OpenFxHostError> {
        match self.resources.acquire_transient(bytes) {
            Ok(()) => Ok(true),
            Err(refusal) => {
                *self.memory_refusal = Some(refusal);
                Ok(false)
            }
        }
    }

    fn resolves_input(&self) -> bool {
        self.source && self.control.resolves_processor_input()
    }

    fn resolve_input(
        &mut self,
        request: OpenFxInputRequest,
    ) -> Result<OpenFxResolvedInput, OpenFxHostError> {
        processor_checkpoint(self.control)?;
        let time = native_frame_time(request.time, self.author_frame_rate)?;
        let (output, [x, y]) = self
            .resources
            .resolve_input_f32(
                EffectProcessorInputRequest {
                    time,
                    frame_extent: self.frame_extent,
                    format: self.format,
                    sampling: self.sampling,
                    deadline: request.deadline,
                },
                request.held_native_bytes,
                self.control,
            )
            .map_err(map_openfx_input_error)?;
        processor_checkpoint(self.control)?;
        let tile = output.into_tile();
        let extent = tile.frame_extent();
        let sampling = tile.sampling();
        if tile.roi() != extent.full_frame_roi() {
            return Err(input_unavailable(
                "processor input service returned a partial full-frame value",
            ));
        }
        let pixels = tile.try_into_pixels().map_err(|_| {
            input_unavailable(
                "processor input pixels remain shared; an unadmitted copy is forbidden",
            )
        })?;
        let frame = mondrian_effects::EffectFloatFrame::from_pixels(
            extent,
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            sampling,
            pixels,
        )
        .map_err(|error| input_unavailable(error.to_string()))?;
        Ok(OpenFxResolvedInput { frame, x, y })
    }
}

/// Interpret the actual native double exactly, then apply the original rational
/// frame period. No guessed microsecond grid or inverse projection of native fps.
fn native_frame_time(
    value: f64,
    rate: mondrian_core::Rational,
) -> Result<mondrian_core::TimelineTime, OpenFxHostError> {
    let bad =
        || input_unavailable("native input time cannot be represented on the exact author clock");
    if !value.is_finite() || rate.num <= 0 || rate.den <= 0 {
        return Err(bad());
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mut mantissa = bits & ((1u64 << 52) - 1);
    if exponent != 0 {
        mantissa |= 1u64 << 52;
    }
    if mantissa == 0 {
        return Ok(mondrian_core::TimelineTime::ZERO);
    }
    let mut power = if exponent == 0 {
        -1074
    } else {
        exponent - 1023 - 52
    };
    let zeros = mantissa.trailing_zeros();
    mantissa >>= zeros;
    power += zeros as i32;
    let (numerator, denominator) = if power >= 0 {
        if power > 63 {
            return Err(bad());
        }
        (i128::from(mantissa) * (1i128 << power), 1i64)
    } else {
        if power < -62 {
            return Err(bad());
        }
        (i128::from(mantissa), 1i64 << -power)
    };
    let numerator = if value.is_sign_negative() {
        -numerator
    } else {
        numerator
    };
    let numerator = i64::try_from(numerator).map_err(|_| bad())?;
    let time = mondrian_core::TimelineTime::new(numerator, denominator).map_err(|_| bad())?;
    time.checked_scale(mondrian_core::TimeScale::new(rate.den, rate.num).map_err(|_| bad())?)
        .map_err(|_| bad())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_time_uses_exact_double_and_original_rational_rate() {
        let rate = mondrian_core::Rational::new(24, 1);
        for (frame, n, d) in [
            (-1.5, -1, 16),
            (2.25, 3, 32),
            (-0.0, 0, 1),
            (0.1, 3602879701896397, 864691128455135232),
        ] {
            assert_eq!(
                native_frame_time(frame, rate).unwrap(),
                mondrian_core::TimelineTime::new(n, d).unwrap()
            );
        }
        assert_eq!(
            native_frame_time(6.0, mondrian_core::Rational::new(24000, 1001)).unwrap(),
            mondrian_core::TimelineTime::new(1001, 4000).unwrap()
        );
        for frame in [f64::NAN, f64::INFINITY, f64::MAX, f64::from_bits(1)] {
            assert!(native_frame_time(frame, rate).is_err());
        }
        assert!(native_frame_time(1.0, mondrian_core::Rational::new(0, 1)).is_err());
    }
}
