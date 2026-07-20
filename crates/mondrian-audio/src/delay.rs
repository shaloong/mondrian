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

    /// Delay one block and add it with a target-time constant gain.
    pub(crate) fn add_interleaved_constant(
        &mut self,
        source: &[f32],
        destination: &mut [f32],
        gain: f32,
    ) -> Result<(), AudioExecutionError> {
        if source.len() != destination.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        if self.samples.is_empty() {
            for (destination, source) in destination.iter_mut().zip(source.iter().copied()) {
                *destination += source * gain;
            }
            return Ok(());
        }
        for (destination, source) in destination.iter_mut().zip(source.iter().copied()) {
            let delayed = self.samples[self.cursor];
            self.samples[self.cursor] = source;
            self.advance_cursor();
            *destination += delayed * gain;
        }
        Ok(())
    }

    /// Delay one block and add it with gains aligned to destination sample time.
    pub(crate) fn add_interleaved_with_gains(
        &mut self,
        source: &[f32],
        destination: &mut [f32],
        gains: &[f32],
    ) -> Result<(), AudioExecutionError> {
        if source.len() != destination.len() || source.len() != gains.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        if self.samples.is_empty() {
            for ((destination, source), gain) in
                destination.iter_mut().zip(source.iter().copied()).zip(gains)
            {
                *destination += source * *gain;
            }
            return Ok(());
        }
        for ((destination, source), gain) in
            destination.iter_mut().zip(source.iter().copied()).zip(gains)
        {
            let delayed = self.samples[self.cursor];
            self.samples[self.cursor] = source;
            self.advance_cursor();
            *destination += delayed * *gain;
        }
        Ok(())
    }

    fn advance_cursor(&mut self) {
        self.cursor += 1;
        if self.cursor == self.samples.len() {
            self.cursor = 0;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.samples.fill(0.0);
        self.cursor = 0;
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

    #[test]
    fn delayed_route_gains_follow_destination_time_not_source_history() {
        let mut delay = FixedDelayLine::new(1, 1).expect("delay");
        let mut output = vec![0.0; 3];
        delay
            .add_interleaved_with_gains(&[1.0, 2.0, 3.0], &mut output, &[1.0, 2.0, 3.0])
            .expect("gained delay");

        assert_eq!(output, [0.0, 2.0, 6.0]);
    }
}
