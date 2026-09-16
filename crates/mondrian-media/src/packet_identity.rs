//! Encoded video-packet identity for conservative essence reuse.
//!
//! Smart Render qualification first proves author and delivery semantics in
//! their owning Modules. This Adapter supplies the final physical fact: one
//! stable file revision contains an exact ordered encoded-packet payload, and
//! a remuxed output contains the same payload. It does not decide whether a
//! timeline range is eligible or whether a codec/container is compatible.

use crate::MediaFileFingerprint;
use ffmpeg_next as ffmpeg;
use mondrian_core::ExecutionCancellationToken;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Stable identity of every encoded packet in one selected video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPacketIdentity {
    /// Absolute container stream index that was inspected.
    pub stream_index: u32,
    /// Number of encoded video packets included in the identity.
    pub packet_count: u64,
    /// Total encoded payload bytes across those packets.
    pub payload_bytes: u64,
    /// Framed SHA-256 digest over ordered packet payloads.
    pub payload_digest: [u8; 32],
    /// Whether the first encoded packet is marked as independently decodable.
    pub first_packet_is_key: bool,
}

/// Failure to capture trustworthy encoded-packet identity.
#[derive(Debug, thiserror::Error)]
pub enum VideoPacketIdentityError {
    /// Owning execution generation was canceled cooperatively.
    #[error("video packet identity capture canceled")]
    Canceled,
    /// FFmpeg runtime initialization failed.
    #[error("initialize FFmpeg for packet identity: {0}")]
    Initialize(ffmpeg::Error),
    /// Source revision evidence was incomplete or changed during inspection.
    #[error("media revision changed or is incomplete while inspecting {path}")]
    SourceRevisionChanged {
        /// Physical source path.
        path: PathBuf,
    },
    /// The container could not be opened.
    #[error("open packet identity source {path}: {source}")]
    Open {
        /// Physical source path.
        path: PathBuf,
        /// FFmpeg failure.
        source: ffmpeg::Error,
    },
    /// Requested or primary video stream was unavailable.
    #[error("video stream {requested:?} is unavailable in {path}")]
    MissingVideoStream {
        /// Physical source path.
        path: PathBuf,
        /// Requested absolute stream index, or `None` for primary video.
        requested: Option<u32>,
    },
    /// The selected stream index cannot be represented by the public contract.
    #[error("video stream index {index} exceeds u32")]
    StreamIndexOverflow {
        /// FFmpeg stream index.
        index: usize,
    },
    /// A packet had no accessible encoded payload.
    #[error("video packet {packet_index} in stream {stream_index} has no payload")]
    MissingPacketPayload {
        /// Selected absolute stream index.
        stream_index: u32,
        /// Zero-based selected-stream packet index.
        packet_index: u64,
    },
    /// Payload byte accounting overflowed the supported evidence extent.
    #[error("video packet payload byte count overflowed for stream {stream_index}")]
    PayloadSizeOverflow {
        /// Selected absolute stream index.
        stream_index: u32,
    },
    /// Selected video stream had no packets.
    #[error("video stream {stream_index} contains no encoded packets")]
    EmptyVideoStream {
        /// Selected absolute stream index.
        stream_index: u32,
    },
}

/// Capture one stable full-stream encoded-packet identity.
///
/// `requested_stream_index=None` selects FFmpeg's primary video stream. The
/// file fingerprint is observed before opening and after complete packet
/// traversal; partial filesystem evidence never authorizes reuse.
pub fn capture_video_packet_identity(
    path: &Path,
    requested_stream_index: Option<u32>,
) -> Result<VideoPacketIdentity, VideoPacketIdentityError> {
    capture_video_packet_identity_cancellable(
        path,
        requested_stream_index,
        &ExecutionCancellationToken::new(),
    )
}

/// Capture packet identity with cooperative cancellation between demuxed packets.
pub fn capture_video_packet_identity_cancellable(
    path: &Path,
    requested_stream_index: Option<u32>,
    cancellation: &ExecutionCancellationToken,
) -> Result<VideoPacketIdentity, VideoPacketIdentityError> {
    if cancellation.is_canceled() {
        return Err(VideoPacketIdentityError::Canceled);
    }
    ffmpeg::init().map_err(VideoPacketIdentityError::Initialize)?;
    let before = MediaFileFingerprint::capture(path);
    if !before.authorizes_reuse() {
        return Err(VideoPacketIdentityError::SourceRevisionChanged { path: path.to_path_buf() });
    }
    let mut input = ffmpeg::format::input(path)
        .map_err(|source| VideoPacketIdentityError::Open { path: path.to_path_buf(), source })?;
    let selected_index = match requested_stream_index {
        Some(index) => input
            .streams()
            .find(|stream| stream.index() == index as usize)
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
            .map(|stream| stream.index()),
        None => input.streams().best(ffmpeg::media::Type::Video).map(|stream| stream.index()),
    }
    .ok_or_else(|| VideoPacketIdentityError::MissingVideoStream {
        path: path.to_path_buf(),
        requested: requested_stream_index,
    })?;
    let stream_index = u32::try_from(selected_index)
        .map_err(|_| VideoPacketIdentityError::StreamIndexOverflow { index: selected_index })?;
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.video-packet-identity.v1");
    let mut packet_count = 0_u64;
    let mut payload_bytes = 0_u64;
    let mut first_packet_is_key = false;
    for (stream, packet) in input.packets() {
        if cancellation.is_canceled() {
            return Err(VideoPacketIdentityError::Canceled);
        }
        if stream.index() != selected_index {
            continue;
        }
        let data = packet.data().ok_or(VideoPacketIdentityError::MissingPacketPayload {
            stream_index,
            packet_index: packet_count,
        })?;
        if packet_count == 0 {
            first_packet_is_key = packet.is_key();
        }
        let packet_len = u64::try_from(data.len())
            .map_err(|_| VideoPacketIdentityError::PayloadSizeOverflow { stream_index })?;
        payload_bytes = payload_bytes
            .checked_add(packet_len)
            .ok_or(VideoPacketIdentityError::PayloadSizeOverflow { stream_index })?;
        hasher.update(packet_len.to_le_bytes());
        hasher.update(data);
        packet_count = packet_count.saturating_add(1);
    }
    if packet_count == 0 {
        return Err(VideoPacketIdentityError::EmptyVideoStream { stream_index });
    }
    if cancellation.is_canceled() {
        return Err(VideoPacketIdentityError::Canceled);
    }
    let after = MediaFileFingerprint::capture(path);
    if after != before || !after.authorizes_reuse() {
        return Err(VideoPacketIdentityError::SourceRevisionChanged { path: path.to_path_buf() });
    }
    Ok(VideoPacketIdentity {
        stream_index,
        packet_count,
        payload_bytes,
        payload_digest: hasher.finalize().into(),
        first_packet_is_key,
    })
}
