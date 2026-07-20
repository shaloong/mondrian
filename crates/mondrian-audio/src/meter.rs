//! Session-local, allocation-free Program Output block metering.

use mondrian_core::AudioChannelLayout;
use std::hint::spin_loop;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Per-channel facts measured from one completed Program Output block.
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

/// Owned observation snapshot for the latest completed output block.
///
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
    /// Semantic Program Output layout.
    pub channel_layout: AudioChannelLayout,
    /// Canonical interleaved-order channel readings.
    pub channels: Vec<AudioChannelMeterReading>,
}

/// Shared read-only endpoint for Program Output meter observations.
///
/// Clone this handle before a Session enters an audio callback. The writer
/// publishes through fixed atomic slots without allocation or locking;
/// `latest` allocates an owned snapshot and belongs on a control, UI, or
/// analysis thread.
#[derive(Debug, Clone)]
pub struct AudioMeterObserver {
    published: Arc<PublishedAudioMeter>,
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
struct PublishedAudioMeter {
    channel_layout: AudioChannelLayout,
    publication_sequence: AtomicU64,
    block_serial: AtomicU64,
    start_sample: AtomicI64,
    frames: AtomicUsize,
    channels: Vec<AtomicChannelMeterReading>,
}

impl PublishedAudioMeter {
    fn new(channel_layout: AudioChannelLayout) -> Self {
        let channels = channel_layout.channel_count();
        Self {
            channel_layout,
            publication_sequence: AtomicU64::new(0),
            block_serial: AtomicU64::new(0),
            start_sample: AtomicI64::new(0),
            frames: AtomicUsize::new(0),
            channels: (0..channels).map(|_| AtomicChannelMeterReading::default()).collect(),
        }
    }

    fn publish(&self, frame: &AudioMeterFrame) {
        debug_assert_eq!(frame.channel_layout, self.channel_layout);
        debug_assert_eq!(frame.channels.len(), self.channels.len());
        // A single Session writer toggles the sequence odd while replacing the
        // fixed snapshot and even only after every field is visible. Sequential
        // consistency keeps the seqlock proof independent of CPU memory model.
        self.publication_sequence.fetch_add(1, Ordering::SeqCst);
        self.block_serial.store(frame.block_serial, Ordering::SeqCst);
        self.start_sample.store(frame.start_sample, Ordering::SeqCst);
        self.frames.store(frame.frames, Ordering::SeqCst);
        for (source, destination) in frame.channels.iter().zip(&self.channels) {
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
                .collect();
            let after = self.publication_sequence.load(Ordering::SeqCst);
            if before == after {
                return AudioMeterFrame {
                    block_serial,
                    start_sample,
                    frames,
                    channel_layout: self.channel_layout,
                    channels,
                };
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProgramOutputMeter {
    frame: AudioMeterFrame,
    sum_squares: Vec<f64>,
    finite_samples: Vec<u64>,
    published: Arc<PublishedAudioMeter>,
}

impl ProgramOutputMeter {
    pub(crate) fn new(channel_layout: AudioChannelLayout) -> Self {
        let channels = channel_layout.channel_count();
        Self {
            frame: AudioMeterFrame {
                block_serial: 0,
                start_sample: 0,
                frames: 0,
                channel_layout,
                channels: vec![AudioChannelMeterReading::default(); channels],
            },
            sum_squares: vec![0.0; channels],
            finite_samples: vec![0; channels],
            published: Arc::new(PublishedAudioMeter::new(channel_layout)),
        }
    }

    pub(crate) fn observe(
        &mut self,
        start_sample: i64,
        frames: usize,
        interleaved: &[f32],
    ) -> bool {
        let channels = self.frame.channels.len();
        let Some(expected_samples) = frames.checked_mul(channels) else {
            return false;
        };
        if interleaved.len() != expected_samples {
            return false;
        }
        self.sum_squares.fill(0.0);
        self.finite_samples.fill(0);
        self.frame.channels.fill(AudioChannelMeterReading::default());
        for frame in interleaved.chunks_exact(channels) {
            for (channel, sample) in frame.iter().copied().enumerate() {
                let reading = &mut self.frame.channels[channel];
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
                self.sum_squares[channel] += sample * sample;
                self.finite_samples[channel] = self.finite_samples[channel].saturating_add(1);
            }
        }
        for (channel, reading) in self.frame.channels.iter_mut().enumerate() {
            if self.finite_samples[channel] > 0 {
                reading.rms_linear =
                    (self.sum_squares[channel] / self.finite_samples[channel] as f64).sqrt();
            }
        }
        self.frame.block_serial = self.frame.block_serial.saturating_add(1);
        self.frame.start_sample = start_sample;
        self.frame.frames = frames;
        self.published.publish(&self.frame);
        true
    }

    pub(crate) fn snapshot(&self) -> AudioMeterFrame {
        self.published.snapshot()
    }

    pub(crate) fn observer(&self) -> AudioMeterObserver {
        AudioMeterObserver { published: Arc::clone(&self.published) }
    }

    pub(crate) fn channel_state_count(&self) -> usize {
        self.frame.channels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_keeps_unclipped_peak_rms_and_invalid_sample_evidence() {
        let mut meter = ProgramOutputMeter::new(AudioChannelLayout::Stereo);
        let observer = meter.observer();
        assert!(meter.observe(12, 3, &[0.5, -0.5, 2.0, f32::NAN, -1.0, f32::INFINITY]));
        let frame = observer.latest();

        assert_eq!(frame.block_serial, 1);
        assert_eq!(frame.start_sample, 12);
        assert_eq!(frame.frames, 3);
        assert_eq!(frame.channels[0].sample_peak_linear, 2.0);
        assert_eq!(frame.channels[0].clipped_sample_count, 1);
        assert_eq!(frame.channels[0].non_finite_sample_count, 0);
        assert!((frame.channels[0].rms_linear - (5.25_f64 / 3.0).sqrt()).abs() < 1.0e-12);
        assert_eq!(frame.channels[1].sample_peak_linear, 0.5);
        assert_eq!(frame.channels[1].non_finite_sample_count, 2);
        assert_eq!(frame.channels[1].rms_linear, 0.5);
    }

    #[test]
    fn observer_never_reads_a_torn_concurrent_publication() {
        const FINAL_SERIAL: u64 = 1_000;
        let mut meter = ProgramOutputMeter::new(AudioChannelLayout::Stereo);
        let observer = meter.observer();
        let writer = std::thread::spawn(move || {
            for serial in 1..=FINAL_SERIAL {
                let value = serial as f32;
                assert!(meter.observe(serial as i64, 1, &[value, -value]));
            }
        });

        loop {
            let frame = observer.latest();
            if frame.block_serial > 0 {
                let expected = frame.block_serial as f32;
                assert_eq!(frame.start_sample, frame.block_serial as i64);
                assert_eq!(frame.frames, 1);
                assert_eq!(frame.channels[0].sample_peak_linear, expected);
                assert_eq!(frame.channels[1].sample_peak_linear, expected);
                assert_eq!(frame.channels[0].rms_linear, f64::from(expected));
                assert_eq!(frame.channels[1].rms_linear, f64::from(expected));
            }
            if frame.block_serial == FINAL_SERIAL {
                break;
            }
        }
        writer.join().expect("meter writer");
    }
}
