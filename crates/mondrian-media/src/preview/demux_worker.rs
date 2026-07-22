//! Child-process implementation for one reusable Preview demux session.

use super::demux_protocol::{
    read_worker_command, read_worker_request, write_closed_message, write_end_message,
    write_error_message, write_open_phase_message, write_packet_message, write_protocol_preamble,
    write_seek_complete_message, write_stream_message, DemuxOpenPhase, DemuxWorkerCommand,
};
use super::ensure_ffmpeg_initialized;
use super::seek_index::preview_seek_index_contract_from_stream;
use super::MediaFileFingerprint;
use ffmpeg::codec::packet::Mut as _;
use ffmpeg_next as ffmpeg;
use std::ffi::CString;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

const EAGAIN_RETRY_DELAY: Duration = Duration::from_millis(1);

/// Run the isolated Preview demux worker over stdin/stdout.
///
/// The helper owns exactly one `AVFormatContext`. Stdout is exclusively the
/// bounded v2 binary protocol; diagnostics belong on stderr. The parent may
/// terminate the process at any point to recover a blocked FFmpeg format call.
pub fn run_preview_demux_worker() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::with_capacity(64 * 1024, stdin.lock());
    let request = read_worker_request(&mut reader)?;
    let stdout = io::stdout();
    let mut writer = BufWriter::with_capacity(64 * 1024, stdout.lock());
    write_protocol_preamble(&mut writer, request.nonce)?;
    writer.flush()?;

    if let Err(error) = run_worker(
        &request.path,
        request.source_revision,
        &mut reader,
        &mut writer,
    ) {
        eprintln!("Preview demux worker failed: {error:#}");
        return Err(error);
    }
    Ok(())
}

fn run_worker(
    path: &Path,
    source_revision: MediaFileFingerprint,
    reader: &mut impl io::Read,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    if let Err(error) = validate_source_revision(path, source_revision, "before open") {
        return send_failure(writer, 0, error);
    }
    let mut input = match open_input(path, writer) {
        Ok(input) => input,
        Err(error) => return send_failure(writer, 0, error),
    };
    if let Err(error) = validate_source_revision(path, source_revision, "after stream discovery") {
        return send_failure(writer, 0, error);
    }
    let (stream_index, parameters, time_base, start_pts, frame_rate, seek_index, truncated) = {
        let stream = match input.streams().best(ffmpeg::media::Type::Video) {
            Some(stream) => stream,
            None => {
                return send_failure(
                    writer,
                    0,
                    anyhow::anyhow!("Preview demux input has no video stream"),
                )
            }
        };
        let start_pts = match stream.start_time() {
            value if value == ffmpeg::ffi::AV_NOPTS_VALUE => 0,
            value => value,
        };
        let (seek_index, truncated) = preview_seek_index_contract_from_stream(&stream);
        (
            stream.index(),
            stream.parameters(),
            stream.time_base(),
            start_pts,
            stream.rate(),
            seek_index,
            truncated,
        )
    };
    if let Err(error) = write_stream_message(
        writer,
        &parameters,
        stream_index,
        time_base,
        start_pts,
        frame_rate,
        &seek_index.keyframe_pts,
        truncated,
    )
    .and_then(|()| writer.flush())
    {
        return Err(error.into());
    }

    let mut expected_command_id = 1_u64;
    loop {
        let command = match read_worker_command(reader) {
            Ok(command) => command,
            Err(error) => {
                return send_failure(
                    writer,
                    0,
                    anyhow::anyhow!("read Preview demux command: {error}"),
                )
            }
        };
        if command.command_id() != expected_command_id {
            return send_failure(
                writer,
                command.command_id(),
                anyhow::anyhow!(
                    "Preview demux command id {} did not match expected {expected_command_id}",
                    command.command_id()
                ),
            );
        }
        expected_command_id = match expected_command_id.checked_add(1) {
            Some(next) => next,
            None => {
                return send_failure(
                    writer,
                    command.command_id(),
                    anyhow::anyhow!("Preview demux command identifier overflow"),
                )
            }
        };

        match command {
            DemuxWorkerCommand::Seek { command_id, min_ts, target_ts, max_ts, flags } => {
                // SAFETY: input owns the live AVFormatContext and this process
                // serializes all operations. Parent termination is the
                // interruption boundary if FFmpeg does not return.
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
                    return send_failure(
                        writer,
                        command_id,
                        anyhow::anyhow!(
                            "Preview demux seek to PTS {target_ts} failed with code {result}: {}",
                            ffmpeg::Error::from(result)
                        ),
                    );
                }
                write_seek_complete_message(writer, command_id)?;
                writer.flush()?;
            }
            DemuxWorkerCommand::Read { command_id } => {
                match read_next_video_packet(&mut input, stream_index) {
                    Ok(Some(packet)) => write_packet_message(writer, command_id, &packet)?,
                    Ok(None) => write_end_message(writer, command_id)?,
                    Err(error) => return send_failure(writer, command_id, error),
                }
                writer.flush()?;
            }
            DemuxWorkerCommand::Close { command_id } => {
                write_closed_message(writer, command_id)?;
                writer.flush()?;
                return Ok(());
            }
        }
    }
}

