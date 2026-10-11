//! Bounded ownership of complete generated PCM, never a DSP checkpoint.

use crate::{AudioBuffer, AudioPcmRenderRequest, AudioPlaybackConfig};
use mondrian_core::{
    AudioChannelLayout, AudioPcmRendererMaterializationId, AudioSamplePosition, AudioSampleRate,
};

/// Why a retained complete-PCM range cannot be supplied by its exact owner.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioRetainedPcmError {
    /// Journal geometry or an allocation extent overflowed.
    #[error("retained PCM journal extent overflowed")]
    ExtentOverflow,
    /// The bounded slot owner could not be allocated.
    #[error("retained PCM journal slot allocation failed")]
    AllocationFailed,
    /// The requested held generation/renderer is no longer this journal's owner.
    #[error("retained PCM owner is stale")]
    StaleOwner,
    /// A popped old render operation has not yet delivered its completion.
    #[error("retained PCM old render work has not settled")]
    OldWorkUnsettled,
    /// No valid complete PCM covers the requested sample coordinate.
    #[error("retained PCM does not cover the requested sample coordinate")]
    CoverageUnavailable,
    /// The sample grid or semantic layout does not match this owner.
    #[error("retained PCM grid or layout does not match its owner")]
    ContractMismatch,
}

/// Read-only ownership/coverage facts for one exact held renderer materialization.
///
/// These facts do not establish a DAC position, private DSP checkpoint, lifecycle
/// preservation promise, whole-host memory grant, PDC migration or resume permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRetainedPcmEvidence {
    /// The actual execution owner that produced every retained window.
    pub renderer_id: AudioPcmRendererMaterializationId,
    /// Render generation under which the old complete windows were generated.
    pub source_generation: u64,
    /// Current blocked generation retaining these old windows.
    pub pending_generation: u64,
    /// Explicit frame grid for all retained PCM coordinates.
    pub sample_rate: AudioSampleRate,
    /// Semantic layout of the complete interleaved PCM.
    pub channel_layout: AudioChannelLayout,
    /// First requested sample whose complete PCM is retained.
    pub requested_start: AudioSamplePosition,
    /// End of the generated complete PCM suffix available through the visitor.
    pub completed_end: AudioSamplePosition,
    /// Oldest complete retained window; it may precede the requested start.
    pub retained_start: AudioSamplePosition,
    /// Number of occupied slots in this fixed journal owner.
    pub retained_windows: usize,
    /// Actual Vec slot capacity multiplied by the owned slot size.
    pub slot_capacity_bytes: usize,
    /// Sum of actual retained PCM Vec capacities in bytes, with no duplicated PCM.
    pub sample_capacity_bytes: usize,
    /// Fixed maximum retained sample-capacity bytes admitted before execution.
    pub sample_capacity_budget_bytes: usize,
}

/// Borrowed part of one existing complete PCM owner in chronological order.
///
/// `pcm` includes the complete original window. `first_frame` skips only the
/// requested prefix; the visitor must not reinterpret it as a DSP state snapshot.
#[derive(Debug, Clone, Copy)]
pub struct AudioRetainedPcmWindow<'a> {
    /// Exact start of the complete original render window.
    pub start_sample: AudioSamplePosition,
    /// Existing complete PCM owner, borrowed without cloning its sample Vec.
    pub pcm: &'a AudioBuffer,
    /// First frame belonging to the requested retained suffix.
    pub first_frame: usize,
}

struct Window {
    start: i64,
    end: i64,
    capacity_bytes: usize,
    pcm: AudioBuffer,
}

#[derive(Clone, Copy)]
struct Owner {
    renderer_id: AudioPcmRendererMaterializationId,
    generation: u64,
}

#[derive(Clone, Copy)]
struct Held {
    pending_generation: u64,
    awaiting_completions: usize,
}

