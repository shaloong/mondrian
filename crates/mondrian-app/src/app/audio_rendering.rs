use super::*;

pub(super) fn render_audio_chunk_with_cache(
    seq: &Sequence,
    library: &AssetLibrary,
    audio_source_cache: &AudioSourceCache,
    sample_rate: u32,
    channels: u8,
    window_start_secs: f64,
    duration_secs: f64,
) -> mondrian_core::Result<AudioBuffer> {
    let chunk_frames = ((duration_secs * sample_rate as f64).round() as usize).max(1);
    let window_end_secs = window_start_secs + duration_secs;

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

            let clip_start_secs = clip.position.to_secs();
            let clip_end_secs = clip.end_position().to_secs();
            let overlap_start = window_start_secs.max(clip_start_secs);
            let overlap_end = window_end_secs.min(clip_end_secs);
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

            let overlap_tc = TimeCode::from_secs(overlap_start, seq.settings.frame_rate);
            let source_start_secs = clip.timeline_to_source_time(overlap_tc).to_secs().max(0.0);
            let source_start_frame = (source_start_secs * sample_rate as f64).floor() as usize;
            let segment_frames =
                ((overlap_end - overlap_start) * sample_rate as f64).ceil() as usize;
            let segment = source.slice_frames(source_start_frame, segment_frames.max(1));
            if segment.samples.is_empty() {
                continue;
            }

            let place_offset = ((overlap_start - window_start_secs) * sample_rate as f64)
                .round()
                .max(0.0) as usize;
            let mut placed = AudioBuffer::silent(sample_rate, channels, chunk_frames);
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

    let mixer = AudioMixer::new(sample_rate, channels);
    Ok(mixer.mix(&tracks))
}
