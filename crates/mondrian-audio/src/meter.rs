//! Session-local, allocation-free Channel Strip metering.

use mondrian_core::{AudioChannelLayout, MixBusId, ProgramOutputId, TrackId};
use std::hint::spin_loop;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Stable post-mute Channel Strip observed by one meter lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AudioMeterTarget {
    /// One Sequence Track mixer channel.
    Track(TrackId),
    /// One Sequence Mix Bus.
    Bus(MixBusId),
    /// One public Program Output.
    ProgramOutput(ProgramOutputId),
}

/// Per-channel facts measured from one completed block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioChannelMeterReading {
    /// Maximum finite absolute sample value. Values above one are retained.
    pub sample_peak_linear: f32,
    /// Root-mean-square amplitude over finite samples in the block.
    pub rms_linear: f64,
    /// Finite samples whose absolute amplitude exceeded digital full scale.
    pub clipped_sample_count: u64,
    /// NaN or infinite samples observed on this channel.
    pub non_finite_sample_count: u64,
}

impl Default for AudioChannelMeterReading {
    fn default() -> Self {
        Self {
            sample_peak_linear: 0.0,
            rms_linear: 0.0,
            clipped_sample_count: 0,
            non_finite_sample_count: 0,
        }
    }
}

/// One Channel Strip's readings inside a completed meter frame.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMeterTargetFrame {
    /// Stable Sequence-owned Channel Strip identity.
    pub target: AudioMeterTarget,
    /// Canonical interleaved-order channel readings.
    pub channels: Vec<AudioChannelMeterReading>,
}

/// Owned observation snapshot for one successfully completed Session block.
///
/// All targets carry the same block serial and exact Sequence sample range.
/// This is a sample-peak/RMS meter, not an EBU R128/ATSC A/85 loudness result
/// and not an oversampled true-peak meter. Loudness/true-peak analysis must be
/// a separately versioned observation stage with its own filter, window,
/// gating, and channel-weight contract.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMeterFrame {
    /// Monotonic successful-block serial within one Session.
    pub block_serial: u64,
    /// First exact Sequence-domain sample in this block.
    pub start_sample: i64,
    /// Exact sample-frame count.
    pub frames: usize,
    /// Semantic Program Output layout shared by every prepared node.
    pub channel_layout: AudioChannelLayout,
    /// Track, Bus, and Program Output readings in prepared topological order.
    pub targets: Vec<AudioMeterTargetFrame>,
}

impl AudioMeterFrame {
    /// Look up one stable Channel Strip reading.
    pub fn target(&self, target: AudioMeterTarget) -> Option<&AudioMeterTargetFrame> {
        self.targets.iter().find(|frame| frame.target == target)
    }
}

/// Shared read-only endpoint for one Session's Channel Strip observations.
///
/// Clone this handle before a Session enters an audio callback. The writer
/// publishes the complete target bank through fixed atomic slots without
/// allocation or locking; [`Self::latest`] allocates an owned snapshot and
/// belongs on a control, UI, or analysis thread.
#[derive(Debug, Clone)]
pub struct AudioMeterObserver {
    published: Arc<PublishedAudioMeterBank>,
}

impl AudioMeterObserver {
    /// Read one internally consistent latest-completed-block snapshot.
    pub fn latest(&self) -> AudioMeterFrame {
        self.published.snapshot()
    }
}

#[derive(Debug)]
struct AtomicChannelMeterReading {
    sample_peak_linear: AtomicU32,
    rms_linear: AtomicU64,
    clipped_sample_count: AtomicU64,
    non_finite_sample_count: AtomicU64,
}

impl Default for AtomicChannelMeterReading {
    fn default() -> Self {
        Self {
            sample_peak_linear: AtomicU32::new(0.0_f32.to_bits()),
            rms_linear: AtomicU64::new(0.0_f64.to_bits()),
            clipped_sample_count: AtomicU64::new(0),
            non_finite_sample_count: AtomicU64::new(0),
        }
    }
}

