//! Session-owned source-field processing.
//!
//! This Module is the only linked-FFmpeg interpretation of decoded field flags.
//! It turns a stable progressive or interlaced source into canonical progressive
//! full-height frames before scaling, color conversion, or composition. The
//! concrete interlaced Implementation is BWDIF in `send_field` mode so each
//! displayed field keeps its own presentation instant.

use super::{PreviewSourceFieldProcessing, PreviewSourceFieldProcessing::*};
use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, PictureFieldDominance, Result};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Bound, Session-local field processor.
pub(super) struct PreviewFieldProcessingSession {
    requested: PreviewSourceFieldProcessing,
    observed: Option<ObservedScan>,
    graph: Option<BwdifGraph>,
    stream_time_base: ffmpeg::Rational,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedScan {
    Progressive,
    Interlaced(PictureFieldDominance),
}

impl PreviewFieldProcessingSession {
    pub(super) fn new(
        requested: PreviewSourceFieldProcessing,
        stream_time_base: ffmpeg::Rational,
        path: &Path,
    ) -> Self {
        Self {
            requested,
            observed: None,
            graph: None,
            stream_time_base,
            path: path.to_path_buf(),
        }
    }

    /// Reset all temporal filter state after seek, cancellation, or codec flush.
    pub(super) fn reset(&mut self) {
        self.graph = None;
        self.observed = None;
    }

    /// Selection uses half-picture ticks whenever decoded-frame inspection or
    /// field-rate output can be required. Demux and seek remain in the source
    /// stream time base; this finer domain exists only after decoded frames
    /// enter the field processor.
    pub(super) const fn selection_scale(&self) -> i64 {
        match self.requested {
            Progressive => 1,
            Automatic | MotionAdaptiveFieldRate { .. } => 2,
        }
    }

    pub(super) fn selection_pts(&self, stream_pts: i64) -> Result<i64> {
        stream_pts.checked_mul(self.selection_scale()).ok_or_else(|| {
            field_error(
                &self.path,
                "field-rate selection timestamp exceeds the exact integer range",
            )
        })
    }

    pub(super) fn selection_duration_pts(&self, stream_duration_pts: i64) -> i64 {
        stream_duration_pts.saturating_mul(self.selection_scale())
    }

    /// A fresh temporal filter needs a real preceding picture after seek.
    /// Automatic can omit it only after decoded frames proved progressive.
    pub(super) fn requires_temporal_preroll(&self) -> bool {
        match self.requested {
            Progressive => false,
            Automatic => self.observed != Some(ObservedScan::Progressive),
            MotionAdaptiveFieldRate { .. } => true,
        }
    }

    /// Process one decoded frame and return zero or more canonical progressive
    /// field-time frames. BWDIF is delayed by its temporal neighborhood, so zero
    /// output for an individual input is ordinary.
    pub(super) fn push(
        &mut self,
        pts: i64,
        frame: &ffmpeg::util::frame::video::Video,
    ) -> Result<Vec<ffmpeg::util::frame::video::Video>> {
        let frame_scan = if frame.is_interlaced() {
            ObservedScan::Interlaced(if frame.is_top_first() {
                PictureFieldDominance::TopFirst
            } else {
                PictureFieldDominance::BottomFirst
            })
        } else {
            ObservedScan::Progressive
        };
        let effective = self.admit(frame_scan)?;
        match effective {
            ObservedScan::Progressive => {
                let mut output = clone_decoded_frame(frame, self.selection_pts(pts)?, &self.path)?;
                let scale = self.selection_scale();
                let denominator =
                    self.stream_time_base.denominator().checked_mul(scale as i32).ok_or_else(
                        || {
                            field_error(
                                &self.path,
                                "selection time base exceeds the exact integer range",
                            )
                        },
                    )?;
                // PTS and duration must share one time base. Without this,
                // the last progressive picture loses half of its proven
                // interval under Automatic and a valid tail sample fails.
                // SAFETY: output exclusively owns this retained AVFrame.
                unsafe {
                    let output = &mut *output.as_mut_ptr();
                    output.duration = output.duration.checked_mul(scale).ok_or_else(|| {
                        field_error(
                            &self.path,
                            "selection duration exceeds the exact integer range",
                        )
                    })?;
                    output.time_base = ffmpeg::ffi::AVRational {
                        num: self.stream_time_base.numerator(),
                        den: denominator,
                    };
                }
                Ok(vec![output])
            }
            ObservedScan::Interlaced(dominance) => {
                if self.graph.is_none() {
                    self.graph = Some(BwdifGraph::open(
                        frame,
                        self.stream_time_base,
                        dominance,
                        &self.path,
                    )?);
                }
                self.graph
                    .as_mut()
                    .expect("BWDIF graph was initialized")
                    .push(frame, pts, &self.path)
            }
        }
    }

