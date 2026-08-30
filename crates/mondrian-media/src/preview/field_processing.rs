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
            ObservedScan::Progressive => Ok(vec![clone_decoded_frame(frame, pts, &self.path)?]),
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
        let settb = ffmpeg::filter::find("settb")
            .ok_or_else(|| field_error(path, "linked FFmpeg runtime has no settb filter"))?;
        let mut graph = ffmpeg::filter::Graph::new();
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
        // BWDIF send_field halves the link time base while doubling PTS. Restore
        // the physical stream time base so the existing exact PTS selector can
        // compare filtered outputs with the caller's SourceSampleTarget. Sources
        // whose original tick grid cannot represent a half-picture instant will
        // produce duplicate PTS and are rejected by the candidate window.
        let mut restore_time_base = graph
            .add(&settb, "mondrian_field_time_base", "expr=2*intb")
            .map_err(|error| field_error(path, format!("BWDIF time-base setup failed: {error}")))?;
        let mut sink = graph
            .add(&buffersink, "mondrian_field_output", "")
            .map_err(|error| field_error(path, format!("BWDIF output setup failed: {error}")))?;
        source.link(0, &mut processor, 0);
        processor.link(0, &mut restore_time_base, 0);
        restore_time_base.link(0, &mut sink, 0);
        graph.validate().map_err(|error| {
            field_error(path, format!("BWDIF graph validation failed: {error}"))
        })?;
        Ok(Self { graph })
    }

    fn push(
        &mut self,
        frame: &ffmpeg::util::frame::video::Video,
        pts: i64,
        path: &Path,
    ) -> Result<Vec<ffmpeg::util::frame::video::Video>> {
        let input = clone_decoded_frame(frame, pts, path)?;
        self.graph
            .get("mondrian_field_input")
            .ok_or_else(|| field_error(path, "BWDIF graph lost its input context"))?
            .source()
            .add(&input)
            .map_err(|error| field_error(path, format!("BWDIF input failed: {error}")))?;
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
            // FFmpeg's public scan flags plus their compatibility mirrors.
            unsafe {
                (*frame.as_mut_ptr()).flags |= ffmpeg::ffi::AV_FRAME_FLAG_INTERLACED
                    | ffmpeg::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
                (*frame.as_mut_ptr()).interlaced_frame = 1;
                (*frame.as_mut_ptr()).top_field_first = 1;
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
