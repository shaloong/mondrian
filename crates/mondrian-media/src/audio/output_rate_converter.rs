use rubato::{
    calculate_cutoff, Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

const CONVERTER_CHUNK_FRAMES: usize = 64;
const CONVERTER_SINC_LENGTH: usize = 128;

pub(super) struct OutputSampleRateConverter {
    channels: usize,
    resampler: SincFixedIn<f32>,
    pending_interleaved: Vec<f32>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    trim_remaining: usize,
}

impl OutputSampleRateConverter {
    pub(super) fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self, String> {
        if input_rate == 0 || output_rate == 0 || channels == 0 {
            return Err(
                "output sample-rate conversion requires positive rates and channels".to_owned(),
            );
        }
        let window = WindowFunction::BlackmanHarris2;
        let parameters = SincInterpolationParameters {
            sinc_len: CONVERTER_SINC_LENGTH,
            f_cutoff: calculate_cutoff(CONVERTER_SINC_LENGTH, window),
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 256,
            window,
        };
        let resampler = SincFixedIn::<f32>::new(
            f64::from(output_rate) / f64::from(input_rate),
            1.0,
            parameters,
            CONVERTER_CHUNK_FRAMES,
            channels,
        )
        .map_err(|error| format!("failed to construct output sample-rate converter: {error}"))?;
        let input = resampler.input_buffer_allocate(true);
        let output = resampler.output_buffer_allocate(true);
        let trim_remaining = resampler.output_delay();
        Ok(Self {
            channels,
            resampler,
            pending_interleaved: Vec::with_capacity(CONVERTER_CHUNK_FRAMES * channels),
            input,
            output,
            trim_remaining,
        })
    }

    pub(super) fn maximum_output_samples(
        &self,
        additional_samples: usize,
    ) -> Result<usize, String> {
        let total_samples = self
            .pending_interleaved
            .len()
            .checked_add(additional_samples)
            .ok_or_else(|| "output sample-rate conversion extent overflowed".to_owned())?;
        let total_frames = total_samples / self.channels;
        let blocks = total_frames / CONVERTER_CHUNK_FRAMES;
        blocks
            .checked_mul(self.resampler.output_frames_max())
            .and_then(|frames| frames.checked_mul(self.channels))
            .ok_or_else(|| "output sample-rate conversion capacity proof overflowed".to_owned())
    }

    pub(super) fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>, String> {
        let mut combined = std::mem::take(&mut self.pending_interleaved);
        combined.extend_from_slice(samples);
        let block_samples = CONVERTER_CHUNK_FRAMES * self.channels;
        let complete_samples = combined.len() / block_samples * block_samples;
        let mut converted = Vec::with_capacity(
            complete_samples / block_samples * self.resampler.output_frames_max() * self.channels,
        );
        for block in combined[..complete_samples].chunks_exact(block_samples) {
            for channel in 0..self.channels {
                for frame in 0..CONVERTER_CHUNK_FRAMES {
                    self.input[channel][frame] = block[frame * self.channels + channel];
                }
            }
            let (_, output_frames) = self
                .resampler
                .process_into_buffer(&self.input, &mut self.output, None)
                .map_err(|error| format!("output sample-rate conversion failed: {error}"))?;
            let skip = self.trim_remaining.min(output_frames);
            self.trim_remaining -= skip;
            for frame in skip..output_frames {
                for channel in 0..self.channels {
                    converted.push(self.output[channel][frame]);
                }
            }
        }
        self.pending_interleaved.extend_from_slice(&combined[complete_samples..]);
        Ok(converted)
    }

    pub(super) fn reset(&mut self) {
        self.resampler.reset();
        self.trim_remaining = self.resampler.output_delay();
        self.pending_interleaved.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert_in_chunks(chunk_frames: &[usize]) -> Vec<f32> {
        let total_frames: usize = chunk_frames.iter().sum();
        let mut source = Vec::with_capacity(total_frames * 2);
        for frame in 0..total_frames {
            let left = if frame < 48_000 {
                (2.0 * std::f32::consts::PI * 1_000.0 * frame as f32 / 48_000.0).sin()
            } else {
                0.0
            };
            source.extend_from_slice(&[left, 0.0]);
        }
        let mut converter =
            OutputSampleRateConverter::new(48_000, 44_100, 2).expect("construct converter");
        let mut output = Vec::new();
        let mut offset = 0;
        for frames in chunk_frames {
            let samples = frames * 2;
            output.extend(
                converter.process(&source[offset..offset + samples]).expect("convert chunk"),
            );
            offset += samples;
        }
        output
    }

    #[test]
    fn arbitrary_enqueue_partition_preserves_one_continuous_conversion() {
        let contiguous = convert_in_chunks(&[48_256]);
        let partitioned = convert_in_chunks(&[1, 63, 937, 2_999, 3_840, 17, 40_399]);
        assert_eq!(partitioned, contiguous);
    }

    #[test]
    fn downsampled_tone_retains_level_frequency_and_channel_isolation() {
        let output = convert_in_chunks(&[3_840; 12].into_iter().chain([2_176]).collect::<Vec<_>>());
        let frames = output.len() / 2;
        assert!((44_000..=44_400).contains(&frames), "frames={frames}");
        let analysis = output
            .chunks_exact(2)
            .skip(1_000)
            .take(40_000)
            .map(|frame| frame[0])
            .collect::<Vec<_>>();
        let rms = (analysis.iter().map(|sample| sample * sample).sum::<f32>()
            / analysis.len() as f32)
            .sqrt();
        assert!((0.69..=0.72).contains(&rms), "rms={rms}");
        let positive_crossings =
            analysis.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count();
        let frequency = positive_crossings as f64 * 44_100.0 / analysis.len() as f64;
        assert!(
            (998.0..=1_002.0).contains(&frequency),
            "frequency={frequency}"
        );
        assert!(
            output.chunks_exact(2).all(|frame| frame[1].abs() <= 1.0e-7),
            "silent right channel received crosstalk"
        );
    }

    #[test]
    fn reset_restarts_filter_history_and_delay_trimming() {
        let mut converter =
            OutputSampleRateConverter::new(48_000, 44_100, 2).expect("construct converter");
        let source = vec![0.25; 3_840 * 2];
        let first = converter.process(&source).expect("first conversion");
        converter.reset();
        let second = converter.process(&source).expect("second conversion");
        assert_eq!(second, first);
    }
}