    /// Flush a delayed BWDIF tail at decoder EOF.
    pub(super) fn flush(&mut self) -> Result<Vec<ffmpeg::util::frame::video::Video>> {
        match self.graph.as_mut() {
            Some(graph) => graph.flush(&self.path),
            None => Ok(Vec::new()),
        }
    }

    fn admit(&mut self, frame_scan: ObservedScan) -> Result<ObservedScan> {
        let effective = match self.requested {
            Automatic => frame_scan,
            Progressive => {
                if frame_scan != ObservedScan::Progressive {
                    return Err(field_error(
                        &self.path,
                        "decoded frame is interlaced but the source contract is progressive",
                    ));
                }
                ObservedScan::Progressive
            }
            MotionAdaptiveFieldRate { dominance } => ObservedScan::Interlaced(dominance),
        };
        if let Some(observed) = self.observed
            && observed != effective
        {
            return Err(field_error(
                &self.path,
                "mixed progressive/interlaced or changing-dominance source is not qualified",
            ));
        }
        self.observed = Some(effective);
        Ok(effective)
    }
}

struct BwdifGraph {
    graph: ffmpeg::filter::Graph,
    input_intervals: VecDeque<(i64, i64)>,
    second_field: bool,
}

impl BwdifGraph {
    fn open(
        frame: &ffmpeg::util::frame::video::Video,
        time_base: ffmpeg::Rational,
        dominance: PictureFieldDominance,
        path: &Path,
    ) -> Result<Self> {
        if frame.height() == 0 || frame.width() == 0 || !frame.height().is_multiple_of(2) {
            return Err(field_error(
                path,
                "BWDIF requires a non-empty even-height source raster",
            ));
        }
        let buffer = ffmpeg::filter::find("buffer")
            .ok_or_else(|| field_error(path, "linked FFmpeg runtime has no buffer filter"))?;
        let bwdif = ffmpeg::filter::find("bwdif")
            .ok_or_else(|| field_error(path, "linked FFmpeg runtime has no bwdif filter"))?;
        let buffersink = ffmpeg::filter::find("buffersink")
            .ok_or_else(|| field_error(path, "linked FFmpeg runtime has no buffersink filter"))?;
        let mut graph = ffmpeg::filter::Graph::new();
        // Filtering executes inside the already-admitted media worker. Codec
        // threads may still be decoding ahead, so an automatic filter pool
        // would spend the same CPU grant a second time. Set this before adding
        // filters: FFmpeg initializes graph threading during filter creation.
        // SAFETY: this fresh, exclusively owned graph has no filter contexts.
        unsafe { (*graph.as_mut_ptr()).nb_threads = 1 };
        let args = format!(
            "video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect=1/1",
            frame.width(),
            frame.height(),
            ffmpeg::ffi::AVPixelFormat::from(frame.format()) as i32,
            time_base.numerator(),
            time_base.denominator(),
        );
        let mut source = graph
            .add(&buffer, "mondrian_field_input", &args)
            .map_err(|error| field_error(path, format!("BWDIF input setup failed: {error}")))?;
        let parity = match dominance {
            PictureFieldDominance::TopFirst => "tff",
            PictureFieldDominance::BottomFirst => "bff",
        };
        let mut processor = graph
            .add(
                &bwdif,
                "mondrian_bwdif",
                &format!("mode=send_field:parity={parity}:deint=all"),
            )
            .map_err(|error| field_error(path, format!("BWDIF setup failed: {error}")))?;
        let mut sink = graph
            .add(&buffersink, "mondrian_field_output", "")
            .map_err(|error| field_error(path, format!("BWDIF output setup failed: {error}")))?;
        source.link(0, &mut processor, 0);
        // `send_field` owns the half-picture time base and emits distinct PTS
        // `2*p` and `2*p+1`. Keep those exact ticks: rescaling them back onto a
        // coarse source grid (commonly 1/25 for 25i) aliases adjacent fields.
        processor.link(0, &mut sink, 0);
        graph.validate().map_err(|error| {
            field_error(path, format!("BWDIF graph validation failed: {error}"))
        })?;
        Ok(Self {
            graph,
            input_intervals: VecDeque::new(),
            second_field: false,
        })
    }

