//! Packet-source seam for Preview decode sessions.
//!
//! Decoder sessions consume one contract whether container work is direct or
//! isolated. This module alone selects the Adapter and owns seek/read error
//! normalization; codec, DPB, and output residency remain in `decode_session`.

pub(super) use super::demux_process::PreviewDemuxWorkerConfig;
use super::demux_process::{
    IsolatedDemuxOpenError, IsolatedDemuxRead, IsolatedDemuxSeek, IsolatedDemuxSession,
};
use super::demux_protocol::DemuxOpenPhase;
use super::seek_index::{
    preview_seek_index_contract_from_stream, PreviewSeekIndex, PreviewSeekIndexCache,
};
use super::*;
use ffmpeg::codec::packet::Mut as _;

pub(super) enum PreviewPacketRead {
    Packet(ffmpeg::Packet),
    End,
    DirectCanceled,
    IsolatedCanceled,
}

pub(super) enum PreviewPacketSeek {
    Complete,
    DirectCanceled,
    IsolatedCanceled,
}

pub(super) enum PreviewPacketSource {
    Direct(ffmpeg::format::context::Input),
    Isolated(Box<IsolatedDemuxSession>),
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
    IsolatedCanceled(PreviewDecodeCancellationCheckpoint),
    Failed(MondrianError),
}

