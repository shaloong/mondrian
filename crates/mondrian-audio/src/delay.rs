//! Preallocated interleaved fixed-delay state used by prepared PDC inputs.

use crate::AudioExecutionError;

#[derive(Debug)]
pub(crate) struct FixedDelayLine {
    samples: Vec<f32>,
    cursor: usize,
}

impl FixedDelayLine {
    pub(crate) fn new(delay_frames: usize, channels: usize) -> Result<Self, AudioExecutionError> {
        let samples =
            delay_frames.checked_mul(channels).ok_or(AudioExecutionError::BufferTooLarge)?;
        Ok(Self { samples: vec![0.0; samples], cursor: 0 })
    }

    pub(crate) const fn sample_capacity(&self) -> usize {
        self.samples.len()
    }

    pub(crate) fn add_interleaved(
        &mut self,
        source: &[f32],
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        if source.len() != destination.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        if self.samples.is_empty() {
            for (destination, source) in destination.iter_mut().zip(source.iter().copied()) {
                *destination += source;
            }
            return Ok(());
        }
        for (destination, source) in destination.iter_mut().zip(source.iter().copied()) {
            let delayed = self.samples[self.cursor];
            self.samples[self.cursor] = source;
            self.cursor += 1;
            if self.cursor == self.samples.len() {
                self.cursor = 0;
            }
            *destination += delayed;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_delay_preserves_channels_across_block_partitions() {
        let mut whole = FixedDelayLine::new(2, 2).expect("delay");
        let mut whole_output = vec![0.0; 8];
        whole
            .add_interleaved(
                &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0],
                &mut whole_output,
            )
            .expect("whole");

        let mut split = FixedDelayLine::new(2, 2).expect("delay");
        let mut first = vec![0.0; 4];
        let mut second = vec![0.0; 4];
        split.add_interleaved(&[1.0, 10.0, 2.0, 20.0], &mut first).expect("first");
        split.add_interleaved(&[3.0, 30.0, 4.0, 40.0], &mut second).expect("second");

        assert_eq!(whole_output, [0.0, 0.0, 0.0, 0.0, 1.0, 10.0, 2.0, 20.0]);
        assert_eq!([first, second].concat(), whole_output);
    }
}