    fn push(
        &mut self,
        frame: &ffmpeg::util::frame::video::Video,
        pts: i64,
        path: &Path,
    ) -> Result<Vec<ffmpeg::util::frame::video::Video>> {
        let input = clone_decoded_frame(frame, pts, path)?;
        // SAFETY: input is retained and exclusively borrowed here.
        let duration = unsafe { (*input.as_ptr()).duration };
        pts.checked_mul(2)
            .and_then(|start| start.checked_add(duration.max(0)))
            .ok_or_else(|| field_error(path, "field interval exceeds the exact integer range"))?;
        self.graph
            .get("mondrian_field_input")
            .ok_or_else(|| field_error(path, "BWDIF graph lost its input context"))?
            .source()
            .add(&input)
            .map_err(|error| field_error(path, format!("BWDIF input failed: {error}")))?;
        self.input_intervals.push_back((pts, duration));
        self.drain(path)
    }

    fn flush(&mut self, path: &Path) -> Result<Vec<ffmpeg::util::frame::video::Video>> {
        self.graph
            .get("mondrian_field_input")
            .ok_or_else(|| field_error(path, "BWDIF graph lost its input context"))?
            .source()
            .flush()
            .map_err(|error| field_error(path, format!("BWDIF flush failed: {error}")))?;
        self.drain(path)
    }

    fn drain(&mut self, path: &Path) -> Result<Vec<ffmpeg::util::frame::video::Video>> {
        let mut output = Vec::with_capacity(2);
        loop {
            let mut frame = ffmpeg::util::frame::video::Video::empty();
            let receive = self
                .graph
                .get("mondrian_field_output")
                .ok_or_else(|| field_error(path, "BWDIF graph lost its output context"))?
                .sink()
                .frame(&mut frame);
            match receive {
                Ok(()) => {
                    if frame.is_interlaced() {
                        return Err(field_error(
                            path,
                            "BWDIF emitted a residual interlaced frame",
                        ));
                    }
                    let &(pts, duration) = self.input_intervals.front().ok_or_else(|| {
                        field_error(path, "BWDIF output has no retained source interval")
                    })?;
                    // BWDIF extrapolates EOF timestamps from adjacent inputs.
                    // For a single picture both inputs have the same PTS.
                    // A proven decoded duration instead divides that actual
                    // picture into two half-duration fields, without extending
                    // EOF or inventing cadence from average frame rate.
                    if duration > 0 {
                        frame.set_pts(Some(pts * 2 + if self.second_field { duration } else { 0 }));
                        // SAFETY: this newly received output is exclusively owned.
                        unsafe { (*frame.as_mut_ptr()).duration = duration };
                    }
                    if self.second_field {
                        self.input_intervals.pop_front();
                    }
                    self.second_field = !self.second_field;
                    output.push(frame);
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(error) => {
                    return Err(field_error(path, format!("BWDIF output failed: {error}")))
                }
            }
        }
        Ok(output)
    }
}

fn clone_decoded_frame(
    frame: &ffmpeg::util::frame::video::Video,
    pts: i64,
    path: &Path,
) -> Result<ffmpeg::util::frame::video::Video> {
    // SAFETY: `frame` is valid for the borrow. The fresh AVFrame owns retained
    // buffer references and is released exactly once by the Video wrapper.
    let cloned = unsafe { ffmpeg::ffi::av_frame_clone(frame.as_ptr()) };
    if cloned.is_null() {
        return Err(field_error(
            path,
            "FFmpeg could not retain a field-processing input",
        ));
    }
    let mut cloned = unsafe { ffmpeg::util::frame::video::Video::wrap(cloned) };
    cloned.set_pts(Some(pts));
    Ok(cloned)
}

fn field_error(path: &Path, reason: impl Into<String>) -> MondrianError {
    MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_progressive_tail_keeps_its_complete_presentation_interval() {
        let path = Path::new("progressive-tail.mov");
        for (mode, scale) in [(Automatic, 2), (Progressive, 1)] {
            let mut session =
                PreviewFieldProcessingSession::new(mode, ffmpeg::Rational(100, 2997), path);
            let mut source = ffmpeg::util::frame::video::Video::new(
                ffmpeg::util::format::pixel::Pixel::YUV420P,
                16,
                16,
            );
            // SAFETY: the test exclusively owns this frame.
            unsafe {
                (*source.as_mut_ptr()).duration = 1;
            }
            let outputs = session.push(29, &source).expect("progressive picture");
            let frame = &outputs[0];
            assert_eq!(frame.pts(), Some(29 * scale));
            // SAFETY: frame is alive and borrowed for these metadata reads.
            let (duration, time_base) =
                unsafe { ((*frame.as_ptr()).duration, (*frame.as_ptr()).time_base) };
            assert_eq!(duration, scale);
            assert_eq!((time_base.num, time_base.den), (100, 2997 * scale as i32));
            let extent = crate::preview::DecodedTemporalExtent::from_duration(29 * scale, duration);
            assert!(
                extent.covers(30 * scale - 1),
                "the actual final picture still covers its last tick"
            );
            assert!(
                !extent.covers(30 * scale),
                "EOF must not invent another picture"
            );
            unsafe {
                (*source.as_mut_ptr()).duration = 0;
            }
            let unknown = session.push(30, &source).expect("unknown source duration");
            assert_eq!(unsafe { (*unknown[0].as_ptr()).duration }, 0);
        }
    }

    #[test]
    fn automatic_progressive_duration_overflow_fails_closed() {
        let mut session = PreviewFieldProcessingSession::new(
            Automatic,
            ffmpeg::Rational(1, 30),
            Path::new("invalid-duration.mov"),
        );
        let mut source = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            16,
            16,
        );
        // SAFETY: the test exclusively owns this frame.
        unsafe {
            (*source.as_mut_ptr()).duration = i64::MAX;
        }
        assert!(session.push(0, &source).is_err());
    }