pub(crate) struct PcmJournal {
    sample_rate: AudioSampleRate,
    channel_layout: AudioChannelLayout,
    slots: Vec<Option<Window>>,
    head: usize,
    len: usize,
    slot_bytes: usize,
    sample_budget: usize,
    sample_bytes: usize,
    owner: Option<Owner>,
    held: Option<Held>,
    completed_end: Option<i64>,
}

impl PcmJournal {
    pub(crate) fn for_playback_config(
        config: AudioPlaybackConfig,
    ) -> Result<Self, AudioRetainedPcmError> {
        if config.chunk_frames == 0 {
            return Err(AudioRetainedPcmError::ExtentOverflow);
        }
        let rate = AudioSampleRate::new(config.sample_rate)
            .map_err(|_| AudioRetainedPcmError::ContractMismatch)?;
        let queue_frames = usize::try_from(config.sample_rate)
            .ok()
            .and_then(|rate| rate.checked_mul(2))
            .ok_or(AudioRetainedPcmError::ExtentOverflow)?;
        let max_windows = queue_frames
            .div_ceil(config.chunk_frames)
            .checked_add(config.max_in_flight)
            .and_then(|windows| windows.checked_add(1))
            .ok_or(AudioRetainedPcmError::ExtentOverflow)?;
        let sample_budget = max_windows
            .checked_mul(config.chunk_frames)
            .and_then(|frames| frames.checked_mul(config.channel_layout.channel_count()))
            .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(AudioRetainedPcmError::ExtentOverflow)?;
        Self::new(rate, config.channel_layout, max_windows, sample_budget)
    }

    pub(crate) fn new(
        sample_rate: AudioSampleRate,
        channel_layout: AudioChannelLayout,
        max_windows: usize,
        sample_budget: usize,
    ) -> Result<Self, AudioRetainedPcmError> {
        if max_windows == 0 || sample_budget == 0 {
            return Err(AudioRetainedPcmError::ExtentOverflow);
        }
        max_windows
            .checked_mul(std::mem::size_of::<Option<Window>>())
            .ok_or(AudioRetainedPcmError::ExtentOverflow)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(max_windows)
            .map_err(|_| AudioRetainedPcmError::AllocationFailed)?;
        let slot_bytes = slots
            .capacity()
            .checked_mul(std::mem::size_of::<Option<Window>>())
            .ok_or(AudioRetainedPcmError::ExtentOverflow)?;
        slots.resize_with(max_windows, || None);
        Ok(Self {
            sample_rate,
            channel_layout,
            slots,
            head: 0,
            len: 0,
            slot_bytes,
            sample_budget,
            sample_bytes: 0,
            owner: None,
            held: None,
            completed_end: None,
        })
    }

    pub(crate) fn begin(
        &mut self,
        renderer_id: Option<AudioPcmRendererMaterializationId>,
        generation: u64,
    ) {
        self.clear_windows();
        self.owner = renderer_id.map(|renderer_id| Owner { renderer_id, generation });
        self.held = None;
        self.completed_end = None;
    }

    pub(crate) fn hold(
        &mut self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
        pending_generation: u64,
        awaiting_completions: usize,
    ) -> Result<(), AudioRetainedPcmError> {
        self.validate_owner(renderer_id, generation)?;
        self.held = Some(Held { pending_generation, awaiting_completions });
        Ok(())
    }

    pub(crate) fn expects_old_completion(
        &self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
    ) -> bool {
        self.validate_owner(renderer_id, generation).is_ok()
            && self.held.is_some_and(|held| held.awaiting_completions != 0)
    }

    pub(crate) fn settle_old_completion(&mut self) {
        if let Some(held) = &mut self.held {
            held.awaiting_completions = held.awaiting_completions.saturating_sub(1);
        }
    }

    pub(crate) fn old_work_unsettled(&self) -> bool {
        self.held.is_some_and(|held| held.awaiting_completions != 0)
    }

