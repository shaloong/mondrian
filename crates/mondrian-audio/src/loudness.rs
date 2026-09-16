//! BS.1770/EBU R128 program loudness and four-times true-peak observation.
//!
//! This analyzer is an observation Adapter for control, export, and analysis
//! threads. It is intentionally separate from the allocation-free realtime
//! sample-peak meter and never changes mixer samples.

use mondrian_core::{AudioChannelLayout, AudioChannelPosition};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::f64::consts::PI;

const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = 10.0;
const LOUDNESS_OFFSET: f64 = -0.691;
const STEP_HZ: usize = 10;
const MOMENTARY_STEPS: usize = 4;
const SHORT_TERM_STEPS: usize = 30;
const TRUE_PEAK_FACTOR: usize = 4;
const TRUE_PEAK_HALF_TAPS: isize = 12;

/// Final standards-oriented Program observation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioLoudnessReport {
    /// EBU R128 integrated loudness after absolute and relative gating.
    pub integrated_lufs: Option<f64>,
    /// Latest 400 ms ungated momentary loudness.
    pub momentary_lufs: Option<f64>,
    /// Latest 3 s ungated short-term loudness.
    pub short_term_lufs: Option<f64>,
    /// Maximum four-times oversampled reconstructed peak.
    pub true_peak_linear: f64,
    /// `20 log10(true_peak_linear)` in dBTP; absent for digital silence.
    pub true_peak_dbtp: Option<f64>,
    /// Complete 400 ms blocks evaluated for integrated loudness.
    pub integrated_block_count: u64,
    /// Complete interleaved sample frames observed.
    pub sample_frames: u64,
}

impl AudioLoudnessReport {
    /// Construct exact digital-silence evidence when execution demand proves
    /// that no Program node can contribute signal.
    pub const fn digital_silence(sample_frames: u64) -> Self {
        Self {
            integrated_lufs: None,
            momentary_lufs: None,
            short_term_lufs: None,
            true_peak_linear: 0.0,
            true_peak_dbtp: None,
            integrated_block_count: 0,
            sample_frames,
        }
    }
}

/// Invalid or incomplete loudness observation input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioLoudnessError {
    /// Analysis cadence must form exact 100 ms step boundaries.
    #[error("loudness sample rate {0} does not form exact 100 ms steps")]
    UnsupportedSampleRate(u32),
    /// The layout has no qualified BS.1770 channel-weight mapping.
    #[error("BS.1770 loudness is not qualified for channel layout {0}")]
    UnprovenChannelSemantics(AudioChannelLayout),
    /// The supplied PCM extent was not whole interleaved frames.
    #[error("interleaved PCM length {samples} is not divisible by {channels} channels")]
    InvalidInterleavedExtent { samples: usize, channels: usize },
    /// A non-finite sample cannot produce trustworthy loudness evidence.
    #[error("non-finite audio sample encountered during loudness analysis")]
    NonFiniteSample,
    /// `finish` was already called and the immutable result cannot be extended.
    #[error("loudness analyzer is already finalized")]
    AlreadyFinalized,
}

/// Streaming program analyzer with bounded filter/window state.
pub struct AudioLoudnessAnalyzer {
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    channel_weights: Vec<f64>,
    filters: Vec<KWeightingFilter>,
    step_frames: usize,
    step_frame_cursor: usize,
    step_weighted_energy: f64,
    recent_step_energies: VecDeque<f64>,
    integrated_blocks: Vec<f64>,
    true_peak: TruePeakAnalyzer,
    sample_frames: u64,
    finalized: bool,
}

impl AudioLoudnessAnalyzer {
    /// Prepare one exact sample-rate/layout analysis Session.
    pub fn new(
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
    ) -> Result<Self, AudioLoudnessError> {
        if sample_rate < 8_000 || !(sample_rate as usize).is_multiple_of(STEP_HZ) {
            return Err(AudioLoudnessError::UnsupportedSampleRate(sample_rate));
        }
        let channel_weights = channel_weights(channel_layout)?;
        let channels = channel_layout.channel_count();
        Ok(Self {
            sample_rate,
            channel_layout,
            channel_weights,
            filters: (0..channels).map(|_| KWeightingFilter::new(sample_rate)).collect(),
            step_frames: sample_rate as usize / STEP_HZ,
            step_frame_cursor: 0,
            step_weighted_energy: 0.0,
            recent_step_energies: VecDeque::with_capacity(SHORT_TERM_STEPS),
            integrated_blocks: Vec::new(),
            true_peak: TruePeakAnalyzer::new(channels),
            sample_frames: 0,
            finalized: false,
        })
    }

