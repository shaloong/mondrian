//! Freeze and analyze the exact media requests of a Clip.

use std::path::Path;

use mondrian_core::{
    mask_data::MaskTrackingSettings, ExecutionCancellationToken, FramePosition, FrameRounding,
    MediaFileFingerprint, Rational, SourceSampleTarget, TimelineTime,
};
use mondrian_effects::{
    stabilization::{
        StabilizationAnalysis, StabilizationAnalyzer, StabilizationError, StabilizationRecord,
        StabilizationSource,
    },
    TrackingFrame,
};
use mondrian_media::{
    PreviewDecodeAccessMode, PreviewDecodeOutcome, PreviewDecodeRequest,
    PreviewDecodeSessionContext, PreviewSourceColorContract,
};
use mondrian_timeline::Clip;

/// Failure to freeze or decode an exact set of camera observations for a Clip.
#[derive(Debug, thiserror::Error)]
pub enum ClipStabilizationError {
    /// A Clip or exact Sequence clock rejected the source request.
    #[error(transparent)]
    Clip(#[from] mondrian_core::MondrianError),
    /// Invalid exact frame grid or a time arithmetic failure.
    #[error(transparent)]
    Time(#[from] mondrian_core::TimelineTimeError),
    /// Source or observation evidence is invalid or stale.
    #[error(transparent)]
    Analysis(#[from] StabilizationError),
    /// The cost of the frozen request exceeded its owner's admission limit.
    #[error("stabilization request exceeds the admitted {0} frames")]
    FrameLimit(usize),
    /// The CPU decoder did not return an analyzable image.
    #[error("stabilization could not decode frame at Clip time {time}: {reason}")]
    Decode {
        /// Exact author time associated with this decoding attempt.
        time: TimelineTime,
        /// Decoder or input adaptation detail.
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRequest {
    time: TimelineTime,
    source: SourceSampleTarget,
}

/// Immutable capture suitable for dispatch to a media analysis worker.
#[derive(Debug)]
pub struct ClipStabilizationCapture {
    source: StabilizationSource,
    settings: MaskTrackingSettings,
    frames: Vec<FrameRequest>,
    end: TimelineTime,
}

impl ClipStabilizationCapture {
    /// Freeze half-open Clip membership and exact source requests on the Sequence frame grid.
    ///
    /// Also capture the exact Clip in-point when it falls between Sequence samples. This
    /// keeps the first fraction of the Clip covered during subframe preview or export.
    pub fn new(
        clip: &Clip,
        source: StabilizationSource,
        frame_rate: Rational,
        settings: MaskTrackingSettings,
        frame_limit: usize,
    ) -> Result<Self, ClipStabilizationError> {
        if clip.media_asset_id() != Some(source.asset) || !source.media.authorizes_reuse() {
            return Err(StabilizationError::StaleSource.into());
        }
        settings.validate()?;
        let end_position = clip.end_position()?;
        if end_position <= clip.position {
            return Err(StabilizationError::InvalidTimeRange.into());
        }
        let first = clip.position.to_frame_position(frame_rate, FrameRounding::Ceil)?.frame;
        let last = end_position.to_frame_position(frame_rate, FrameRounding::Ceil)?.frame;
        let time_base = Rational::new(frame_rate.den, frame_rate.num);
        let frame_time =
            |frame| TimelineTime::from_frame_position(FramePosition::new(frame, time_base));
        let prepend = frame_time(first)? != clip.position;
        let count = (i128::from(last) - i128::from(first)) + i128::from(prepend);
        let count = usize::try_from(count)
            .ok()
            .filter(|count| *count <= frame_limit)
            .ok_or(ClipStabilizationError::FrameLimit(frame_limit))?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(count)
            .map_err(|_| ClipStabilizationError::FrameLimit(frame_limit))?;
        let mut add = |position| -> Result<(), ClipStabilizationError> {
            frames.push(FrameRequest {
                time: clip.timeline_to_clip_time(position)?,
                source: stabilization_source_sample(clip, position)?,
            });
            Ok(())
        };
        if prepend {
            add(clip.position)?;
        }
        for frame in first..last {
            add(frame_time(frame)?)?;
        }
        Ok(Self {
            source,
            settings,
            frames,
            end: clip.clip_time_out()?,
        })
    }

    /// Number of image samples in this frozen capture, including its reference image.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the capture has no sample requests.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Decode and track the frozen inputs through the application's FFmpeg adapter.
    pub fn analyze_media(
        &self,
        path: &Path,
        color: PreviewSourceColorContract,
        decoder: &mut PreviewDecodeSessionContext,
        cancellation: &ExecutionCancellationToken,
        progress: impl FnMut(usize, usize),
    ) -> Result<StabilizationRecord, ClipStabilizationError> {
        if cancellation.is_canceled() {
            return Err(StabilizationError::Canceled.into());
        }
        if MediaFileFingerprint::capture(path) != self.source.media {
            return Err(StabilizationError::StaleSource.into());
        }
        let record = self.analyze(
            |time, source| {
                let request = PreviewDecodeRequest::new(
                    path,
                    source,
                    PreviewDecodeAccessMode::RandomAccessStillFrame,
                    color,
                )
                .with_video_stream_index(self.source.video_stream)
                .with_max_size(
                    Some(self.settings.analysis_max_dimension),
                    Some(self.settings.analysis_max_dimension),
                )
                .with_fingerprint(self.source.media);
                let fail = |reason| ClipStabilizationError::Decode { time, reason };
                let decode_cancel = cancellation.clone();
                let frame = match decoder
                    .decode_cancellable(request, move || decode_cancel.is_canceled())
                    .map_err(|error| fail(error.to_string()))?
                {
                    PreviewDecodeOutcome::Frame(frame) => {
                        TrackingFrame::from_covered_rgba8(frame.width, frame.height, frame.rgba())
                    }
                    PreviewDecodeOutcome::FloatFrame(frame) => {
                        TrackingFrame::from_covered_rgba_f32(
                            frame.width,
                            frame.height,
                            frame.rgba(),
                        )
                    }
                    PreviewDecodeOutcome::Unorm16Frame(frame) => {
                        let (width, height) = (frame.width, frame.height);
                        TrackingFrame::from_covered_rgba_f32(
                            width,
                            height,
                            &frame.into_normalized_f32(),
                        )
                    }
                    PreviewDecodeOutcome::Canceled(_) => {
                        return Err(StabilizationError::Canceled.into())
                    }
                    PreviewDecodeOutcome::CpuYuvFrame(_)
                    | PreviewDecodeOutcome::NativeGpuFrame(_) => {
                        return Err(fail("expected a CPU RGBA analysis frame".to_owned()));
                    }
                };
                frame.map_err(|error| fail(error.to_string()))
            },
            cancellation,
            progress,
        )?;
        if MediaFileFingerprint::capture(path) != self.source.media {
            return Err(StabilizationError::StaleSource.into());
        }
        Ok(record)
    }

    fn analyze(
        &self,
        mut decode: impl FnMut(
            TimelineTime,
            SourceSampleTarget,
        ) -> Result<TrackingFrame, ClipStabilizationError>,
        cancellation: &ExecutionCancellationToken,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<StabilizationRecord, ClipStabilizationError> {
        let mut analyzer = StabilizationAnalyzer::new(self.settings)?;
        for (index, frame) in self.frames.iter().enumerate() {
            if cancellation.is_canceled() {
                return Err(StabilizationError::Canceled.into());
            }
            let image = decode(frame.time, frame.source)?;
            analyzer.push_frame(frame.time, image, false, || cancellation.is_canceled())?;
            progress(index + 1, self.frames.len());
        }
        if cancellation.is_canceled() {
            return Err(StabilizationError::Canceled.into());
        }
        Ok(StabilizationRecord::new(
            self.source,
            self.settings,
            self.frames.iter().map(|frame| frame.source).collect(),
            analyzer.finish(self.end)?,
        )?)
    }
}

/// Verify a frozen analysis against the current Clip and resolved media revision before use.
pub fn stabilization_analysis_for_clip<'a>(
    record: &'a StabilizationRecord,
    clip: &Clip,
    source: StabilizationSource,
    settings: MaskTrackingSettings,
) -> Result<&'a StabilizationAnalysis, ClipStabilizationError> {
    if clip.media_asset_id() != Some(source.asset) {
        return Err(StabilizationError::StaleSource.into());
    }
    let analysis = record.analysis_for_source(source, settings, |time| {
        clip.clip_to_timeline_time(time)
            .and_then(|position| stabilization_source_sample(clip, position))
            .ok()
    })?;
    let (start, end) = analysis.analysis_interval();
    if clip.clip_time_in < start || clip.clip_time_out()? > end {
        return Err(StabilizationError::OutsideAnalysis.into());
    }
    Ok(analysis)
}

fn stabilization_source_sample(
    clip: &Clip,
    position: TimelineTime,
) -> mondrian_core::Result<SourceSampleTarget> {
    let target = clip.timeline_to_source_sample(position)?;
    match clip.media_interpretation() {
        Some(interpretation) => Ok(interpretation.resolve_source_sample(target)?),
        None => Ok(target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        AssetId, ColorSpace, MediaFileChangeStamp, MediaFileObjectIdentity, TimeScale,
    };
    use mondrian_effects::stabilization::{
        StabilizationFraming, StabilizationLink, StabilizationMotion,
    };
    use mondrian_media::DecodedVideoRangeContract;

    fn time(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).unwrap()
    }

    fn fixture_source() -> StabilizationSource {
        StabilizationSource {
            asset: AssetId::new(),
            video_stream: 0,
            media: MediaFileFingerprint {
                len: Some(514),
                modified_secs: Some(31),
                modified_nanos: Some(3),
                object_identity: Some(MediaFileObjectIdentity::Unix { device: 1, inode: 2 }),
                change_stamp: Some(MediaFileChangeStamp::Unix { seconds: 42, nanoseconds: 0 }),
            },
        }
    }

    #[test]
    fn off_grid_in_point_and_reverse_source_samples_are_frozen_exactly() {
        let source = fixture_source();
        let mut clip = Clip::new(source.asset, time(1, 100), time(1, 5)).unwrap();
        clip.set_constant_source_time_map(TimelineTime::ONE, TimeScale::NEGATIVE_ONE)
            .unwrap();
        let settings = MaskTrackingSettings::default();
        let rate = Rational::new(10, 1);
        assert!(matches!(
            ClipStabilizationCapture::new(&clip, source, rate, settings, 2),
            Err(ClipStabilizationError::FrameLimit(2))
        ));
        let capture = ClipStabilizationCapture::new(&clip, source, rate, settings, 3).unwrap();
        assert_eq!(
            capture.frames.iter().map(|frame| frame.time).collect::<Vec<_>>(),
            [TimelineTime::ZERO, time(9, 100), time(19, 100)]
        );
        assert_eq!(
            capture.frames[0].source,
            SourceSampleTarget::strict_predecessor(TimelineTime::ONE)
        );
        assert_eq!(
            capture.frames[2].source,
            SourceSampleTarget::strict_predecessor(time(81, 100))
        );
        let cancellation = ExecutionCancellationToken::new();
        let mut decoded = Vec::new();
        let canceled = ExecutionCancellationToken::new();
        let discarded = capture.analyze(
            |_, _| Ok(TrackingFrame::new(40, 40, vec![0.3; 1600]).unwrap()),
            &canceled,
            |done, total| {
                if done == total {
                    canceled.cancel();
                }
            },
        );
        assert!(matches!(
            discarded,
            Err(ClipStabilizationError::Analysis(
                StabilizationError::Canceled
            ))
        ));
        let record = capture
            .analyze(
                |_, target| {
                    decoded.push(target);
                    Ok(TrackingFrame::new(40, 40, vec![0.3; 1600]).unwrap())
                },
                &cancellation,
                |_, _| {},
            )
            .unwrap();
        assert_eq!(decoded.len(), 3);
        clip.position = time(9, 1);
        let analysis = stabilization_analysis_for_clip(&record, &clip, source, settings).unwrap();
        assert_eq!(
            analysis.analysis_interval(),
            (TimelineTime::ZERO, time(1, 5))
        );
        clip.media_interpretation_mut().unwrap().frame_rate_override = Some(Rational::FPS_30);
        assert!(matches!(
            stabilization_analysis_for_clip(&record, &clip, source, settings),
            Err(ClipStabilizationError::Analysis(
                StabilizationError::StaleSource
            ))
        ));
        let interpreted = ClipStabilizationCapture::new(&clip, source, rate, settings, 3).unwrap();
        assert_ne!(interpreted.frames[0].source, decoded[0]);
        assert_eq!(
            interpreted.frames[0].source,
            clip.media_interpretation().unwrap().resolve_source_sample(decoded[0]).unwrap()
        );
        clip.media_interpretation_mut().unwrap().frame_rate_override = None;
        clip.set_source_origin(time(103, 100)).unwrap();
        assert!(matches!(
            stabilization_analysis_for_clip(&record, &clip, source, settings),
            Err(ClipStabilizationError::Analysis(
                StabilizationError::StaleSource
            ))
        ));
    }

    #[test]
    fn stabilization_capture_decodes_real_media_and_replays_after_serialization() {
        let dir = tempfile::tempdir().unwrap();
        let (width, height) = (160u32, 96u32);
        for index in 0..4 {
            let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
            for y in 0..height {
                for x in 0..width {
                    let x = x.wrapping_sub(index * 3);
                    let hash = x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b);
                    let hash = (hash ^ (hash >> 16)).wrapping_mul(0x7feb_352d);
                    ppm.extend([hash as u8; 3]);
                }
            }
            std::fs::write(dir.path().join(format!("frame{index:02}.ppm")), ppm).unwrap();
        }
        let path = dir.path().join("jitter.mkv");
        let status = mondrian_media::ffmpeg_command().expect("FFmpeg fixture capability")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-framerate", "10", "-i"])
            .arg(dir.path().join("frame%02d.ppm"))
            .args(["-c:v", "ffv1", "-vf", "scale=out_range=tv,format=yuv420p,setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709"])
            .arg(&path).status().unwrap();
        assert!(status.success());
        let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, time(2, 5)).unwrap();
        let source = StabilizationSource {
            asset: clip.media_asset_id().unwrap(),
            media: MediaFileFingerprint::capture(&path),
            video_stream: 0,
        };
        let settings = MaskTrackingSettings { search_radius: 10, ..Default::default() };
        let captured =
            ClipStabilizationCapture::new(&clip, source, Rational::new(10, 1), settings, 4)
                .unwrap();
        let color = PreviewSourceColorContract::new(
            ColorSpace::Rec709,
            DecodedVideoRangeContract::OverrideLimited,
        );
        let mut decoder = PreviewDecodeSessionContext::new();
        let cancellation = ExecutionCancellationToken::new();
        let mut events = Vec::new();
        let record = captured
            .analyze_media(
                &path,
                color,
                &mut decoder,
                &cancellation,
                |completed, total| events.push((completed, total)),
            )
            .unwrap();
        assert_eq!(events, [(1, 4), (2, 4), (3, 4), (4, 4)]);
        let bytes = serde_json::to_vec(&record).unwrap();
        let reopened = serde_json::from_slice(&bytes).unwrap();
        let analysis = stabilization_analysis_for_clip(&reopened, &clip, source, settings).unwrap();
        assert!(analysis.samples()[1..]
            .iter()
            .all(|sample| matches!(sample.link, StabilizationLink::Measured(_))));
        let render_path = analysis
            .prepare_path(
                StabilizationMotion::Lock,
                StabilizationFraming::ExposeEdges,
                1.0,
                || false,
            )
            .unwrap();
        let last = render_path.view_at(time(3, 10)).unwrap();
        let dx = last.source_offset[0] * f64::from(width);
        eprintln!("decoded stabilization path offset at 3/10: {dx:.6} pixels");
        assert!((dx - 9.0).abs() < 0.15);
        assert!((last.source_offset[1] * f64::from(height)).abs() < 0.15);
        drop(decoder);
        std::fs::write(&path, b"different media revision").unwrap();
        let stale = captured.analyze_media(
            &path,
            color,
            &mut PreviewDecodeSessionContext::new(),
            &cancellation,
            |_, _| {},
        );
        assert!(matches!(
            stale,
            Err(ClipStabilizationError::Analysis(
                StabilizationError::StaleSource
            ))
        ));
    }
}