#[derive(Debug)]
struct PublishedAudioMeterBank {
    channel_layout: AudioChannelLayout,
    targets: Vec<AudioMeterTarget>,
    publication_sequence: AtomicU64,
    block_serial: AtomicU64,
    start_sample: AtomicI64,
    frames: AtomicUsize,
    channels: Vec<AtomicChannelMeterReading>,
}

impl PublishedAudioMeterBank {
    fn new(channel_layout: AudioChannelLayout, targets: &[AudioMeterTarget]) -> Self {
        let channel_states = targets.len().saturating_mul(channel_layout.channel_count());
        Self {
            channel_layout,
            targets: targets.to_vec(),
            publication_sequence: AtomicU64::new(0),
            block_serial: AtomicU64::new(0),
            start_sample: AtomicI64::new(0),
            frames: AtomicUsize::new(0),
            channels: (0..channel_states).map(|_| AtomicChannelMeterReading::default()).collect(),
        }
    }

    fn publish(&self, frame: &AudioMeterFrame) {
        debug_assert_eq!(frame.channel_layout, self.channel_layout);
        debug_assert_eq!(frame.targets.len(), self.targets.len());
        // One Session writer toggles this bank-wide sequence odd while
        // replacing every target and even only after the whole block is
        // visible. Readers therefore never combine two render blocks.
        self.publication_sequence.fetch_add(1, Ordering::SeqCst);
        self.block_serial.store(frame.block_serial, Ordering::SeqCst);
        self.start_sample.store(frame.start_sample, Ordering::SeqCst);
        self.frames.store(frame.frames, Ordering::SeqCst);
        let source_channels = frame.targets.iter().flat_map(|target| &target.channels);
        for (source, destination) in source_channels.zip(&self.channels) {
            destination
                .sample_peak_linear
                .store(source.sample_peak_linear.to_bits(), Ordering::SeqCst);
            destination.rms_linear.store(source.rms_linear.to_bits(), Ordering::SeqCst);
            destination
                .clipped_sample_count
                .store(source.clipped_sample_count, Ordering::SeqCst);
            destination
                .non_finite_sample_count
                .store(source.non_finite_sample_count, Ordering::SeqCst);
        }
        self.publication_sequence.fetch_add(1, Ordering::SeqCst);
    }