    #[test]
    fn bwdif_does_not_create_an_unbudgeted_inner_thread_pool() {
        ffmpeg::init().expect("linked FFmpeg runtime");
        let frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            1920,
            1080,
        );
        let graph = BwdifGraph::open(
            &frame,
            ffmpeg::Rational(1, 50),
            PictureFieldDominance::TopFirst,
            Path::new("bounded-field-processing.mov"),
        )
        .expect("BWDIF graph");
        // SAFETY: the graph is initialized, owned here and only read during
        // this borrow. This is FFmpeg's actual configured execution ceiling.
        let threads = unsafe { (*graph.graph.as_ptr()).nb_threads };
        assert_eq!(
            threads, 1,
            "field processing must stay on its admitted worker"
        );
    }

    #[test]
    fn automatic_session_rejects_mixed_scan_and_dominance_changes() {
        let path = Path::new("field-source.mov");
        let mut mixed =
            PreviewFieldProcessingSession::new(Automatic, ffmpeg::Rational(1, 50), path);
        mixed.admit(ObservedScan::Progressive).expect("first scan");
        assert!(mixed
            .admit(ObservedScan::Interlaced(PictureFieldDominance::TopFirst))
            .expect_err("mixed scan must fail")
            .to_string()
            .contains("mixed progressive/interlaced"));

        let mut dominance =
            PreviewFieldProcessingSession::new(Automatic, ffmpeg::Rational(1, 50), path);
        dominance
            .admit(ObservedScan::Interlaced(PictureFieldDominance::TopFirst))
            .expect("first dominance");
        assert!(dominance
            .admit(ObservedScan::Interlaced(PictureFieldDominance::BottomFirst))
            .expect_err("dominance change must fail")
            .to_string()
            .contains("changing-dominance"));
    }