    /// Observe whole interleaved PCM frames without modifying them.
    pub fn observe_interleaved(&mut self, pcm: &[f32]) -> Result<(), AudioLoudnessError> {
        if self.finalized {
            return Err(AudioLoudnessError::AlreadyFinalized);
        }
        let channels = self.channel_layout.channel_count();
        if !pcm.len().is_multiple_of(channels) {
            return Err(AudioLoudnessError::InvalidInterleavedExtent {
                samples: pcm.len(),
                channels,
            });
        }
        for frame in pcm.chunks_exact(channels) {
            let mut weighted_energy = 0.0;
            for (channel, sample) in frame.iter().copied().enumerate() {
                if !sample.is_finite() {
                    return Err(AudioLoudnessError::NonFiniteSample);
                }
                let filtered = self.filters[channel].process(f64::from(sample));
                weighted_energy += self.channel_weights[channel] * filtered * filtered;
                self.true_peak.push(channel, f64::from(sample));
            }
            self.step_weighted_energy += weighted_energy;
            self.step_frame_cursor += 1;
            self.sample_frames = self.sample_frames.saturating_add(1);
            if self.step_frame_cursor == self.step_frames {
                let mean = self.step_weighted_energy / self.step_frames as f64;
                self.recent_step_energies.push_back(mean);
                if self.recent_step_energies.len() > SHORT_TERM_STEPS {
                    self.recent_step_energies.pop_front();
                }
                if self.recent_step_energies.len() >= MOMENTARY_STEPS {
                    let energy = mean_tail(&self.recent_step_energies, MOMENTARY_STEPS);
                    self.integrated_blocks.push(energy);
                }
                self.step_frame_cursor = 0;
                self.step_weighted_energy = 0.0;
            }
        }
        Ok(())
    }

    /// Finalize FIR tail observation and calculate gated loudness.
    pub fn finish(&mut self) -> Result<AudioLoudnessReport, AudioLoudnessError> {
        if self.finalized {
            return Err(AudioLoudnessError::AlreadyFinalized);
        }
        self.finalized = true;
        self.true_peak.finish();
        let momentary_lufs = (self.recent_step_energies.len() >= MOMENTARY_STEPS)
            .then(|| loudness_from_energy(mean_tail(&self.recent_step_energies, MOMENTARY_STEPS)))
            .flatten();
        let short_term_lufs = (self.recent_step_energies.len() >= SHORT_TERM_STEPS)
            .then(|| loudness_from_energy(mean_tail(&self.recent_step_energies, SHORT_TERM_STEPS)))
            .flatten();
        let integrated_lufs = integrated_loudness(&self.integrated_blocks);
        let true_peak_linear = self.true_peak.maximum();
        Ok(AudioLoudnessReport {
            integrated_lufs,
            momentary_lufs,
            short_term_lufs,
            true_peak_linear,
            true_peak_dbtp: (true_peak_linear > 0.0).then(|| 20.0 * true_peak_linear.log10()),
            integrated_block_count: self.integrated_blocks.len() as u64,
            sample_frames: self.sample_frames,
        })
    }

    /// Exact sample rate bound to this Session.
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Semantic layout whose BS.1770 weights are being applied.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }
}

