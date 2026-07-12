use super::*;

pub(super) struct TimelineAudioPcmRenderer {
    sequence: Sequence,
    library: Arc<AssetLibrary>,
    source_cache: Arc<AudioSourceCache>,
    sample_rate: u32,
    channels: u8,
}

impl TimelineAudioPcmRenderer {
    pub(super) fn new(
        sequence: Sequence,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
        sample_rate: u32,
        channels: u8,
    ) -> Self {
        Self {
            sequence,
            library,
            source_cache,
            sample_rate,
            channels,
        }
    }
}

impl AudioPcmRenderer for TimelineAudioPcmRenderer {
    fn render(&self, request: AudioPcmRenderRequest) -> mondrian_core::Result<AudioBuffer> {
        if request.sample_rate != self.sample_rate || request.channels != self.channels {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "timeline_audio_render_contract".to_string(),
                reason: format!(
                    "requested {} Hz/{} ch but Adapter is configured for {} Hz/{} ch",
                    request.sample_rate, request.channels, self.sample_rate, self.channels
                ),
            });
        }
        render_audio_chunk_with_cache(
            &self.sequence,
            self.library.as_ref(),
            self.source_cache.as_ref(),
            self.sample_rate,
            self.channels,
            request.start_sample,
            request.frame_count,
        )
    }
}

pub(super) fn render_audio_chunk_with_cache(
    seq: &Sequence,
    library: &AssetLibrary,
    audio_source_cache: &AudioSourceCache,
    sample_rate: u32,
    channels: u8,
    window_start_sample: i64,
    chunk_frames: usize,
) -> mondrian_core::Result<AudioBuffer> {
    let chunk_frames = chunk_frames.max(1);
    let sample_rate = AudioSampleRate::new(sample_rate).map_err(|error| {
        mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "timeline_audio_sample_rate".to_string(),
            reason: error.to_string(),
        }
    })?;
    let chunk_frames_i64 = i64::try_from(chunk_frames).map_err(|_| audio_sample_range_error())?;
    let window_end_sample = window_start_sample
        .checked_add(chunk_frames_i64)
        .ok_or_else(audio_sample_range_error)?;

    let has_solo = seq.audio_tracks.iter().any(|t| t.is_solo && !t.is_muted);
    let mut tracks = Vec::new();

    for track in &seq.audio_tracks {
        if track.is_muted || (has_solo && !track.is_solo) {
            continue;
        }

        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }

            let clip_start_sample = AudioSamplePosition::from_timeline_time(
                clip.position,
                sample_rate,
                AudioSampleRounding::Nearest,
            )
            .map_err(audio_time_error)?
            .sample();
            let clip_end_sample = AudioSamplePosition::from_timeline_time(
                clip.end_position()?,
                sample_rate,
                AudioSampleRounding::Nearest,
            )
            .map_err(audio_time_error)?
            .sample();
            let overlap_start = window_start_sample.max(clip_start_sample);
            let overlap_end = window_end_sample.min(clip_end_sample);
            if overlap_end <= overlap_start {
                continue;
            }

            let Some(asset) = library.get_asset(clip.asset_id)? else {
                continue;
            };

            let source = match audio_source_cache.get_or_decode(asset.path.as_path()) {
                Ok(decoded) => decoded,
                Err(err) => {
                    tracing::debug!("音频解码失败，已跳过素材 {}: {}", asset.id, err);
                    continue;
                }
            };

            let overlap_time = TimelineTime::new(overlap_start, i64::from(sample_rate.hz()))?;
            let source_time = clip.timeline_to_source_time(overlap_time)?;
            let source_start_frame = AudioSamplePosition::from_timeline_time(
                source_time,
                sample_rate,
                AudioSampleRounding::Floor,
            )
            .map_err(audio_time_error)?
            .sample();
            let source_start_frame = usize::try_from(source_start_frame.max(0))
                .map_err(|_| audio_sample_range_error())?;
            let segment_frames = usize::try_from(overlap_end - overlap_start)
                .map_err(|_| audio_sample_range_error())?;
            let segment = source.slice_frames(source_start_frame, segment_frames.max(1));
            if segment.samples.is_empty() {
                continue;
            }

            let place_offset = usize::try_from(overlap_start - window_start_sample)
                .map_err(|_| audio_sample_range_error())?;
            let mut placed = AudioBuffer::silent(sample_rate.hz(), channels, chunk_frames);
            let max_place_frames = chunk_frames.saturating_sub(place_offset);
            let copy_frames = segment.frame_count().min(max_place_frames);

            let dst_channels = channels as usize;
            let src_channels = segment.channels as usize;
            for frame in 0..copy_frames {
                let dst_base = (place_offset + frame) * dst_channels;
                let src_base = frame * src_channels;
                for ch in 0..dst_channels {
                    let v = segment
                        .samples
                        .get(src_base + ch.min(src_channels.saturating_sub(1)))
                        .copied()
                        .unwrap_or(0.0);
                    placed.samples[dst_base + ch] = v;
                }
            }

            tracks.push(AudioTrackData {
                buffer: placed,
                config: AudioTrackConfig {
                    volume: 1.0,
                    pan: 0.0,
                    is_muted: false,
                    is_solo: false,
                },
            });
        }
    }

    if tracks.is_empty() {
        return Ok(AudioBuffer::silent(
            sample_rate.hz(),
            channels,
            chunk_frames,
        ));
    }
    let mixer = AudioMixer::new(sample_rate.hz(), channels);
    Ok(mixer.mix(&tracks))
}

fn audio_time_error(error: mondrian_core::AudioTimeError) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "timeline_audio_time_mapping".to_string(),
        reason: error.to_string(),
    }
}

fn audio_sample_range_error() -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "timeline_audio_sample_range".to_string(),
        reason: "audio sample window is outside the supported platform range".to_string(),
    }
}