    #[test]
    fn reset_forgets_temporal_scan_evidence() {
        let path = Path::new("field-source.mov");
        let mut session =
            PreviewFieldProcessingSession::new(Automatic, ffmpeg::Rational(1, 50), path);
        session
            .admit(ObservedScan::Interlaced(PictureFieldDominance::TopFirst))
            .expect("initial scan");
        session.reset();
        session.admit(ObservedScan::Progressive).expect("post-seek scan");
    }

    #[test]
    fn linked_bwdif_preserves_two_picture_tail_intervals() {
        ffmpeg::init().expect("linked FFmpeg runtime");
        let path = Path::new("two-picture-interlaced.mov");
        for count in [1, 2] {
            let mut session = PreviewFieldProcessingSession::new(
                MotionAdaptiveFieldRate { dominance: PictureFieldDominance::TopFirst },
                ffmpeg::Rational(1, 2997),
                path,
            );
            let mut outputs = Vec::new();
            for index in 0..count {
                let mut frame = ffmpeg::util::frame::video::Video::new(
                    ffmpeg::util::format::pixel::Pixel::YUV420P,
                    16,
                    16,
                );
                frame.data_mut(0).fill(96);
                frame.data_mut(1).fill(128);
                frame.data_mut(2).fill(128);
                // SAFETY: the test exclusively owns this AVFrame.
                unsafe {
                    (*frame.as_mut_ptr()).flags |= ffmpeg::ffi::AV_FRAME_FLAG_INTERLACED
                        | ffmpeg::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
                    (*frame.as_mut_ptr()).duration = 100;
                }
                outputs.extend(session.push(index * 100, &frame).expect("BWDIF input"));
            }
            outputs.extend(session.flush().expect("BWDIF EOF"));
            let intervals = outputs
                .iter()
                .map(|frame| {
                    // SAFETY: each frame is alive and borrowed for metadata reads.
                    (frame.pts(), unsafe { (*frame.as_ptr()).duration })
                })
                .collect::<Vec<_>>();
            let expected = (0..count * 2).map(|field| (Some(field * 100), 100)).collect::<Vec<_>>();
            assert_eq!(intervals, expected);
            let tail = crate::preview::DecodedTemporalExtent::from_duration(
                intervals.last().unwrap().0.unwrap(),
                100,
            );
            assert!(tail.covers(count * 200 - 1));
            assert!(
                !tail.covers(count * 200),
                "no field beyond the proven source EOF"
            );
        }
    }

    #[test]
    fn linked_bwdif_emits_progressive_frames_at_distinct_field_timestamps() {
        ffmpeg::init().expect("linked FFmpeg runtime");
        let path = Path::new("synthetic-1080i.mov");
        let mut session = PreviewFieldProcessingSession::new(
            MotionAdaptiveFieldRate { dominance: PictureFieldDominance::TopFirst },
            ffmpeg::Rational(1, 50),
            path,
        );
        let mut outputs = Vec::new();
        for (pts, luma) in [(0_i64, 32_u8), (2, 96), (4, 160), (6, 224)] {
            let mut frame = ffmpeg::util::frame::video::Video::new(
                ffmpeg::util::format::pixel::Pixel::YUV420P,
                16,
                16,
            );
            frame.data_mut(0).fill(luma);
            frame.data_mut(1).fill(128);
            frame.data_mut(2).fill(128);
            // SAFETY: the test exclusively owns this AVFrame and sets only
            // FFmpeg's public scan flags, plus mirrors required by older bindings.
            unsafe {
                (*frame.as_mut_ptr()).flags |= ffmpeg::ffi::AV_FRAME_FLAG_INTERLACED
                    | ffmpeg::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
                #[cfg(not(mondrian_ffmpeg_8_0))]
                {
                    (*frame.as_mut_ptr()).interlaced_frame = 1;
                    (*frame.as_mut_ptr()).top_field_first = 1;
                }
            }
            outputs.extend(session.push(pts, &frame).expect("BWDIF input"));
        }
        outputs.extend(session.flush().expect("BWDIF flush"));

        assert!(outputs.len() >= 6, "send_field must retain field cadence");
        assert!(outputs.iter().all(|frame| !frame.is_interlaced()));
        let timestamps = outputs.iter().filter_map(|frame| frame.pts()).collect::<Vec<_>>();
        assert_eq!(timestamps.len(), outputs.len());
        assert!(timestamps.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
