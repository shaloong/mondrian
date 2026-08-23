//! Prepared allocation-free execution of canonical channel-mix matrices.

use crate::{AudioExecutionError, AudioKernelBackend};
use mondrian_core::{AudioChannelLayout, AudioChannelMixMatrix};

/// Immutable execution form of one validated semantic channel matrix.
///
/// The sparse coefficient order is preserved exactly. Identity matrices use a
/// contiguous-copy fast path; all other matrices accumulate in canonical
/// destination-major/source-major order so scalar and optimized execution are
/// sample-identical.
#[derive(Debug, Clone)]
pub struct PreparedAudioChannelMixer {
    matrix: AudioChannelMixMatrix,
    entries: Vec<PreparedMixEntry>,
    identity: bool,
}

#[derive(Debug, Clone, Copy)]
struct PreparedMixEntry {
    source_channel: usize,
    destination_channel: usize,
    gain: f32,
}

impl PreparedAudioChannelMixer {
    /// Lower one already-validated canonical matrix into hot-loop indices.
    pub fn new(matrix: AudioChannelMixMatrix) -> Self {
        let identity = matrix.is_identity();
        let entries = matrix
            .entries()
            .iter()
            .map(|entry| PreparedMixEntry {
                source_channel: usize::from(entry.source_channel()),
                destination_channel: usize::from(entry.destination_channel()),
                gain: entry.gain().get() as f32,
            })
            .collect();
        Self { matrix, entries, identity }
    }

    /// Exact source layout accepted by this mixer.
    pub const fn source_layout(&self) -> AudioChannelLayout {
        self.matrix.source_layout()
    }

    /// Exact destination layout produced by this mixer.
    pub const fn destination_layout(&self) -> AudioChannelLayout {
        self.matrix.destination_layout()
    }

    /// Number of non-zero coefficients in the prepared sparse matrix.
    pub fn coefficient_count(&self) -> usize {
        self.entries.len()
    }

    /// Transform exact interleaved frames without allocation or hidden gain policy.
    pub fn mix_into(
        &self,
        backend: AudioKernelBackend,
        frames: usize,
        source: &[f32],
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let source_channels = self.source_layout().channel_count();
        let destination_channels = self.destination_layout().channel_count();
        let source_samples =
            frames.checked_mul(source_channels).ok_or(AudioExecutionError::BufferTooLarge)?;
        let destination_samples = frames
            .checked_mul(destination_channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if source.len() != source_samples {
            return Err(AudioExecutionError::ChannelMixSourceSizeMismatch);
        }
        if destination.len() != destination_samples {
            return Err(AudioExecutionError::ChannelMixDestinationSizeMismatch);
        }
        if self.identity {
            destination.copy_from_slice(source);
            return Ok(());
        }
        destination.fill(0.0);
        match backend {
            AudioKernelBackend::ScalarReference | AudioKernelBackend::RuntimeVectorized => {
                mix_sparse_scalar(
                    frames,
                    source_channels,
                    destination_channels,
                    &self.entries,
                    source,
                    destination,
                );
            }
        }
        Ok(())
    }
}

fn mix_sparse_scalar(
    frames: usize,
    source_channels: usize,
    destination_channels: usize,
    entries: &[PreparedMixEntry],
    source: &[f32],
    destination: &mut [f32],
) {
    for frame in 0..frames {
        let source_base = frame * source_channels;
        let destination_base = frame * destination_channels;
        for entry in entries {
            destination[destination_base + entry.destination_channel] +=
                source[source_base + entry.source_channel] * entry.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_and_runtime_backends_match_for_sparse_downmix() {
        let matrix = AudioChannelMixMatrix::standard(
            AudioChannelLayout::Surround51Side,
            AudioChannelLayout::Stereo,
        )
        .expect("standard matrix");
        let mixer = PreparedAudioChannelMixer::new(matrix);
        let source =
            (0..6 * 67).map(|index| index as f32 * 0.003_906_25 - 0.75).collect::<Vec<_>>();
        let mut scalar = vec![0.0; 2 * 67];
        let mut optimized = vec![0.0; 2 * 67];

        mixer
            .mix_into(
                AudioKernelBackend::ScalarReference,
                67,
                &source,
                &mut scalar,
            )
            .expect("scalar mix");
        mixer
            .mix_into(
                AudioKernelBackend::RuntimeVectorized,
                67,
                &source,
                &mut optimized,
            )
            .expect("optimized mix");

        assert_eq!(optimized, scalar);
    }

    #[test]
    fn identity_and_silence_matrices_are_exact() {
        let identity = PreparedAudioChannelMixer::new(AudioChannelMixMatrix::identity(
            AudioChannelLayout::Stereo,
        ));
        let source = [0.25_f32, -0.5, 0.75, -1.0];
        let mut output = [0.0_f32; 4];
        identity
            .mix_into(
                AudioKernelBackend::RuntimeVectorized,
                2,
                &source,
                &mut output,
            )
            .expect("identity mix");
        assert_eq!(output, source);

        let silence = PreparedAudioChannelMixer::new(
            AudioChannelMixMatrix::new(AudioChannelLayout::Mono, AudioChannelLayout::Stereo, [])
                .expect("silence matrix"),
        );
        let mut output = [1.0_f32; 4];
        silence
            .mix_into(
                AudioKernelBackend::ScalarReference,
                2,
                &[0.5, -0.5],
                &mut output,
            )
            .expect("silence mix");
        assert_eq!(output, [0.0; 4]);
    }
}