    pub(crate) fn advance_pending_generation(
        &mut self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
    ) {
        if self.owner.is_some_and(|owner| owner.renderer_id == renderer_id)
            && let Some(held) = &mut self.held
        {
            held.pending_generation = generation;
        } else {
            self.begin(None, generation);
        }
    }

    pub(crate) fn invalidate_suffix(&mut self) {
        self.clear_windows();
        self.completed_end = None;
    }

    /// Move an already validated complete owner; no PCM/slot allocation occurs.
    pub(crate) fn record(
        &mut self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
        request: AudioPcmRenderRequest,
        pcm: AudioBuffer,
    ) -> Result<(), AudioRetainedPcmError> {
        self.validate_owner(renderer_id, generation)?;
        if request.sample_rate != self.sample_rate.hz()
            || request.continuity.generation().get() != generation
            || request.channel_layout != self.channel_layout
            || pcm.sample_rate != request.sample_rate
            || pcm.channel_layout != request.channel_layout
            || request.frame_count.checked_mul(self.channel_layout.channel_count())
                != Some(pcm.samples.len())
            || request.frame_count == 0
        {
            self.invalidate_suffix();
            return Err(AudioRetainedPcmError::ContractMismatch);
        }
        let start = request.start_sample;
        let end = i64::try_from(request.frame_count)
            .ok()
            .and_then(|frames| start.checked_add(frames));
        let capacity_bytes = pcm.samples.capacity().checked_mul(std::mem::size_of::<f32>());
        let (Some(end), Some(capacity_bytes)) = (end, capacity_bytes) else {
            self.invalidate_suffix();
            return Err(AudioRetainedPcmError::ExtentOverflow);
        };
        // A discontinuity begins a new retained suffix; no gap is hidden by
        // copying or re-rendering the prior history.
        if self.completed_end.is_some_and(|previous| previous != start) {
            self.clear_windows();
        }
        self.completed_end = Some(end);
        if capacity_bytes > self.sample_budget {
            self.clear_windows();
            return Err(AudioRetainedPcmError::CoverageUnavailable);
        }
        while self.len == self.slots.len()
            || self.sample_bytes > self.sample_budget - capacity_bytes
        {
            self.evict_first();
        }
        let slot = (self.head + self.len) % self.slots.len();
        self.slots[slot] = Some(Window { start, end, capacity_bytes, pcm });
        self.sample_bytes += capacity_bytes;
        self.len += 1;
        Ok(())
    }