fn validate_source_revision(
    path: &Path,
    expected: MediaFileFingerprint,
    checkpoint: &str,
) -> anyhow::Result<()> {
    if !expected.authorizes_reuse() {
        return Ok(());
    }
    let observed = MediaFileFingerprint::capture(path);
    if observed != expected {
        anyhow::bail!(
            "Preview demux source revision changed {checkpoint}: expected {expected:?}, observed {observed:?}"
        );
    }
    Ok(())
}

fn open_input(
    path: &Path,
    writer: &mut impl Write,
) -> anyhow::Result<ffmpeg::format::context::Input> {
    ensure_ffmpeg_initialized(path)?;
    let path_utf8 = path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "Preview demux input path is not representable by FFmpeg's UTF-8 path Adapter: {}",
            path.display()
        )
    })?;
    let path_c = CString::new(path_utf8).map_err(|error| {
        anyhow::anyhow!(
            "Preview demux input path contains an interior NUL byte ({}): {error}",
            path.display()
        )
    })?;

    write_open_phase_message(writer, DemuxOpenPhase::InputOpen)?;
    writer.flush()?;
    // SAFETY: every error path closes the context if FFmpeg allocated one;
    // success transfers sole ownership to the ffmpeg-next Input wrapper.
    unsafe {
        let mut input = std::ptr::null_mut();
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
            anyhow::bail!(
                "open Preview demux input {}: {}",
                path.display(),
                ffmpeg::Error::from(open_result)
            );
        }

        if let Err(error) = write_open_phase_message(writer, DemuxOpenPhase::StreamInfo)
            .and_then(|()| writer.flush())
        {
            ffmpeg::ffi::avformat_close_input(&mut input);
            return Err(error.into());
        }
        let stream_info_result =
            ffmpeg::ffi::avformat_find_stream_info(input, std::ptr::null_mut());
        if stream_info_result < 0 {
            ffmpeg::ffi::avformat_close_input(&mut input);
            anyhow::bail!(
                "discover Preview demux streams for {}: {}",
                path.display(),
                ffmpeg::Error::from(stream_info_result)
            );
        }
        Ok(ffmpeg::format::context::Input::wrap(input))
    }
}

fn read_next_video_packet(
    input: &mut ffmpeg::format::context::Input,
    stream_index: usize,
) -> anyhow::Result<Option<ffmpeg::Packet>> {
    loop {
        let mut packet = ffmpeg::Packet::empty();
        // Do not use ffmpeg-next's PacketIter here: it retries every non-EOF
        // demux error forever. EAGAIN receives bounded-backoff retries while
        // the parent remains able to terminate this process immediately.
        let result = unsafe { ffmpeg::ffi::av_read_frame(input.as_mut_ptr(), packet.as_mut_ptr()) };
        if result == ffmpeg::ffi::AVERROR_EOF {
            return Ok(None);
        }
        if result == ffmpeg::ffi::AVERROR(ffmpeg::ffi::EAGAIN) {
            thread::sleep(EAGAIN_RETRY_DELAY);
            continue;
        }
        if result < 0 {
            anyhow::bail!(
                "Preview demux packet read failed with code {result}: {}",
                ffmpeg::Error::from(result)
            );
        }
        if packet.stream() == stream_index {
            return Ok(Some(packet));
        }
    }
}

fn send_failure<T>(
    writer: &mut impl Write,
    command_id: u64,
    error: anyhow::Error,
) -> anyhow::Result<T> {
    let message = format!("{error:#}");
    let _ = write_error_message(writer, command_id, &message).and_then(|()| writer.flush());
    Err(error)
}
