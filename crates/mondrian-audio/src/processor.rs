//! Allocation-free parameter delivery shared by built-in and future hosted processors.

use mondrian_core::ParameterId;
use std::ops::Range;

/// One exact definition-domain value change inside a processor block.
///
/// Values are not normalized to a plugin ABI. A concrete VST3, CLAP, GPU, or
/// native Adapter owns that final conversion after it has negotiated the real
/// parameter definition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioParameterEvent {
    /// Zero-based sample-frame offset from the start of the current block.
    pub sample_offset: u32,
    /// Exact value in the parameter's captured definition domain.
    pub value: f64,
}

/// Borrowed sample-accurate parameter events for one generated processor occurrence.
///
/// Lanes follow the immutable prepared parameter order and each lane is sorted
/// by `sample_offset`. Constant parameters contain one event at offset zero;
/// varying curves contain an exact value for every sample frame. The Session
/// owns and reuses the backing storage, so callback execution does not allocate.
#[derive(Debug, Clone, Copy)]
pub struct AudioParameterEventBatch<'a> {
    block_start_sample: i64,
    block_frames: usize,
    parameter_ids: &'a [ParameterId],
    lane_ranges: &'a [Range<usize>],
    events: &'a [AudioParameterEvent],
}

impl<'a> AudioParameterEventBatch<'a> {
    pub(crate) fn new(
        block_start_sample: i64,
        block_frames: usize,
        parameter_ids: &'a [ParameterId],
        lane_ranges: &'a [Range<usize>],
        events: &'a [AudioParameterEvent],
    ) -> Self {
        debug_assert_eq!(parameter_ids.len(), lane_ranges.len());
        Self {
            block_start_sample,
            block_frames,
            parameter_ids,
            lane_ranges,
            events,
        }
    }

    /// First absolute Sequence-domain sample frame represented by this batch.
    pub const fn block_start_sample(self) -> i64 {
        self.block_start_sample
    }

    /// Number of sample frames represented by this batch.
    pub const fn block_frames(self) -> usize {
        self.block_frames
    }

    /// Number of stable parameter lanes.
    pub const fn lane_count(self) -> usize {
        self.parameter_ids.len()
    }

    /// Stable parameter identity at one prepared lane.
    pub fn parameter_id(self, lane: usize) -> Option<&'a ParameterId> {
        self.parameter_ids.get(lane)
    }

    /// Ordered events for one prepared parameter lane.
    pub fn events(self, lane: usize) -> Option<&'a [AudioParameterEvent]> {
        let range = self.lane_ranges.get(lane)?.clone();
        self.events.get(range)
    }

    /// Total event count across all parameter lanes.
    pub const fn event_count(self) -> usize {
        self.events.len()
    }
}