fn channel_weights(layout: AudioChannelLayout) -> Result<Vec<f64>, AudioLoudnessError> {
    if !matches!(
        layout,
        AudioChannelLayout::Mono
            | AudioChannelLayout::Stereo
            | AudioChannelLayout::Surround51Side
            | AudioChannelLayout::Surround51Back
            | AudioChannelLayout::Surround71
    ) {
        return Err(AudioLoudnessError::UnprovenChannelSemantics(layout));
    }
    (0..layout.channel_count())
        .map(|index| {
            let position = layout
                .channel_position(index)
                .ok_or(AudioLoudnessError::UnprovenChannelSemantics(layout))?;
            Ok(match position {
                AudioChannelPosition::LowFrequencyEffects
                | AudioChannelPosition::LowFrequencyEffects2 => 0.0,
                AudioChannelPosition::BackLeft
                | AudioChannelPosition::BackRight
                | AudioChannelPosition::SideLeft
                | AudioChannelPosition::SideRight => 1.41,
                AudioChannelPosition::Mono
                | AudioChannelPosition::FrontLeft
                | AudioChannelPosition::FrontRight
                | AudioChannelPosition::FrontCenter => 1.0,
                AudioChannelPosition::FrontLeftOfCenter
                | AudioChannelPosition::FrontRightOfCenter
                | AudioChannelPosition::BackCenter
                | AudioChannelPosition::TopCenter
                | AudioChannelPosition::TopFrontLeft
                | AudioChannelPosition::TopFrontCenter
                | AudioChannelPosition::TopFrontRight
                | AudioChannelPosition::TopBackLeft
                | AudioChannelPosition::TopBackCenter
                | AudioChannelPosition::TopBackRight
                | AudioChannelPosition::WideLeft
                | AudioChannelPosition::WideRight
                | AudioChannelPosition::TopSideLeft
                | AudioChannelPosition::TopSideRight => {
                    unreachable!("qualified layouts contain only mono, front, LFE, side, or back")
                }
            })
        })
        .collect()
}

fn mean_tail(values: &VecDeque<f64>, count: usize) -> f64 {
    values.iter().rev().take(count).sum::<f64>() / count as f64
}

fn loudness_from_energy(energy: f64) -> Option<f64> {
    (energy > 0.0).then(|| LOUDNESS_OFFSET + 10.0 * energy.log10())
}

