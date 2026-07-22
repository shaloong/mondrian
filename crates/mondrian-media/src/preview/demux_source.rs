//! Packet-source abstraction for Preview decode sessions.
//!
//! Decoder sessions consume one contract whether container work is direct or
//! isolated. This module alone knows which side owns `AVFormatContext`; codec,
//! DPB, and output residency remain in `decode_session`.

pub(super) use super::demux_process::PreviewDemuxWorkerConfig;
use super::demux_process::{IsolatedDemuxOpenError, IsolatedDemuxRead, IsolatedDemuxRequest};
use super::seek_index::{
    preview_seek_index_cache_get, preview_seek_index_cache_put, preview_seek_index_from_stream,
    PreviewSeekIndex,
};
use super::*;
use ffmpeg::codec::packet::Mut as _;

pub(super) enum PreviewPacketRead {
    Packet(ffmpeg::Packet),
    End,
    Canceled,
}

pub(super) enum PreviewPacketSource {
    Direct(ffmpeg::format::context::Input),
    Isolated(IsolatedDemuxRequest),
}

pub(super) struct PreviewPacketSourceOpen {
    pub source: PreviewPacketSource,
    pub parameters: ffmpeg::codec::Parameters,
    pub stream_index: usize,
    pub stream_tb: ffmpeg::Rational,
    pub stream_start_pts: i64,
    pub stream_rate: ffmpeg::Rational,
    pub seek_index: PreviewSeekIndex,
}

pub(super) enum PreviewPacketSourceOpenError {
    DirectCanceled,
    IsolatedCanceled,
    Failed(MondrianError),
}

impl PreviewPacketSource {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open(
        path: &Path,
        fingerprint: MediaFileFingerprint,
        access_mode: PreviewDecodeAccessMode,
        source_time: TimelineTime,
        demux_worker: Option<&PreviewDemuxWorkerConfig>,
        interrupt_state: &Arc<PreviewDecodeInterruptState>,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> std::result::Result<PreviewPacketSourceOpen, PreviewPacketSourceOpenError> {
        if access_mode == PreviewDecodeAccessMode::RandomAccessStillFrame {
            if let Some(config) = demux_worker {
                interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::InputOpen);
                return match IsolatedDemuxRequest::open(config, path, source_time, should_cancel) {
                    Ok(open) => {
                        let stream = open.stream;
                        let seek_index =
                            preview_seek_index_cache_get(path, fingerprint, stream.stream_index)
                                .unwrap_or_default();
                        Ok(PreviewPacketSourceOpen {
                            source: Self::Isolated(open.source),
                            parameters: stream.parameters,
                            stream_index: stream.stream_index,
                            stream_tb: stream.time_base,
                            stream_start_pts: stream.start_pts,
                            stream_rate: stream.frame_rate,
                            seek_index,
                        })
                    }
                    Err(IsolatedDemuxOpenError::Canceled) => {
                        Err(PreviewPacketSourceOpenError::IsolatedCanceled)
                    }
                    Err(IsolatedDemuxOpenError::Failed(reason)) => {
                        Err(PreviewPacketSourceOpenError::Failed(
                            MondrianError::MediaOpen { path: path.display().to_string(), reason },
                        ))
                    }
                };
            }
        }
        match Self::open_direct(path, fingerprint, interrupt_state) {
            Ok(source) => Ok(source),
            Err(_) if should_cancel() => Err(PreviewPacketSourceOpenError::DirectCanceled),
            Err(error) => Err(PreviewPacketSourceOpenError::Failed(error)),
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        matches!(self, Self::Isolated(source) if source.is_terminal())
    }

    pub(super) fn is_isolated(&self) -> bool {
        matches!(self, Self::Isolated(_))
    }

    pub(super) fn isolated_demux_was_canceled(&self) -> bool {
        matches!(self, Self::Isolated(source) if source.was_canceled())
    }

    pub(super) fn finish_isolated_one_shot(&mut self) {
        if let Self::Isolated(source) = self {
            source.finish_one_shot();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn seek(
        &mut self,
        stream_index: usize,
        min_ts: i64,
        seek_target_ts: i64,
        max_ts: i64,
        seek_flags: i32,
        requested_target_pts: i64,
        path: &Path,
    ) -> Result<()> {
        let result = match self {
            Self::Direct(input) => unsafe {
                ffmpeg::ffi::avformat_seek_file(
                    input.as_mut_ptr(),
                    stream_index as i32,
                    min_ts,
                    seek_target_ts,
                    max_ts,
                    seek_flags,
                )
            },
            Self::Isolated(source) => {
                if source.seek_target_pts() != requested_target_pts {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "one-shot isolated demux target {} cannot satisfy target {requested_target_pts}",
                            source.seek_target_pts()
                        ),
                    });
                }
                0
            }
        };
        if result < 0 {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!("seek failed with code {result}"),
            });
        }
        Ok(())
    }

    pub(super) fn read_next(
        &mut self,
        path: &Path,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<PreviewPacketRead> {
        match self {
            Self::Direct(input) => loop {
                let mut packet = ffmpeg::Packet::empty();
                // SAFETY: input and packet are exclusively worker-owned.
                let result =
                    unsafe { ffmpeg::ffi::av_read_frame(input.as_mut_ptr(), packet.as_mut_ptr()) };
                if result == ffmpeg::ffi::AVERROR_EOF {
                    return Ok(PreviewPacketRead::End);
                }
                if result == ffmpeg::ffi::AVERROR(ffmpeg::ffi::EAGAIN) {
                    if should_cancel() {
                        return Ok(PreviewPacketRead::Canceled);
                    }
                    continue;
                }
                if result < 0 {
                    if should_cancel() {
                        return Ok(PreviewPacketRead::Canceled);
                    }
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "packet read failed with code {result}: {}",
                            ffmpeg::Error::from(result)
                        ),
                    });
                }
                return Ok(PreviewPacketRead::Packet(packet));
            },
            Self::Isolated(source) => match source.read_next(should_cancel) {
                Ok(IsolatedDemuxRead::Packet(packet)) => Ok(PreviewPacketRead::Packet(packet)),
                Ok(IsolatedDemuxRead::End) => Ok(PreviewPacketRead::End),
                Ok(IsolatedDemuxRead::Canceled) => Ok(PreviewPacketRead::Canceled),
                Err(reason) => Err(MondrianError::DecodeFailed {
                    asset_id: path.display().to_string(),
                    reason,
                }),
            },
        }
    }

    fn open_direct(
        path: &Path,
        fingerprint: MediaFileFingerprint,
        interrupt_state: &Arc<PreviewDecodeInterruptState>,
    ) -> Result<PreviewPacketSourceOpen> {
        let input = open_preview_input(path, interrupt_state)?;
        let (stream_index, parameters, stream_tb, stream_start_pts, stream_rate, seek_index) = {
            let stream = input.streams().best(ffmpeg::media::Type::Video).ok_or_else(|| {
                MondrianError::UnsupportedFormat { format: "no video stream".to_owned() }
            })?;
            let stream_index = stream.index();
            let stream_start_pts = match stream.start_time() {
                value if value == ffmpeg::ffi::AV_NOPTS_VALUE => 0,
                value => value,
            };
            let seek_index = preview_seek_index_cache_get(path, fingerprint, stream_index)
                .unwrap_or_else(|| {
                    let seek_index = preview_seek_index_from_stream(&stream);
                    if seek_index.source == PreviewSeekIndexSource::ProbeBacked {
                        preview_seek_index_cache_put(
                            path,
                            fingerprint,
                            stream_index,
                            &seek_index.keyframe_pts,
                        );
                    }
                    seek_index
                });
            (
                stream_index,
                stream.parameters(),
                stream.time_base(),
                stream_start_pts,
                stream.rate(),
                seek_index,
            )
        };
        Ok(PreviewPacketSourceOpen {
            source: Self::Direct(input),
            parameters,
            stream_index,
            stream_tb,
            stream_start_pts,
            stream_rate,
            seek_index,
        })
    }
}