    fn snapshot(&self) -> AudioMeterFrame {
        loop {
            let before = self.publication_sequence.load(Ordering::SeqCst);
            if before & 1 != 0 {
                spin_loop();
                continue;
            }
            let block_serial = self.block_serial.load(Ordering::SeqCst);
            let start_sample = self.start_sample.load(Ordering::SeqCst);
            let frames = self.frames.load(Ordering::SeqCst);
            let channels = self
                .channels
                .iter()
                .map(|channel| AudioChannelMeterReading {
                    sample_peak_linear: f32::from_bits(
                        channel.sample_peak_linear.load(Ordering::SeqCst),
                    ),
                    rms_linear: f64::from_bits(channel.rms_linear.load(Ordering::SeqCst)),
                    clipped_sample_count: channel.clipped_sample_count.load(Ordering::SeqCst),
                    non_finite_sample_count: channel.non_finite_sample_count.load(Ordering::SeqCst),
                })
                .collect::<Vec<_>>();
            let after = self.publication_sequence.load(Ordering::SeqCst);
            if before == after {
                let channel_count = self.channel_layout.channel_count();
                let targets = self
                    .targets
                    .iter()
                    .copied()
                    .zip(channels.chunks_exact(channel_count))
                    .map(|(target, channels)| AudioMeterTargetFrame {
                        target,
                        channels: channels.to_vec(),
                    })
                    .collect();
                return AudioMeterFrame {
                    block_serial,
                    start_sample,
                    frames,
                    channel_layout: self.channel_layout,
                    targets,
                };
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct AudioMeterBank {
    frame: AudioMeterFrame,
    sum_squares: Vec<f64>,
    finite_samples: Vec<u64>,
    measured_targets: Vec<bool>,
    published: Arc<PublishedAudioMeterBank>,
}

impl AudioMeterBank {
    pub(crate) fn new(
        channel_layout: AudioChannelLayout,
        targets: impl IntoIterator<Item = AudioMeterTarget>,
    ) -> Self {
        let targets = targets.into_iter().collect::<Vec<_>>();
        let channel_count = channel_layout.channel_count();
        let target_frames = targets
            .iter()
            .copied()
            .map(|target| AudioMeterTargetFrame {
                target,
                channels: vec![AudioChannelMeterReading::default(); channel_count],
            })
            .collect::<Vec<_>>();
        let channel_states = target_frames.len().saturating_mul(channel_count);
        Self {
            frame: AudioMeterFrame {
                block_serial: 0,
                start_sample: 0,
                frames: 0,
                channel_layout,
                targets: target_frames,
            },
            sum_squares: vec![0.0; channel_states],
            finite_samples: vec![0; channel_states],
            measured_targets: vec![false; targets.len()],
            published: Arc::new(PublishedAudioMeterBank::new(channel_layout, &targets)),
        }
    }

    pub(crate) fn begin_block(&mut self, block_serial: u64, start_sample: i64, frames: usize) {
        self.frame.block_serial = block_serial;
        self.frame.start_sample = start_sample;
        self.frame.frames = frames;
        self.measured_targets.fill(false);
    }

    pub(crate) fn measure_target(&mut self, target_index: usize, interleaved: &[f32]) -> bool {
        let channel_count = self.frame.channel_layout.channel_count();
        let Some(expected_samples) = self.frame.frames.checked_mul(channel_count) else {
            return false;
        };
        if interleaved.len() != expected_samples {
            return false;
        }
        let Some(target) = self.frame.targets.get_mut(target_index) else {
            return false;
        };
        let Some(measured) = self.measured_targets.get_mut(target_index) else {
            return false;
        };
        let base = target_index.saturating_mul(channel_count);
        let end = base.saturating_add(channel_count);
        let Some(sum_squares) = self.sum_squares.get_mut(base..end) else {
            return false;
        };
        let Some(finite_samples) = self.finite_samples.get_mut(base..end) else {
            return false;
        };
        sum_squares.fill(0.0);
        finite_samples.fill(0);
        target.channels.fill(AudioChannelMeterReading::default());
        for frame in interleaved.chunks_exact(channel_count) {
            for (channel, sample) in frame.iter().copied().enumerate() {
                let reading = &mut target.channels[channel];
                if !sample.is_finite() {
                    reading.non_finite_sample_count =
                        reading.non_finite_sample_count.saturating_add(1);
                    continue;
                }
                let magnitude = sample.abs();
                reading.sample_peak_linear = reading.sample_peak_linear.max(magnitude);
                if magnitude > 1.0 {
                    reading.clipped_sample_count = reading.clipped_sample_count.saturating_add(1);
                }
                let sample = f64::from(sample);
                sum_squares[channel] += sample * sample;
                finite_samples[channel] = finite_samples[channel].saturating_add(1);
            }
        }
        for (channel, reading) in target.channels.iter_mut().enumerate() {
            if finite_samples[channel] > 0 {
                reading.rms_linear = (sum_squares[channel] / finite_samples[channel] as f64).sqrt();
            }
        }
        *measured = true;
        true
    }

    pub(crate) fn publish_completed_block(&self) -> bool {
        if !self.measured_targets.iter().all(|measured| *measured) {
            return false;
        }
        self.published.publish(&self.frame);
        true
    }

    pub(crate) fn snapshot(&self) -> AudioMeterFrame {
        self.published.snapshot()
    }

    pub(crate) fn observer(&self) -> AudioMeterObserver {
        AudioMeterObserver { published: Arc::clone(&self.published) }
    }

    pub(crate) fn target_count(&self) -> usize {
        self.frame.targets.len()
    }

    pub(crate) fn channel_state_count(&self) -> usize {
        self.frame
            .targets
            .len()
            .saturating_mul(self.frame.channel_layout.channel_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> [AudioMeterTarget; 2] {
        [
            AudioMeterTarget::Track(TrackId::new()),
            AudioMeterTarget::ProgramOutput(ProgramOutputId::new()),
        ]
    }

    #[test]
    fn bank_keeps_unclipped_peak_rms_and_invalid_sample_evidence() {
        let targets = targets();
        let mut meter = AudioMeterBank::new(AudioChannelLayout::Stereo, targets);
        let observer = meter.observer();
        meter.begin_block(1, 12, 3);
        assert!(meter.measure_target(0, &[0.5, -0.5, 2.0, f32::NAN, -1.0, f32::INFINITY]));
        assert!(meter.measure_target(1, &[0.25, -0.25, 0.5, -0.5, 0.75, -0.75]));
        assert!(meter.publish_completed_block());
        let frame = observer.latest();

        assert_eq!(frame.block_serial, 1);
        assert_eq!(frame.start_sample, 12);
        assert_eq!(frame.frames, 3);
        let track = frame.target(targets[0]).expect("Track meter");
        assert_eq!(track.channels[0].sample_peak_linear, 2.0);
        assert_eq!(track.channels[0].clipped_sample_count, 1);
        assert_eq!(track.channels[0].non_finite_sample_count, 0);
        assert!((track.channels[0].rms_linear - (5.25_f64 / 3.0).sqrt()).abs() < 1.0e-12);
        assert_eq!(track.channels[1].sample_peak_linear, 0.5);
        assert_eq!(track.channels[1].non_finite_sample_count, 2);
        assert_eq!(track.channels[1].rms_linear, 0.5);
    }

    #[test]
    fn incomplete_block_never_replaces_the_last_complete_bank() {
        let targets = targets();
        let mut meter = AudioMeterBank::new(AudioChannelLayout::Stereo, targets);
        let observer = meter.observer();
        meter.begin_block(1, 0, 1);
        assert!(meter.measure_target(0, &[0.25, -0.25]));
        assert!(meter.measure_target(1, &[0.5, -0.5]));
        assert!(meter.publish_completed_block());

        meter.begin_block(2, 1, 1);
        assert!(meter.measure_target(0, &[1.0, -1.0]));
        assert!(!meter.publish_completed_block());

        let frame = observer.latest();
        assert_eq!(frame.block_serial, 1);
        assert_eq!(frame.start_sample, 0);
    }

    #[test]
    fn observer_never_reads_a_torn_concurrent_bank_publication() {
        const FINAL_SERIAL: u64 = 1_000;
        let targets = targets();
        let mut meter = AudioMeterBank::new(AudioChannelLayout::Stereo, targets);
        let observer = meter.observer();
        let writer = std::thread::spawn(move || {
            for serial in 1..=FINAL_SERIAL {
                let value = serial as f32;
                meter.begin_block(serial, serial as i64, 1);
                assert!(meter.measure_target(0, &[value, -value]));
                assert!(meter.measure_target(1, &[value * 2.0, -value * 2.0]));
                assert!(meter.publish_completed_block());
            }
        });

        loop {
            let frame = observer.latest();
            if frame.block_serial > 0 {
                let expected = frame.block_serial as f32;
                assert_eq!(frame.start_sample, frame.block_serial as i64);
                assert_eq!(frame.frames, 1);
                assert_eq!(frame.targets[0].channels[0].sample_peak_linear, expected);
                assert_eq!(
                    frame.targets[1].channels[0].sample_peak_linear,
                    expected * 2.0
                );
            }
            if frame.block_serial == FINAL_SERIAL {
                break;
            }
        }
        writer.join().expect("meter writer");
    }
}