fn integrated_loudness(blocks: &[f64]) -> Option<f64> {
    let absolute = blocks
        .iter()
        .copied()
        .filter(|energy| {
            loudness_from_energy(*energy).is_some_and(|lufs| lufs >= ABSOLUTE_GATE_LUFS)
        })
        .collect::<Vec<_>>();
    if absolute.is_empty() {
        return None;
    }
    let absolute_mean = absolute.iter().sum::<f64>() / absolute.len() as f64;
    let relative_gate = loudness_from_energy(absolute_mean)? - RELATIVE_GATE_LU;
    let relative = absolute
        .into_iter()
        .filter(|energy| loudness_from_energy(*energy).is_some_and(|lufs| lufs >= relative_gate))
        .collect::<Vec<_>>();
    (!relative.is_empty())
        .then(|| loudness_from_energy(relative.iter().sum::<f64>() / relative.len() as f64))
        .flatten()
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

struct KWeightingFilter {
    shelf: Biquad,
    high_pass: Biquad,
}

impl KWeightingFilter {
    fn new(sample_rate: u32) -> Self {
        Self {
            shelf: high_shelf(
                sample_rate as f64,
                1_681.974_450_955_533,
                3.999_843_853_973_347,
                0.707_175_236_955_419_6,
            ),
            high_pass: high_pass(
                sample_rate as f64,
                38.135_470_876_024_44,
                0.500_327_037_323_877_3,
            ),
        }
    }

    fn process(&mut self, sample: f64) -> f64 {
        self.high_pass.process(self.shelf.process(sample))
    }
}

fn high_shelf(sample_rate: f64, frequency: f64, gain_db: f64, q: f64) -> Biquad {
    let k = (PI * frequency / sample_rate).tan();
    let vh = 10.0_f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let denominator = 1.0 + k / q + k * k;
    Biquad {
        b0: (vh + vb * k / q + k * k) / denominator,
        b1: 2.0 * (k * k - vh) / denominator,
        b2: (vh - vb * k / q + k * k) / denominator,
        a1: 2.0 * (k * k - 1.0) / denominator,
        a2: (1.0 - k / q + k * k) / denominator,
        z1: 0.0,
        z2: 0.0,
    }
}

fn high_pass(sample_rate: f64, frequency: f64, q: f64) -> Biquad {
    let k = (PI * frequency / sample_rate).tan();
    let denominator = 1.0 + k / q + k * k;
    Biquad {
        // BS.1770's RLB stage retains the [1, -2, 1] numerator rather
        // than normalizing it with the recursive denominator.
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / denominator,
        a2: (1.0 - k / q + k * k) / denominator,
        z1: 0.0,
        z2: 0.0,
    }
}

struct TruePeakAnalyzer {
    histories: Vec<VecDeque<f64>>,
    coefficients: [Vec<f64>; TRUE_PEAK_FACTOR],
    maximum: f64,
}

impl TruePeakAnalyzer {
    fn new(channels: usize) -> Self {
        let histories = (0..channels)
            .map(|_| {
                let mut history = VecDeque::with_capacity((TRUE_PEAK_HALF_TAPS * 2) as usize);
                history.extend(std::iter::repeat_n(0.0, (TRUE_PEAK_HALF_TAPS - 1) as usize));
                history
            })
            .collect();
        Self {
            histories,
            coefficients: std::array::from_fn(true_peak_coefficients),
            maximum: 0.0,
        }
    }

    fn push(&mut self, channel: usize, sample: f64) {
        let history = &mut self.histories[channel];
        history.push_back(sample);
        let taps = (TRUE_PEAK_HALF_TAPS * 2) as usize;
        if history.len() > taps {
            history.pop_front();
        }
        if history.len() == taps {
            for coefficients in &self.coefficients {
                let reconstructed = history
                    .iter()
                    .zip(coefficients)
                    .map(|(sample, coefficient)| sample * coefficient)
                    .sum::<f64>();
                self.maximum = self.maximum.max(reconstructed.abs());
            }
        }
    }

    fn finish(&mut self) {
        for _ in 0..TRUE_PEAK_HALF_TAPS {
            for channel in 0..self.histories.len() {
                self.push(channel, 0.0);
            }
        }
    }

    const fn maximum(&self) -> f64 {
        self.maximum
    }
}

fn true_peak_coefficients(phase: usize) -> Vec<f64> {
    let fractional = phase as f64 / TRUE_PEAK_FACTOR as f64;
    let taps = TRUE_PEAK_HALF_TAPS * 2;
    let mut coefficients = (-TRUE_PEAK_HALF_TAPS + 1..=TRUE_PEAK_HALF_TAPS)
        .map(|offset| {
            let x = offset as f64 - fractional;
            let sinc = if x.abs() < f64::EPSILON {
                1.0
            } else {
                (PI * x).sin() / (PI * x)
            };
            let index = offset + TRUE_PEAK_HALF_TAPS - 1;
            let window = 0.42 - 0.5 * (2.0 * PI * index as f64 / (taps - 1) as f64).cos()
                + 0.08 * (4.0 * PI * index as f64 / (taps - 1) as f64).cos();
            sinc * window
        })
        .collect::<Vec<_>>();
    let normalization = coefficients.iter().sum::<f64>();
    for coefficient in &mut coefficients {
        *coefficient /= normalization;
    }
    coefficients
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_one_kilohertz_reference_has_expected_loudness() {
        let sample_rate = 48_000;
        let mut pcm = Vec::with_capacity(sample_rate as usize * 4 * 2);
        for frame in 0..sample_rate as usize * 4 {
            let sample = 0.1 * (2.0 * PI * 1_000.0 * frame as f64 / sample_rate as f64).sin();
            pcm.extend([sample as f32, sample as f32]);
        }
        let mut analyzer =
            AudioLoudnessAnalyzer::new(sample_rate, AudioChannelLayout::Stereo).expect("analyzer");
        analyzer.observe_interleaved(&pcm).expect("finite PCM");
        let report = analyzer.finish().expect("report");
        let integrated = report.integrated_lufs.expect("audible integrated loudness");
        assert!((-20.05..=-19.95).contains(&integrated), "{report:?}");
        assert!((report.true_peak_linear - 0.1).abs() < 0.002, "{report:?}");
        assert_eq!(report.sample_frames, sample_rate as u64 * 4);
    }

    #[test]
    fn silence_and_lfe_do_not_claim_program_loudness() {
        let sample_rate = 48_000;
        let frames = sample_rate as usize;
        let mut silence =
            AudioLoudnessAnalyzer::new(sample_rate, AudioChannelLayout::Stereo).expect("analyzer");
        silence.observe_interleaved(&vec![0.0; frames * 2]).expect("silence");
        let report = silence.finish().expect("report");
        assert_eq!(report.integrated_lufs, None);
        assert_eq!(report.true_peak_dbtp, None);

        let mut lfe = AudioLoudnessAnalyzer::new(sample_rate, AudioChannelLayout::Surround51Side)
            .expect("analyzer");
        let mut lfe_pcm = vec![0.0; frames * 6];
        for frame in lfe_pcm.chunks_exact_mut(6) {
            frame[3] = 0.5;
        }
        lfe.observe_interleaved(&lfe_pcm).expect("LFE signal");
        let report = lfe.finish().expect("report");
        assert_eq!(report.integrated_lufs, None);
        assert!(report.true_peak_linear >= 0.49);
    }

    #[test]
    fn four_times_reconstruction_detects_an_intersample_peak() {
        let sample_rate = 48_000;
        let pcm = (0..sample_rate)
            .map(|frame| {
                (2.0 * PI * 12_000.0 * frame as f64 / sample_rate as f64 + PI / 4.0).sin() as f32
            })
            .collect::<Vec<_>>();
        let sample_peak = pcm.iter().copied().map(f32::abs).fold(0.0, f32::max);
        let mut analyzer =
            AudioLoudnessAnalyzer::new(sample_rate, AudioChannelLayout::Mono).expect("analyzer");
        analyzer.observe_interleaved(&pcm).expect("finite PCM");
        let report = analyzer.finish().expect("report");
        assert!(sample_peak < 0.71);
        assert!(report.true_peak_linear > 0.98, "{report:?}");
    }

    #[test]
    fn discrete_layout_and_non_finite_samples_fail_closed() {
        let discrete = AudioChannelLayout::discrete(2).expect("discrete");
        assert!(matches!(
            AudioLoudnessAnalyzer::new(48_000, discrete),
            Err(AudioLoudnessError::UnprovenChannelSemantics(_))
        ));
        let mut analyzer =
            AudioLoudnessAnalyzer::new(48_000, AudioChannelLayout::Mono).expect("analyzer");
        assert_eq!(
            analyzer.observe_interleaved(&[f32::NAN]),
            Err(AudioLoudnessError::NonFiniteSample)
        );

        let custom = AudioChannelLayout::speakers([AudioChannelPosition::FrontCenter])
            .expect("custom speaker layout");
        assert!(matches!(
            AudioLoudnessAnalyzer::new(48_000, custom),
            Err(AudioLoudnessError::UnprovenChannelSemantics(_))
        ));
    }

    #[test]
    fn forty_eight_kilohertz_k_weighting_matches_bs1770_coefficients() {
        let shelf = high_shelf(
            48_000.0,
            1_681.974_450_955_533,
            3.999_843_853_973_347,
            0.707_175_236_955_419_6,
        );
        assert!((shelf.b0 - 1.535_124_859_586_97).abs() < 1.0e-12);
        assert!((shelf.b1 + 2.691_696_189_406_38).abs() < 1.0e-12);
        assert!((shelf.b2 - 1.198_392_810_852_85).abs() < 1.0e-12);
        assert!((shelf.a1 + 1.690_659_293_182_41).abs() < 1.0e-12);
        assert!((shelf.a2 - 0.732_480_774_215_85).abs() < 1.0e-12);

        let rlb = high_pass(48_000.0, 38.135_470_876_024_44, 0.500_327_037_323_877_3);
        assert_eq!([rlb.b0, rlb.b1, rlb.b2], [1.0, -2.0, 1.0]);
        assert!((rlb.a1 + 1.990_047_454_833_98).abs() < 1.0e-12);
        assert!((rlb.a2 - 0.990_072_250_366_21).abs() < 1.0e-12);
    }

    #[test]
    fn integrated_loudness_applies_absolute_then_relative_gate() {
        let energy_at = |lufs: f64| 10.0_f64.powf((lufs - LOUDNESS_OFFSET) / 10.0);
        let integrated =
            integrated_loudness(&[energy_at(-20.0), energy_at(-40.0), energy_at(-80.0)])
                .expect("one block survives both gates");
        assert!((integrated + 20.0).abs() < 1.0e-12, "{integrated}");
    }
}