    pub(crate) fn visit(
        &self,
        renderer_id: AudioPcmRendererMaterializationId,
        pending_generation: u64,
        requested_start: AudioSamplePosition,
        mut visitor: impl FnMut(AudioRetainedPcmWindow<'_>),
    ) -> Result<AudioRetainedPcmEvidence, AudioRetainedPcmError> {
        let owner = self.owner.ok_or(AudioRetainedPcmError::StaleOwner)?;
        let held = self.held.ok_or(AudioRetainedPcmError::StaleOwner)?;
        if renderer_id != owner.renderer_id || pending_generation != held.pending_generation {
            return Err(AudioRetainedPcmError::StaleOwner);
        }
        if held.awaiting_completions != 0 {
            return Err(AudioRetainedPcmError::OldWorkUnsettled);
        }
        if requested_start.rate() != self.sample_rate {
            return Err(AudioRetainedPcmError::ContractMismatch);
        }
        let first = self.window(0).ok_or(AudioRetainedPcmError::CoverageUnavailable)?;
        let end = self.completed_end.ok_or(AudioRetainedPcmError::CoverageUnavailable)?;
        let requested = requested_start.sample();
        if requested < first.start || requested > end {
            return Err(AudioRetainedPcmError::CoverageUnavailable);
        }
        for ordinal in 0..self.len {
            let window = self.window(ordinal).ok_or(AudioRetainedPcmError::CoverageUnavailable)?;
            if window.end <= requested {
                continue;
            }
            let first_frame = usize::try_from(requested.max(window.start) - window.start)
                .map_err(|_| AudioRetainedPcmError::ExtentOverflow)?;
            visitor(AudioRetainedPcmWindow {
                start_sample: AudioSamplePosition::new(window.start, self.sample_rate),
                pcm: &window.pcm,
                first_frame,
            });
        }
        Ok(AudioRetainedPcmEvidence {
            renderer_id,
            source_generation: owner.generation,
            pending_generation,
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
            requested_start,
            completed_end: AudioSamplePosition::new(end, self.sample_rate),
            retained_start: AudioSamplePosition::new(first.start, self.sample_rate),
            retained_windows: self.len,
            slot_capacity_bytes: self.slot_bytes,
            sample_capacity_bytes: self.sample_bytes,
            sample_capacity_budget_bytes: self.sample_budget,
        })
    }

    fn validate_owner(
        &self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
    ) -> Result<(), AudioRetainedPcmError> {
        if !self
            .owner
            .is_some_and(|owner| owner.renderer_id == renderer_id && owner.generation == generation)
        {
            return Err(AudioRetainedPcmError::StaleOwner);
        }
        Ok(())
    }

    fn window(&self, ordinal: usize) -> Option<&Window> {
        (ordinal < self.len)
            .then(|| self.slots[(self.head + ordinal) % self.slots.len()].as_ref())
            .flatten()
    }

    fn evict_first(&mut self) {
        if self.len == 0 {
            return;
        }
        if let Some(window) = self.slots[self.head].take() {
            self.sample_bytes -= window.capacity_bytes;
        }
        self.head = (self.head + 1) % self.slots.len();
        self.len -= 1;
    }

    fn clear_windows(&mut self) {
        while self.len != 0 {
            self.evict_first();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm_allocation_probe::AllocationScope;
    use crate::{AudioPcmContinuity, AudioPcmRenderGeneration};

    fn rate() -> AudioSampleRate {
        AudioSampleRate::new(100).expect("positive grid")
    }

    fn request(start: i64, frames: usize) -> AudioPcmRenderRequest {
        AudioPcmRenderRequest {
            start_sample: start,
            frame_count: frames,
            sample_rate: 100,
            channel_layout: AudioChannelLayout::Mono,
            continuity: AudioPcmContinuity::Continue(AudioPcmRenderGeneration::new(7)),
        }
    }

    fn pcm(start: i64, frames: usize) -> AudioBuffer {
        AudioBuffer {
            samples: (0..frames).map(|index| (start + index as i64) as f32).collect(),
            sample_rate: 100,
            channel_layout: AudioChannelLayout::Mono,
        }
    }

    fn journal(
        max_windows: usize,
        budget: usize,
    ) -> (PcmJournal, AudioPcmRendererMaterializationId) {
        let owner = AudioPcmRendererMaterializationId::new();
        let mut journal = PcmJournal::new(rate(), AudioChannelLayout::Mono, max_windows, budget)
            .expect("bounded owner");
        journal.begin(Some(owner), 7);
        (journal, owner)
    }

    #[test]
    fn pcm_journal_full_capacity_record_visit_keeps_original_backings_without_allocation() {
        let (mut journal, owner) = journal(2, 32);
        let buffers = [pcm(0, 4), pcm(4, 4), pcm(8, 4), pcm(12, 4)];
        let original_pointers = buffers.each_ref().map(|buffer| buffer.samples.as_ptr());
        let original_capacities = buffers.each_ref().map(|buffer| buffer.samples.capacity());
        let mut suffix = [0.0; 7];
        let mut filled = 0;
        // Preparation, PCM allocation and scalar TLS initialization precede the
        // measured interval. Allocations by other threads are excluded.
        let allocations = AllocationScope::enter();
        for (ordinal, buffer) in buffers.into_iter().enumerate() {
            journal
                .record(owner, 7, request((ordinal * 4) as i64, 4), buffer)
                .expect("moved original owner");
        }
        journal.hold(owner, 7, 8, 0).expect("exact held materialization");
        let evidence = journal
            .visit(owner, 8, AudioSamplePosition::new(9, rate()), |window| {
                let ordinal = usize::try_from(window.start_sample.sample() / 4)
                    .expect("nonnegative original ordinal");
                assert_eq!(window.pcm.samples.as_ptr(), original_pointers[ordinal]);
                let samples = &window.pcm.samples[window.first_frame..];
                suffix[filled..filled + samples.len()].copy_from_slice(samples);
                filled += samples.len();
            })
            .expect("borrowed complete original suffix");
        let count = allocations.count();
        drop(allocations);
        assert_eq!(count, 0, "record/evict/hold/visit may not allocate");
        assert_eq!(filled, 7);
        assert_eq!(suffix, [9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
        assert_eq!(
            evidence.sample_capacity_bytes,
            (original_capacities[2] + original_capacities[3]) * std::mem::size_of::<f32>()
        );
        assert_eq!(
            evidence.slot_capacity_bytes,
            journal.slots.capacity() * std::mem::size_of::<Option<Window>>()
        );
        assert_eq!(evidence.retained_start.sample(), 8);
        assert_eq!(evidence.completed_end.sample(), 16);
    }

    #[test]
    fn pcm_journal_refuses_stale_owner_grid_and_unsettled_old_work_before_visiting() {
        let (mut journal, owner) = journal(2, 32);
        journal.record(owner, 7, request(0, 4), pcm(0, 4)).expect("complete PCM");
        journal.hold(owner, 7, 8, 1).expect("one actually popped owner");
        assert_eq!(
            journal
                .visit(owner, 8, AudioSamplePosition::new(1, rate()), |_| panic!(
                    "no premature visit"
                ))
                .expect_err("unsettled"),
            AudioRetainedPcmError::OldWorkUnsettled
        );
        journal.settle_old_completion();
        assert_eq!(
            journal
                .visit(
                    AudioPcmRendererMaterializationId::new(),
                    8,
                    AudioSamplePosition::new(1, rate()),
                    |_| panic!("wrong owner")
                )
                .expect_err("owner"),
            AudioRetainedPcmError::StaleOwner
        );
        assert_eq!(
            journal
                .visit(owner, 9, AudioSamplePosition::new(1, rate()), |_| panic!(
                    "wrong generation"
                ))
                .expect_err("generation"),
            AudioRetainedPcmError::StaleOwner
        );
        let wrong_grid =
            AudioSamplePosition::new(1, AudioSampleRate::new(101).expect("other positive grid"));
        assert_eq!(
            journal.visit(owner, 8, wrong_grid, |_| panic!("wrong grid")).expect_err("grid"),
            AudioRetainedPcmError::ContractMismatch
        );
    }

    #[test]
    fn pcm_journal_counts_actual_capacity_and_refuses_unadmitted_large_backing() {
        let (mut journal, owner) = journal(2, 32);
        let mut samples = Vec::with_capacity(32);
        samples.extend_from_slice(&[0.0; 4]);
        let buffer = AudioBuffer {
            samples,
            sample_rate: 100,
            channel_layout: AudioChannelLayout::Mono,
        };
        assert_eq!(
            journal
                .record(owner, 7, request(0, 4), buffer)
                .expect_err("actual capacity exceeds grant"),
            AudioRetainedPcmError::CoverageUnavailable
        );
        journal.hold(owner, 7, 8, 0).expect("held owner");
        assert_eq!(
            journal
                .visit(owner, 8, AudioSamplePosition::new(0, rate()), |_| panic!(
                    "no invented retained PCM"
                ))
                .expect_err("coverage"),
            AudioRetainedPcmError::CoverageUnavailable
        );
        assert_eq!(journal.sample_bytes, 0);
    }

    #[test]
    fn pcm_journal_window_gap_preserves_only_the_new_known_suffix() {
        let (mut journal, owner) = journal(2, 32);
        journal.record(owner, 7, request(0, 4), pcm(0, 4)).expect("old complete window");
        journal
            .record(owner, 7, request(8, 4), pcm(8, 4))
            .expect("later complete window");
        journal.hold(owner, 7, 8, 0).expect("held owner");
        assert_eq!(
            journal
                .visit(owner, 8, AudioSamplePosition::new(4, rate()), |_| panic!(
                    "gap is never filled"
                ))
                .expect_err("gap"),
            AudioRetainedPcmError::CoverageUnavailable
        );
        let evidence = journal
            .visit(owner, 8, AudioSamplePosition::new(8, rate()), |window| {
                assert_eq!(window.pcm.samples, [8.0, 9.0, 10.0, 11.0])
            })
            .expect("actual suffix");
        assert_eq!(evidence.retained_windows, 1);
        assert_eq!(evidence.completed_end.sample(), 12);
    }

    #[test]
    fn pcm_journal_retains_signed_coordinates_without_clamping_or_replaying() {
        let (mut journal, owner) = journal(2, 32);
        journal
            .record(owner, 7, request(-7, 4), pcm(-7, 4))
            .expect("negative first window");
        journal
            .record(owner, 7, request(-3, 4), pcm(-3, 4))
            .expect("negative continuation");
        journal.hold(owner, 7, 8, 0).expect("held owner");
        let mut suffix = [0.0; 5];
        let mut filled = 0;
        let evidence = journal
            .visit(owner, 8, AudioSamplePosition::new(-4, rate()), |window| {
                let samples = &window.pcm.samples[window.first_frame..];
                suffix[filled..filled + samples.len()].copy_from_slice(samples);
                filled += samples.len();
            })
            .expect("actual signed suffix");
        assert_eq!(suffix, [-4.0, -3.0, -2.0, -1.0, 0.0]);
        assert_eq!(evidence.completed_end.sample(), 1);
    }

    #[test]
    fn pcm_journal_device_generation_change_keeps_old_owner_but_seek_retires_it() {
        let (mut journal, owner) = journal(2, 32);
        journal.record(owner, 7, request(0, 4), pcm(0, 4)).expect("original PCM");
        journal.hold(owner, 7, 8, 1).expect("one outstanding old completion");
        journal.advance_pending_generation(owner, 9);
        assert_eq!(
            journal
                .visit(owner, 8, AudioSamplePosition::new(0, rate()), |_| panic!(
                    "stale pending generation"
                ))
                .expect_err("old pending"),
            AudioRetainedPcmError::StaleOwner
        );
        journal.settle_old_completion();
        let evidence = journal
            .visit(owner, 9, AudioSamplePosition::new(0, rate()), |_| {})
            .expect("same original generation under new pending output");
        assert_eq!(evidence.source_generation, 7);
        assert_eq!(evidence.pending_generation, 9);
        journal.begin(Some(owner), 10);
        assert_eq!(
            journal
                .visit(owner, 9, AudioSamplePosition::new(0, rate()), |_| panic!(
                    "seek retired old PCM"
                ))
                .expect_err("retired"),
            AudioRetainedPcmError::StaleOwner
        );
    }

    #[test]
    fn pcm_journal_invalid_window_extent_never_leaves_an_invented_complete_head() {
        let (mut journal, owner) = journal(2, 32);
        journal.record(owner, 7, request(0, 4), pcm(0, 4)).expect("old PCM");
        assert_eq!(
            journal
                .record(owner, 7, request(i64::MAX, 2), pcm(0, 2))
                .expect_err("invalid signed end"),
            AudioRetainedPcmError::ExtentOverflow
        );
        journal.hold(owner, 7, 8, 0).expect("held owner");
        assert_eq!(
            journal
                .visit(owner, 8, AudioSamplePosition::new(0, rate()), |_| panic!(
                    "no fabricated head after invalid extent"
                ))
                .expect_err("invalid complete head"),
            AudioRetainedPcmError::CoverageUnavailable
        );
    }
}
