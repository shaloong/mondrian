//! Child-process implementation for isolated Preview container demux.

use super::demux_protocol::{
    read_worker_request, write_end_message, write_error_message, write_packet_message,
    write_protocol_preamble, write_stream_message,
};
use super::{ensure_ffmpeg_initialized, source_time_to_stream_pts};
use ffmpeg::codec::packet::Mut as _;
use ffmpeg_next as ffmpeg;
use mondrian_core::TimelineTime;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

/// Run the isolated Preview demux worker over stdout.
///
/// The caller must launch this function in the packaged helper executable.
/// Stdout is exclusively the versioned binary protocol; diagnostics belong on
/// stderr. Terminating the helper is the parent's recovery mechanism when any
/// FFmpeg format/open/seek/read call does not return.
pub fn run_preview_demux_worker() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let request = read_worker_request(&mut BufReader::new(stdin.lock()))?;
    let stdout = io::stdout();
    let mut writer = BufWriter::with_capacity(64 * 1024, stdout.lock());
    write_protocol_preamble(&mut writer, request.nonce)?;
    writer.flush()?;

    match run_worker(&request.path, request.source_time, &mut writer) {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = format!("{error:#}");
            let _ = write_error_message(&mut writer, &message);
            let _ = writer.flush();
            Err(error)
        }
    }
}

fn run_worker(
    path: &Path,
    source_time: TimelineTime,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    if source_time.is_negative() {
        anyhow::bail!("Preview demux source time cannot be negative: {source_time}");
    }
    ensure_ffmpeg_initialized(path)?;
    let mut input = ffmpeg::format::input(path)
        .map_err(|error| anyhow::anyhow!("open Preview demux input {}: {error}", path.display()))?;
    let (stream_index, parameters, time_base, start_pts, frame_rate) = {
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| anyhow::anyhow!("Preview demux input has no video stream"))?;
        let start_pts = match stream.start_time() {
            value if value == ffmpeg::ffi::AV_NOPTS_VALUE => 0,
            value => value,
        };
        (
            stream.index(),
            stream.parameters(),
            stream.time_base(),
            start_pts,
            stream.rate(),
        )
    };
    let seek_target_pts =
        source_time_to_stream_pts(source_time, time_base, start_pts).map_err(anyhow::Error::msg)?;
    write_stream_message(
        writer,
        &parameters,
        stream_index,
        time_base,
        start_pts,
        frame_rate,
        seek_target_pts,
    )?;
    writer.flush()?;

    // SAFETY: input owns a live AVFormatContext. The helper process is itself
    // the interruption boundary, so a parent cancellation terminates this
    // process even if this call never returns.
    let seek_result = unsafe {
        ffmpeg::ffi::avformat_seek_file(
            input.as_mut_ptr(),
            stream_index as i32,
            i64::MIN,
            seek_target_pts,
            seek_target_pts,
            ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
        )
    };
    if seek_result < 0 {
        anyhow::bail!(
            "Preview demux seek to PTS {seek_target_pts} failed: {}",
            ffmpeg::Error::from(seek_result)
        );
    }

    loop {
        let mut packet = ffmpeg::Packet::empty();
        // Do not use ffmpeg-next's PacketIter here: it retries every non-EOF
        // demux error forever. This explicit boundary preserves EOF, EAGAIN,
        // and terminal failures as distinct worker outcomes.
        let read_result =
            unsafe { ffmpeg::ffi::av_read_frame(input.as_mut_ptr(), packet.as_mut_ptr()) };
        if read_result == ffmpeg::ffi::AVERROR_EOF {
            break;
        }
        if read_result == ffmpeg::ffi::AVERROR(ffmpeg::ffi::EAGAIN) {
            anyhow::bail!("Preview demux read unexpectedly returned EAGAIN for a local file");
        }
        if read_result < 0 {
            anyhow::bail!(
                "Preview demux packet read failed with code {read_result}: {}",
                ffmpeg::Error::from(read_result)
            );
        }
        if packet.stream() == stream_index {
            write_packet_message(writer, &packet)?;
            writer.flush()?;
        }
    }
    write_end_message(writer)?;
    writer.flush()?;
    Ok(())
}
