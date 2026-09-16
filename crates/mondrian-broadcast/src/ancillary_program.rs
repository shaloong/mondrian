use crate::{encode_st436_ancillary, AncillaryFrame, St436Error};
use mondrian_core::{Rational, TimelineTime};
use serde::{Deserialize, Serialize};

/// Immutable sparse canonical ANC program on one exact output frame grid.
/// Empty frames are explicit through the bounded duration, without allocating
/// a packet object per frame. This is an author-selected output attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenAncillaryProgram {
    source_start: TimelineTime,
    output_frame_rate: Rational,
    frame_count: u64,
    nonempty_frames: Vec<AncillaryFrame>,
    #[serde(default)]
    caption_source: Option<crate::CaptionImportReceipt>,
}

impl FrozenAncillaryProgram {
    /// Freeze exact Timeline origin, output cadence/duration and ordered packets.
    pub fn new(
        source_start: TimelineTime,
        output_frame_rate: Rational,
        frame_count: u64,
        nonempty_frames: Vec<AncillaryFrame>,
    ) -> Result<Self, St436Error> {
        let result = Self {
            source_start,
            output_frame_rate,
            frame_count,
            nonempty_frames,
            caption_source: None,
        };
        result.validate()?;
        Ok(result)
    }
    /// Validate untrusted deserialized programs and standard-carriage closure.
    pub fn validate(&self) -> Result<(), St436Error> {
        if self.source_start < TimelineTime::ZERO
            || self.output_frame_rate.num <= 0
            || self.output_frame_rate.den <= 0
            || self.frame_count == 0
            || self.frame_count > 100_000_000
            || self.nonempty_frames.len() > 1_000_000
        {
            return Err(St436Error::Extent);
        }
        let mut previous = None;
        let mut bytes = 0usize;
        for frame in &self.nonempty_frames {
            if frame.frame_index() >= self.frame_count
                || frame.packets().is_empty()
                || previous.is_some_and(|prior| frame.frame_index() <= prior)
            {
                return Err(St436Error::CanonicalMismatch);
            }
            bytes = bytes
                .checked_add(encode_st436_ancillary(frame)?.len())
                .filter(|bytes| *bytes <= 128 * 1024 * 1024)
                .ok_or(St436Error::Extent)?;
            previous = Some(frame.frame_index());
        }
        crate::captions::validate_program_caption_source(self)
            .map_err(|_| St436Error::CanonicalMismatch)?;
        Ok(())
    }
    /// Prove that this attachment belongs to the resolved export selection.
    pub fn bind(
        &self,
        source_start: TimelineTime,
        output_frame_rate: Rational,
        frame_count: u64,
    ) -> Result<(), St436Error> {
        self.validate()?;
        if self.source_start != source_start
            || self.output_frame_rate != output_frame_rate
            || self.frame_count != frame_count
        {
            return Err(St436Error::CanonicalMismatch);
        }
        Ok(())
    }
    /// Exact bounded number of output frames, including empty ANC inventories.
    pub const fn frame_count(&self) -> u64 {
        self.frame_count
    }
    /// Exact selected Timeline origin retained by this attachment.
    pub const fn source_start(&self) -> TimelineTime {
        self.source_start
    }
    /// Exact output cadence retained by this attachment.
    pub const fn output_frame_rate(&self) -> Rational {
        self.output_frame_rate
    }
    /// Packet inventory count across nonempty frames, without materializing gaps.
    pub fn packet_count(&self) -> usize {
        self.nonempty_frames.iter().map(|frame| frame.packets().len()).sum()
    }
    /// Borrow the existing sparse inventory for bounded provider admission.
    /// Indices remain selection-relative; this never expands empty frame gaps.
    pub fn nonempty_frames(&self) -> &[AncillaryFrame] {
        &self.nonempty_frames
    }
    /// Materialize only the requested canonical frame; never resample or infer.
    pub fn frame(&self, index: u64) -> Result<AncillaryFrame, St436Error> {
        if index >= self.frame_count {
            return Err(St436Error::Extent);
        }
        Ok(
            match self.nonempty_frames.binary_search_by_key(&index, AncillaryFrame::frame_index) {
                Ok(position) => self.nonempty_frames[position].clone(),
                Err(_) => AncillaryFrame::empty(index),
            },
        )
    }
    /// Original caption-source provenance, without any Semantic upgrade.
    pub fn caption_source(&self) -> Option<&crate::CaptionImportReceipt> {
        self.caption_source.as_ref()
    }
    pub(crate) fn with_caption_source(mut self, receipt: crate::CaptionImportReceipt) -> Self {
        self.caption_source = Some(receipt);
        self
    }
    /// Verify real MXF ANC essence against this sparse canonical program.
    pub fn verify_mxf(
        &self,
        reader: &mut impl std::io::Read,
        maximum_bytes: u64,
    ) -> Result<u64, St436Error> {
        self.validate()?;
        crate::st436::verify_mxf_frames(
            reader,
            self.frame_count,
            |index| self.frame(index),
            maximum_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        write_st436_klv_frame, AncillaryField, AncillaryOrigin, AncillaryPacket,
        AncillaryPlacement, AncillarySpace, AncillaryValidationLevel, St291Type2Packet,
    };
    fn frame(index: u64) -> AncillaryFrame {
        AncillaryFrame::new(
            index,
            vec![AncillaryPacket {
                placement: AncillaryPlacement::new(
                    AncillarySpace::Vanc,
                    AncillaryField::Progressive,
                    20,
                    0,
                )
                .expect("placement"),
                packet: St291Type2Packet::from_8bit_payload(0x61, 1, &[1, 2]).expect("packet"),
                origin: AncillaryOrigin::Derived,
                validation: AncillaryValidationLevel::Semantic,
            }],
        )
        .expect("frame")
    }
    #[test]
    fn sparse_program_requires_exact_owner_grid_and_real_empty_frames() {
        let rate = Rational::new(25, 1);
        let program = FrozenAncillaryProgram::new(TimelineTime::ZERO, rate, 3, vec![frame(1)])
            .expect("program");
        assert!(program.bind(TimelineTime::ZERO, rate, 3).is_ok());
        assert!(program.bind(TimelineTime::ZERO, Rational::new(24, 1), 3).is_err());
        assert!(program.bind(TimelineTime::ZERO, rate, 2).is_err());
        assert!(program.frame(0).expect("empty").packets().is_empty());
        assert!(program.frame(3).is_err());
        let mut klv = Vec::new();
        for index in 0..3 {
            write_st436_klv_frame(&mut klv, &program.frame(index).expect("frame")).expect("write");
        }
        assert_eq!(
            program.verify_mxf(&mut klv.as_slice(), klv.len() as u64).expect("verified"),
            3
        );
        assert!(program.verify_mxf(&mut klv.as_slice(), klv.len() as u64 - 1).is_err());
        let mut missing = Vec::new();
        write_st436_klv_frame(&mut missing, &frame(1)).expect("write");
        assert!(program.verify_mxf(&mut missing.as_slice(), missing.len() as u64).is_err());
    }
    #[test]
    fn out_of_range_duplicate_unsorted_and_unbounded_programs_reject() {
        let rate = Rational::new(25, 1);
        for frames in [
            vec![frame(3)],
            vec![frame(1), frame(1)],
            vec![frame(2), frame(1)],
            vec![AncillaryFrame::empty(1)],
        ] {
            assert!(FrozenAncillaryProgram::new(TimelineTime::ZERO, rate, 3, frames).is_err());
        }
        assert!(FrozenAncillaryProgram::new(TimelineTime::ZERO, rate, 0, vec![]).is_err());
        assert!(
            FrozenAncillaryProgram::new(TimelineTime::ZERO, rate, 100_000_001, vec![]).is_err()
        );
    }
}