impl PreviewPacketSource {
    pub(super) fn open(
        path: &Path,
        fingerprint: MediaFileFingerprint,
        video_stream_index: Option<u32>,
        seek_index_cache: &PreviewSeekIndexCache,
        demux_worker: Option<&PreviewDemuxWorkerConfig>,
        interrupt_state: &Arc<PreviewDecodeInterruptState>,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> std::result::Result<PreviewPacketSourceOpen, PreviewPacketSourceOpenError> {
        if let Some(config) = demux_worker {
            let mut last_open_checkpoint = PreviewDecodeCancellationCheckpoint::InputOpen;
            let mut observe_open_phase = |phase| {
                last_open_checkpoint = match phase {
                    DemuxOpenPhase::InputOpen => PreviewDecodeCancellationCheckpoint::InputOpen,
                    DemuxOpenPhase::StreamInfo => PreviewDecodeCancellationCheckpoint::StreamInfo,
                };
                interrupt_state.set_checkpoint(last_open_checkpoint);
            };
            return match IsolatedDemuxSession::open(
                config,
                path,
                fingerprint,
                video_stream_index,
                should_cancel,
                &mut observe_open_phase,
            ) {
                Ok(open) => {
                    let stream = open.stream;
                    let seek_index = seek_index_cache
                        .get(path, fingerprint, stream.stream_index)
                        .unwrap_or_else(|| {
                            let index =
                                PreviewSeekIndex::from_probe_keyframes(stream.keyframe_pts.clone());
                            if !stream.keyframe_index_truncated
                                && index.source == PreviewSeekIndexSource::ProbeBacked
                            {
                                seek_index_cache.put(
                                    path,
                                    fingerprint,
                                    stream.stream_index,
                                    &index.keyframe_pts,
                                );
                            }
                            index
                        });
                    Ok(PreviewPacketSourceOpen {
                        source: Self::Isolated(Box::new(open.source)),
                        parameters: stream.parameters,
                        stream_index: stream.stream_index,
                        stream_tb: stream.time_base,
                        stream_start_pts: stream.start_pts,
                        stream_rate: stream.frame_rate,
                        seek_index,
                    })
                }
                Err(IsolatedDemuxOpenError::Canceled) => Err(
                    PreviewPacketSourceOpenError::IsolatedCanceled(last_open_checkpoint),
                ),
                Err(IsolatedDemuxOpenError::Failed(reason)) => {
                    Err(PreviewPacketSourceOpenError::Failed(
                        MondrianError::MediaOpen { path: path.display().to_string(), reason },
                    ))
                }
            };
        }
        match Self::open_direct(
            path,
            fingerprint,
            video_stream_index,
            seek_index_cache,
            interrupt_state,
        ) {
            Ok(source) => Ok(source),
            Err(_) if should_cancel() => Err(PreviewPacketSourceOpenError::DirectCanceled),
            Err(error) => Err(PreviewPacketSourceOpenError::Failed(error)),
        }
    }

    pub(super) fn is_healthy(&self) -> bool {
        match self {
            Self::Direct(_) => true,
            Self::Isolated(source) => source.is_healthy(),
        }
    }

    pub(super) fn is_isolated(&self) -> bool {
        matches!(self, Self::Isolated(_))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn seek(
        &mut self,
        stream_index: usize,
        min_ts: i64,
        target_ts: i64,
        max_ts: i64,
        flags: i32,
        path: &Path,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<PreviewPacketSeek> {
        match self {
            Self::Direct(input) => {
                // SAFETY: the input is exclusively worker-owned and the
                // interrupt callback state outlives this direct source.
                let result = unsafe {
                    ffmpeg::ffi::avformat_seek_file(
                        input.as_mut_ptr(),
                        stream_index as i32,
                        min_ts,
                        target_ts,
                        max_ts,
                        flags,
                    )
                };
                if result < 0 {
                    if should_cancel() {
                        return Ok(PreviewPacketSeek::DirectCanceled);
                    }
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "seek to PTS {target_ts} failed with code {result}: {}",
                            ffmpeg::Error::from(result)
                        ),
                    });
                }
                if should_cancel() {
                    Ok(PreviewPacketSeek::DirectCanceled)
                } else {
                    Ok(PreviewPacketSeek::Complete)
                }
            }
            Self::Isolated(source) => {
                match source.seek(min_ts, target_ts, max_ts, flags, should_cancel) {
                    Ok(IsolatedDemuxSeek::Complete) => Ok(PreviewPacketSeek::Complete),
                    Ok(IsolatedDemuxSeek::Canceled) => Ok(PreviewPacketSeek::IsolatedCanceled),
                    Err(reason) => Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason,
                    }),
                }
            }
        }
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
                        return Ok(PreviewPacketRead::DirectCanceled);
                    }
                    std::thread::yield_now();
                    continue;
                }
                if result < 0 {
                    if should_cancel() {
                        return Ok(PreviewPacketRead::DirectCanceled);
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
                Ok(IsolatedDemuxRead::Canceled) => Ok(PreviewPacketRead::IsolatedCanceled),
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
        video_stream_index: Option<u32>,
        seek_index_cache: &PreviewSeekIndexCache,
        interrupt_state: &Arc<PreviewDecodeInterruptState>,
    ) -> Result<PreviewPacketSourceOpen> {
        let input = open_preview_input(path, interrupt_state)?;
        // Close the remaining validation/open race for local sources. If the
        // path changed while FFmpeg was opening it, discard this input before
        // stream facts or a reusable Session can be published.
        verify_preview_source_revision(path, fingerprint)?;
        let (stream_index, parameters, stream_tb, stream_start_pts, stream_rate, seek_index) = {
            let stream = match video_stream_index {
                Some(index) => input
                    .streams()
                    .find(|stream| stream.index() == index as usize)
                    .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
                    .ok_or_else(|| MondrianError::UnsupportedFormat {
                        format: format!(
                            "requested video stream {index} is missing or is not video"
                        ),
                    })?,
                None => input.streams().best(ffmpeg::media::Type::Video).ok_or_else(|| {
                    MondrianError::UnsupportedFormat { format: "no video stream".to_owned() }
                })?,
            };
            let stream_index = stream.index();
            let stream_start_pts = match stream.start_time() {
                value if value == ffmpeg::ffi::AV_NOPTS_VALUE => 0,
                value => value,
            };
            let seek_index =
                seek_index_cache.get(path, fingerprint, stream_index).unwrap_or_else(|| {
                    let (seek_index, truncated) = preview_seek_index_contract_from_stream(&stream);
                    if !truncated && seek_index.source == PreviewSeekIndexSource::ProbeBacked {
                        seek_index_cache.put(
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
    let path_utf8 = path.to_str().ok_or_else(|| MondrianError::MediaOpen {
        path: path.display().to_string(),
        reason: "media path is not representable by FFmpeg's UTF-8 path Adapter".to_owned(),
    })?;
    let path_c = CString::new(path_utf8).map_err(|error| MondrianError::MediaOpen {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn ffmpeg_input_open_interrupts_a_blocked_http_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local stall server");
        let address = listener.local_addr().expect("stall server address");
        let (request_tx, request_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept FFmpeg HTTP connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bound request-read timeout");
            let mut request = Vec::with_capacity(1_024);
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                assert!(
                    request.len() < 16 * 1_024,
                    "FFmpeg HTTP request exceeded test cap"
                );
                let read = stream.read(&mut chunk).expect("read FFmpeg HTTP request");
                assert!(read > 0, "FFmpeg closed before sending an HTTP request");
                request.extend_from_slice(&chunk[..read]);
            }
            request_tx.send(()).expect("publish complete HTTP request");
            // A complete request proves FFmpeg has entered input-open protocol
            // I/O. Keep the response absent until cancellation returns.
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });

        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let url = PathBuf::from(format!("http://{address}/blocked-open.mp4"));
        ensure_ffmpeg_initialized(&url).expect("initialize FFmpeg network protocols");
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let observer = PreviewDecodeExecutionObserver::new();
            let interrupt_state = Arc::new(PreviewDecodeInterruptState::with_execution_observer(
                observer,
            ));
            let should_cancel: PreviewDecodeCancelProbe =
                Arc::new(move || worker_canceled.load(Ordering::Acquire));
            let _interrupt_guard = interrupt_state.install(Arc::clone(&should_cancel));
            let synthetic_remote_revision = MediaFileFingerprint {
                len: Some(0),
                modified_secs: Some(0),
                modified_nanos: Some(0),
                object_identity: Some(MediaFileObjectIdentity::Unix { device: 1, inode: 1 }),
                change_stamp: Some(MediaFileChangeStamp::Unix { seconds: 0, nanoseconds: 0 }),
            };
            let result = PreviewPacketSource::open(
                &url,
                synthetic_remote_revision,
                None,
                &PreviewSeekIndexCache::default(),
                None,
                &interrupt_state,
                should_cancel.as_ref(),
            );
            let outcome = match result {
                Err(PreviewPacketSourceOpenError::DirectCanceled) => Ok(
                    interrupt_state.cancellation(PreviewDecodeCancellationCheckpoint::InputOpen)
                ),
                Err(PreviewPacketSourceOpenError::IsolatedCanceled(checkpoint)) => Err(format!(
                    "unexpected isolated cancellation at {checkpoint:?}"
                )),
                Err(PreviewPacketSourceOpenError::Failed(error)) => Err(error.to_string()),
                Ok(_) => Err("stalled HTTP input unexpectedly opened".to_owned()),
            };
            let _ = outcome_tx.send(outcome);
        });

        request_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("FFmpeg must send the controlled HTTP request before cancellation");
        let requested_at = Instant::now();
        canceled.store(true, Ordering::Release);
        let outcome = outcome_rx.recv_timeout(Duration::from_secs(5));
        let return_latency = requested_at.elapsed();

        let _ = release_tx.send(());
        server.join().expect("stall server must return");
        let outcome = outcome
            .expect("FFmpeg interrupt callback must stop blocked input open")
            .expect("blocked input cancellation must not become a media failure");
        worker.join().expect("decode worker must return");
        assert_eq!(
            outcome,
            PreviewDecodeCancellation::ffmpeg_interrupt(
                PreviewDecodeCancellationCheckpoint::InputOpen,
            )
        );
        assert!(
            return_latency <= Duration::from_millis(500),
            "blocked input open returned too late after cancellation: {return_latency:?}"
        );
    }
}
