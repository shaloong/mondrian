//! Exact Program picture-sample scheduling.
//!
//! Interlaced output is not a codec flag. This Module maps one encoded Program
//! picture to two full-raster progressive evaluations at exact field instants.
//! Effects and compositors therefore keep their ordinary progressive-frame
//! Interface while animation, transitions, nested Sequences, temporal demand,
//! seeds, and caches observe distinct exact times.

use mondrian_core::{timeline_data::FieldOrder, FramePosition, Rational};

/// Temporal role of one canonical progressive Program sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureSamplePhase {
    /// One complete progressive encoded picture.
    Progressive,
    /// First field in display time.
    FirstField,
    /// Second field in display time.
    SecondField,
}

/// Line parity extracted from a progressive sample during interlaced assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureFieldLineParity {
    /// Top field: zero-based even rows.
    Top,
    /// Bottom field: zero-based odd rows.
    Bottom,
}

/// One exact progressive full-raster Program evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PictureSampleAddress {
    encoded_frame: u64,
    phase: PictureSamplePhase,
    position: FramePosition,
    field_line_parity: Option<PictureFieldLineParity>,
}

impl PictureSampleAddress {
    /// Encoded output picture owning this sample.
    pub const fn encoded_frame(self) -> u64 {
        self.encoded_frame
    }

    /// Progressive or display-order field role.
    pub const fn phase(self) -> PictureSamplePhase {
        self.phase
    }

    /// Exact Sequence-local evaluation coordinate.
    pub const fn position(self) -> FramePosition {
        self.position
    }

    /// Row parity extracted during interlaced assembly.
    pub const fn field_line_parity(self) -> Option<PictureFieldLineParity> {
        self.field_line_parity
    }

    /// Rebase this semantic sample role onto an exact range-local position.
    ///
    /// Export ranges can begin after Sequence origin; the phase/parity identity
    /// remains unchanged while the evaluation coordinate moves with that range.
    pub const fn with_position(mut self, position: FramePosition) -> Self {
        self.position = position;
        self
    }
}

/// Samples required to produce one encoded Program picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramPictureSamples {
    /// One ordinary progressive sample.
    Progressive(PictureSampleAddress),
    /// Two samples in display-time order.
    Interlaced {
        /// Dominant/first displayed field sample.
        first: PictureSampleAddress,
        /// Second displayed field sample.
        second: PictureSampleAddress,
    },
}

/// Immutable Program scan scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramPictureSampling {
    frame_time_base: Rational,
    field_order: FieldOrder,
}

impl ProgramPictureSampling {
    /// Bind the Sequence frame grid and Program Output scan identity.
    pub const fn new(frame_time_base: Rational, field_order: FieldOrder) -> Self {
        Self { frame_time_base, field_order }
    }

    /// Time base used by exact field evaluations.
    pub fn field_time_base(self) -> Option<Rational> {
        (self.field_order != FieldOrder::Progressive).then(|| {
            Rational::new(
                self.frame_time_base.num,
                self.frame_time_base
                    .den
                    .checked_mul(2)
                    .expect("qualified Sequence time base can be doubled"),
            )
        })
    }

    /// Resolve all progressive working samples for one encoded output frame.
    pub fn samples(
        self,
        encoded_frame: u64,
    ) -> Result<ProgramPictureSamples, PictureSamplingError> {
        let encoded_frame =
            i64::try_from(encoded_frame).map_err(|_| PictureSamplingError::FrameIndexOverflow)?;
        if self.field_order == FieldOrder::Progressive {
            return Ok(ProgramPictureSamples::Progressive(PictureSampleAddress {
                encoded_frame: encoded_frame as u64,
                phase: PictureSamplePhase::Progressive,
                position: FramePosition::new(encoded_frame, self.frame_time_base),
                field_line_parity: None,
            }));
        }
        let first_index =
            encoded_frame.checked_mul(2).ok_or(PictureSamplingError::FrameIndexOverflow)?;
        let second_index =
            first_index.checked_add(1).ok_or(PictureSamplingError::FrameIndexOverflow)?;
        let field_time_base =
            self.field_time_base().ok_or(PictureSamplingError::ProgressiveHasNoFieldGrid)?;
        let (first_parity, second_parity) = match self.field_order {
            FieldOrder::UpperFirst => (PictureFieldLineParity::Top, PictureFieldLineParity::Bottom),
            FieldOrder::LowerFirst => (PictureFieldLineParity::Bottom, PictureFieldLineParity::Top),
            FieldOrder::Progressive => unreachable!("handled above"),
        };
        Ok(ProgramPictureSamples::Interlaced {
            first: PictureSampleAddress {
                encoded_frame: encoded_frame as u64,
                phase: PictureSamplePhase::FirstField,
                position: FramePosition::new(first_index, field_time_base),
                field_line_parity: Some(first_parity),
            },
            second: PictureSampleAddress {
                encoded_frame: encoded_frame as u64,
                phase: PictureSamplePhase::SecondField,
                position: FramePosition::new(second_index, field_time_base),
                field_line_parity: Some(second_parity),
            },
        })
    }
}

/// Invalid Program picture-sample request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PictureSamplingError {
    /// Encoded frame or doubled field index exceeded the exact coordinate range.
    #[error("Program picture sample index overflowed")]
    FrameIndexOverflow,
    /// Internal misuse requested a field grid for progressive output.
    #[error("progressive Program Output has no field grid")]
    ProgressiveHasNoFieldGrid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::TimelineTime;

    #[test]
    fn interlaced_samples_have_distinct_exact_times_and_display_parity() {
        let time_base = Rational::new(1, 25);
        let samples = ProgramPictureSampling::new(time_base, FieldOrder::UpperFirst)
            .samples(7)
            .expect("samples");
        let ProgramPictureSamples::Interlaced { first, second } = samples else {
            panic!("interlaced samples")
        };
        assert_eq!(
            first.position(),
            FramePosition::new(14, Rational::new(1, 50))
        );
        assert_eq!(
            second.position(),
            FramePosition::new(15, Rational::new(1, 50))
        );
        assert_eq!(first.field_line_parity(), Some(PictureFieldLineParity::Top));
        assert_eq!(
            second.field_line_parity(),
            Some(PictureFieldLineParity::Bottom)
        );
        assert_eq!(
            TimelineTime::from_frame_position(second.position()).expect("time"),
            TimelineTime::new(3, 10).expect("exact 0.3 seconds")
        );
    }

    #[test]
    fn lower_first_swaps_extraction_parity_without_swapping_time() {
        let ProgramPictureSamples::Interlaced { first, second } =
            ProgramPictureSampling::new(Rational::new(1001, 30000), FieldOrder::LowerFirst)
                .samples(0)
                .expect("samples")
        else {
            panic!("interlaced samples")
        };
        assert_eq!(first.phase(), PictureSamplePhase::FirstField);
        assert_eq!(
            first.field_line_parity(),
            Some(PictureFieldLineParity::Bottom)
        );
        assert_eq!(
            second.field_line_parity(),
            Some(PictureFieldLineParity::Top)
        );
    }
}