fn open_preview_input(
    path: &Path,
    interrupt_state: &Arc<PreviewDecodeInterruptState>,
) -> Result<ffmpeg::format::context::Input> {
    let path_string = path.to_string_lossy();
    let path_c =
        CString::new(path_string.as_bytes()).map_err(|error| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: format!("media path contains an interior NUL byte: {error}"),
        })?;

    // SAFETY: the allocated context is transferred into the safe wrapper or
    // closed on every error path. The callback state outlives the direct source.
    unsafe {
        let mut input = ffmpeg::ffi::avformat_alloc_context();
        if input.is_null() {
            return Err(MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: "FFmpeg could not allocate an input context".to_owned(),
            });
        }
        (*input).interrupt_callback = ffmpeg::ffi::AVIOInterruptCB {
            callback: Some(preview_decode_interrupt_callback),
            opaque: Arc::as_ptr(interrupt_state).cast_mut().cast(),
        };

        interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::InputOpen);
        let open_result = ffmpeg::ffi::avformat_open_input(
            &mut input,
            path_c.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if open_result < 0 {
            if !input.is_null() {
                ffmpeg::ffi::avformat_close_input(&mut input);
            }
            return Err(MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: ffmpeg::Error::from(open_result).to_string(),
            });
        }

        interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::StreamInfo);
        let stream_info_result =
            ffmpeg::ffi::avformat_find_stream_info(input, std::ptr::null_mut());
        if stream_info_result < 0 {
            ffmpeg::ffi::avformat_close_input(&mut input);
            return Err(MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: ffmpeg::Error::from(stream_info_result).to_string(),
            });
        }

        Ok(ffmpeg::format::context::Input::wrap(input))
    }
}
