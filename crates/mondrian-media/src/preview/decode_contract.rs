//! Exact physical source and geometry contracts for Preview decode.
//!
//! These values deliberately exclude Asset identity, authoring interpretation,
//! scheduling priority, generation, and presentation demand. They describe only
//! the physical file/stream revision, exact source-local target, media color
//! conversion input, and the geometry/payload contract that the media Adapter
//! is allowed to execute.

use std::path::{Path, PathBuf};

use mondrian_core::{Resolution, SourceSampleTarget, SourceSamplingBoundary, TimelineTime};

use super::{MediaFileFingerprint, PreviewHardwareDecodeRequest, PreviewSourceColorContract};
use crate::info::{PixelFormat, VideoStreamInfo};
use crate::proxy::{
    ProxyArtifactManifest, ProxyEncodingProfile, PROXY_MANIFEST_VERSION,
    PROXY_PRIMARY_VIDEO_STREAM_INDEX,
};

/// Decoder-native surface family conservatively inferred from physical source evidence.
///
/// This is an admission hint, not proof that a decoder or renderer produced or
/// imported such a surface. Actual decoded frames retain their independent
/// [`crate::DecodedVideoSurfaceFormat`] evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewNativeSurfaceHint {
    /// An opaque 8-bit 4:2:0 source may decode to an NV12 hardware surface.
    Nv12,
    /// An opaque 10-bit 4:2:0 source may decode to a P010 hardware surface.
    P010,
}

/// Strength of physical Alpha evidence attached to one decode source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewDecodeAlphaPresence {
    /// Probe or generated-artifact evidence proves that the source is opaque.
    Opaque,
    /// Probe evidence proves that the source carries Alpha.
    Present,
    /// The caller has no complete sampling evidence; native output is forbidden.
    Unknown,
}

/// Exact selected absolute file path, revision, and physical video stream.
///
/// Construction requires a process-independent absolute path plus complete
/// filesystem object/change evidence. A path, length, or modification timestamp
/// alone can never become a reusable Preview decode identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewDecodeSource {
    path: PathBuf,
    fingerprint: MediaFileFingerprint,
    video_stream_index: u32,
    alpha_presence: PreviewDecodeAlphaPresence,
    native_surface_hint: Option<PreviewNativeSurfaceHint>,
}

impl PreviewDecodeSource {
    /// Capture and validate one original media source from an admitted video-stream probe.
    pub fn capture_probed_stream(
        path: impl Into<PathBuf>,
        stream: &VideoStreamInfo,
    ) -> Result<Self, PreviewDecodeContractError> {
        let path = path.into();
        let fingerprint = MediaFileFingerprint::capture(&path);
        Self::from_probed_stream(path, fingerprint, stream)
    }

    /// Build one original media source from an already captured exact revision and probe.
    pub fn from_probed_stream(
        path: impl Into<PathBuf>,
        fingerprint: MediaFileFingerprint,
        stream: &VideoStreamInfo,
    ) -> Result<Self, PreviewDecodeContractError> {
        if stream.width == 0 || stream.height == 0 {
            return Err(PreviewDecodeContractError::EmptySourceExtent {
                stream_index: stream.index,
                width: stream.width,
                height: stream.height,
            });
        }
        let sampling =
            stream
                .proven_sampling()
                .ok_or(PreviewDecodeContractError::UnprovenSourceSampling {
                    stream_index: stream.index,
                })?;
        let alpha_presence = if sampling.has_alpha {
            PreviewDecodeAlphaPresence::Present
        } else {
            PreviewDecodeAlphaPresence::Opaque
        };
        Self::new(
            path.into(),
            fingerprint,
            stream.index,
            alpha_presence,
            native_surface_hint_from_pixel_format(sampling.pixel_format),
        )
    }

    /// Capture and validate one generated proxy artifact under its exact manifest contract.
    ///
    /// Mondrian proxy publication maps the first selected picture to output
    /// stream zero. Its native surface hint comes from the artifact encoding
    /// profile, never from the original source probe.
    pub fn capture_proxy_artifact(
        path: impl Into<PathBuf>,
        manifest: &ProxyArtifactManifest,
    ) -> Result<Self, PreviewDecodeContractError> {
        let path = path.into();
        let fingerprint = MediaFileFingerprint::capture(&path);
        Self::from_proxy_artifact(path, fingerprint, manifest)
    }

    /// Build one generated proxy source from an already captured exact revision.
    pub fn from_proxy_artifact(
        path: impl Into<PathBuf>,
        fingerprint: MediaFileFingerprint,
        manifest: &ProxyArtifactManifest,
    ) -> Result<Self, PreviewDecodeContractError> {
        if manifest.version != PROXY_MANIFEST_VERSION {
            return Err(
                PreviewDecodeContractError::UnsupportedProxyManifestVersion {
                    actual: manifest.version,
                    expected: PROXY_MANIFEST_VERSION,
                },
            );
        }
        let native_surface_hint = match manifest.encoding {
            ProxyEncodingProfile::H264High8 => Some(PreviewNativeSurfaceHint::Nv12),
            ProxyEncodingProfile::H265Main10 => Some(PreviewNativeSurfaceHint::P010),
            ProxyEncodingProfile::DnxHrSq8 | ProxyEncodingProfile::DnxHrHqx10 => None,
        };
        Self::new(
            path.into(),
            fingerprint,
            PROXY_PRIMARY_VIDEO_STREAM_INDEX,
            PreviewDecodeAlphaPresence::Opaque,
            native_surface_hint,
        )
    }

    /// Build an exact CPU-only source when no complete sampling evidence was frozen.
    ///
    /// This constructor is intended for callers such as immutable Export
    /// snapshots that already froze path/revision/stream but deliberately do
    /// not claim native-surface eligibility.
    pub fn from_frozen_cpu_stream(
        path: impl Into<PathBuf>,
        fingerprint: MediaFileFingerprint,
        video_stream_index: u32,
    ) -> Result<Self, PreviewDecodeContractError> {
        Self::new(
            path.into(),
            fingerprint,
            video_stream_index,
            PreviewDecodeAlphaPresence::Unknown,
            None,
        )
    }

    fn new(
        path: PathBuf,
        fingerprint: MediaFileFingerprint,
        video_stream_index: u32,
        alpha_presence: PreviewDecodeAlphaPresence,
        native_surface_hint: Option<PreviewNativeSurfaceHint>,
    ) -> Result<Self, PreviewDecodeContractError> {
        if path.as_os_str().is_empty() {
            return Err(PreviewDecodeContractError::EmptySourcePath);
        }
        if !path.is_absolute() {
            return Err(PreviewDecodeContractError::RelativeSourcePath {
                path: path.display().to_string(),
            });
        }
        if !fingerprint.authorizes_reuse() {
            return Err(PreviewDecodeContractError::IncompleteSourceRevision {
                path: path.display().to_string(),
            });
        }
        Ok(Self {
            path,
            fingerprint,
            video_stream_index,
            alpha_presence,
            native_surface_hint,
        })
    }

    /// Selected physical file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Complete file revision that must still match at execution.
    pub const fn fingerprint(&self) -> MediaFileFingerprint {
        self.fingerprint
    }

    /// Absolute physical video stream selected from the container.
    pub const fn video_stream_index(&self) -> u32 {
        self.video_stream_index
    }

    /// Proven or explicitly unknown physical Alpha presence.
    pub const fn alpha_presence(&self) -> PreviewDecodeAlphaPresence {
        self.alpha_presence
    }

    /// Conservative native decoder surface family, when proven.
    pub const fn native_surface_hint(&self) -> Option<PreviewNativeSurfaceHint> {
        self.native_surface_hint
    }

    /// Whether this source is eligible to request an opaque native output.
    pub const fn permits_native_output(&self) -> bool {
        matches!(self.alpha_presence, PreviewDecodeAlphaPresence::Opaque)
            && self.native_surface_hint.is_some()
    }
}

/// Addressability required by the downstream Preview execution path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewDecodePayloadRequirement {
    /// The downstream processor must be able to address CPU pixels.
    CpuAddressable,
    /// A renderer-admitted native surface may be returned.
    NativeAllowed,
}

/// Canonical physical decode geometry and native-payload permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewDecodeGeometry {
    /// Produce CPU-addressable pixels fitting within this non-empty extent.
    FitWithin(Resolution),
    /// Permit a decoder-native surface while retaining the exact downstream
    /// materialization extent in the physical decode identity.
    NativeSource {
        /// Non-empty raster extent the renderer must materialize from the
        /// decoder-native surface. The decoder may retain its physical source
        /// extent; this value prevents Preview scale from disappearing at the
        /// native/renderer boundary.
        target: Resolution,
    },
}

impl PreviewDecodeGeometry {
    /// Resolve the only valid geometry for one payload requirement and hardware intent.
    pub fn canonical(
        source: &PreviewDecodeSource,
        requested_extent: Resolution,
        payload_requirement: PreviewDecodePayloadRequirement,
        hardware_request: PreviewHardwareDecodeRequest,
    ) -> Result<Self, PreviewDecodeContractError> {
        validate_extent(requested_extent)?;
        let native_requested = matches!(
            hardware_request,
            PreviewHardwareDecodeRequest::PreferGpuResident
                | PreviewHardwareDecodeRequest::RequireGpuResident
        );
        if payload_requirement == PreviewDecodePayloadRequirement::NativeAllowed
            && native_requested
            && source.permits_native_output()
        {
            return Ok(Self::NativeSource { target: requested_extent });
        }
        if hardware_request == PreviewHardwareDecodeRequest::RequireGpuResident {
            return Err(
                PreviewDecodeContractError::RequiredNativeOutputUnavailable {
                    alpha_presence: source.alpha_presence,
                    native_surface_hint: source.native_surface_hint,
                    payload_requirement,
                },
            );
        }
        Ok(Self::FitWithin(requested_extent))
    }

    /// Validate a directly constructed geometry against its physical source.
    pub fn validate_for(
        self,
        source: &PreviewDecodeSource,
    ) -> Result<(), PreviewDecodeContractError> {
        match self {
            Self::FitWithin(extent) => validate_extent(extent),
            Self::NativeSource { target } => {
                validate_extent(target)?;
                if source.permits_native_output() {
                    Ok(())
                } else {
                    Err(PreviewDecodeContractError::NativeSourceUnavailable {
                        alpha_presence: source.alpha_presence,
                        native_surface_hint: source.native_surface_hint,
                    })
                }
            }
        }
    }

    /// Legacy FFmpeg maximum dimensions represented by this exact geometry.
    pub const fn maximum_dimensions(self) -> (Option<u32>, Option<u32>) {
        match self {
            Self::FitWithin(extent) => (Some(extent.width), Some(extent.height)),
            Self::NativeSource { target } => (Some(target.width), Some(target.height)),
        }
    }

    /// Resolve the aspect-preserving materialization extent for a concrete
    /// decoded source raster.
    pub fn materialization_extent(self, source: Resolution) -> Resolution {
        let target = match self {
            Self::FitWithin(target) | Self::NativeSource { target } => target,
        };
        fit_within_extent(source, target)
    }
}

fn fit_within_extent(source: Resolution, target: Resolution) -> Resolution {
    let source_width = f64::from(source.width);
    let source_height = f64::from(source.height);
    let scale = (f64::from(target.width) / source_width)
        .min(f64::from(target.height) / source_height)
        .min(1.0);
    let mut width = (source_width * scale).round().max(1.0) as u32;
    let mut height = (source_height * scale).round().max(1.0) as u32;
    if width % 2 == 1 {
        width = width.saturating_sub(1).max(1);
    }
    if height % 2 == 1 {
        height = height.saturating_sub(1).max(1);
    }
    Resolution { width, height }
}

/// Exact cache/session-independent identity of one physical Preview decode.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewDecodeKey {
    source: PreviewDecodeSource,
    source_sample: SourceSampleTarget,
    geometry: PreviewDecodeGeometry,
    source_color: PreviewSourceColorContract,
}

impl PreviewDecodeKey {
    /// Build a validated exact physical Preview decode key.
    pub fn new(
        source: PreviewDecodeSource,
        source_sample: SourceSampleTarget,
        geometry: PreviewDecodeGeometry,
        source_color: PreviewSourceColorContract,
    ) -> Result<Self, PreviewDecodeContractError> {
        if source_sample.time().is_negative() {
            return Err(PreviewDecodeContractError::NegativeSourceTime {
                source_time: source_sample.time(),
            });
        }
        if source_sample.time().is_zero()
            && source_sample.boundary() == SourceSamplingBoundary::StrictPredecessor
        {
            return Err(PreviewDecodeContractError::SourceSampleBeforeOrigin {
                source_time: source_sample.time(),
                boundary: source_sample.boundary(),
            });
        }
        geometry.validate_for(&source)?;
        Ok(Self { source, source_sample, geometry, source_color })
    }

    /// Selected physical file and stream revision.
    pub const fn source(&self) -> &PreviewDecodeSource {
        &self.source
    }

    /// Exact media-source-local target.
    pub const fn source_sample(&self) -> SourceSampleTarget {
        self.source_sample
    }

    /// Canonical CPU/native decode geometry.
    pub const fn geometry(&self) -> PreviewDecodeGeometry {
        self.geometry
    }

    /// App-resolved source color/range facts used by media conversion.
    pub const fn source_color(&self) -> PreviewSourceColorContract {
        self.source_color
    }
}

/// Invalid or incomplete physical Preview decode contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreviewDecodeContractError {
    /// Empty paths cannot identify a physical source.
    #[error("Preview decode source path is empty")]
    EmptySourcePath,
    /// A reusable physical identity cannot depend on the process working directory.
    #[error("Preview decode source path is not absolute: {path}")]
    RelativeSourcePath {
        /// Relative path rejected by the contract.
        path: String,
    },
    /// The filesystem did not provide complete object/change evidence.
    #[error("Preview decode source revision is incomplete for {path}")]
    IncompleteSourceRevision {
        /// Source path whose revision was incomplete.
        path: String,
    },
    /// The probed stream had no non-empty physical raster.
    #[error("Preview video stream {stream_index} has an empty source extent {width}x{height}")]
    EmptySourceExtent {
        /// Absolute physical stream index.
        stream_index: u32,
        /// Invalid width.
        width: u32,
        /// Invalid height.
        height: u32,
    },
    /// Sampling fallbacks cannot authorize Alpha or native-surface behavior.
    #[error("Preview video stream {stream_index} has no proven sampling contract")]
    UnprovenSourceSampling {
        /// Absolute physical stream index.
        stream_index: u32,
    },
    /// Only the current generated-proxy manifest contract is executable.
    #[error("Preview proxy manifest version {actual} is unsupported; expected {expected}")]
    UnsupportedProxyManifestVersion {
        /// Observed manifest version.
        actual: u16,
        /// Current executable manifest version.
        expected: u16,
    },
    /// CPU geometry must be non-empty.
    #[error("Preview decode geometry is empty: {width}x{height}")]
    EmptyDecodeExtent {
        /// Invalid width.
        width: u32,
        /// Invalid height.
        height: u32,
    },
    /// Native geometry requires an opaque source with a supported surface hint.
    #[error(
        "source-native Preview geometry is unavailable for alpha={alpha_presence:?}, surface={native_surface_hint:?}"
    )]
    NativeSourceUnavailable {
        /// Physical Alpha evidence.
        alpha_presence: PreviewDecodeAlphaPresence,
        /// Physical native-surface hint.
        native_surface_hint: Option<PreviewNativeSurfaceHint>,
    },
    /// A required native request cannot be reconciled with source/output facts.
    #[error(
        "required native Preview output is unavailable for requirement={payload_requirement:?}, alpha={alpha_presence:?}, surface={native_surface_hint:?}"
    )]
    RequiredNativeOutputUnavailable {
        /// Physical Alpha evidence.
        alpha_presence: PreviewDecodeAlphaPresence,
        /// Physical native-surface hint.
        native_surface_hint: Option<PreviewNativeSurfaceHint>,
        /// Downstream payload requirement.
        payload_requirement: PreviewDecodePayloadRequirement,
    },
    /// Physical decode targets are nonnegative.
    #[error("Preview source target is negative: {source_time}")]
    NegativeSourceTime {
        /// Invalid source-local target.
        source_time: TimelineTime,
    },
    /// A strict-predecessor request at source origin has no physical sample.
    #[error("Preview source sample precedes origin: time={source_time}, boundary={boundary:?}")]
    SourceSampleBeforeOrigin {
        /// Exact source-local boundary.
        source_time: TimelineTime,
        /// Sampling boundary that selected the missing predecessor.
        boundary: SourceSamplingBoundary,
    },
}

fn validate_extent(extent: Resolution) -> Result<(), PreviewDecodeContractError> {
    if extent.width == 0 || extent.height == 0 {
        return Err(PreviewDecodeContractError::EmptyDecodeExtent {
            width: extent.width,
            height: extent.height,
        });
    }
    Ok(())
}

const fn native_surface_hint_from_pixel_format(
    pixel_format: PixelFormat,
) -> Option<PreviewNativeSurfaceHint> {
    match pixel_format {
        PixelFormat::Yuv420p | PixelFormat::Nv12 => Some(PreviewNativeSurfaceHint::Nv12),
        PixelFormat::Yuv420p10le | PixelFormat::P010 => Some(PreviewNativeSurfaceHint::P010),
        PixelFormat::Yuv422p
        | PixelFormat::Yuv444p
        | PixelFormat::Yuv422p10le
        | PixelFormat::Yuv444p10le
        | PixelFormat::Rgb24
        | PixelFormat::Rgba => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use mondrian_core::{
        ColorSpace, DetectedColorInterpretation, MediaFileChangeStamp, MediaFileObjectIdentity,
        Rational, VideoCodec, VideoCodecProfile,
    };

    use super::*;
    use crate::proxy::{
        ProxyArtifactSettings, ProxyColorContract, ProxyResolution, ProxySourceFingerprint,
    };
    use crate::DecodedVideoRange;

    fn exact_fingerprint(seed: u64) -> MediaFileFingerprint {
        MediaFileFingerprint {
            len: Some(seed.saturating_add(1)),
            modified_secs: Some(seed.saturating_add(2)),
            modified_nanos: Some((seed % u64::from(u32::MAX)) as u32),
            object_identity: Some(MediaFileObjectIdentity::Unix {
                device: seed.saturating_add(3),
                inode: seed.saturating_add(4),
            }),
            change_stamp: Some(MediaFileChangeStamp::Unix {
                seconds: seed as i64,
                nanoseconds: seed.saturating_add(5) as i64,
            }),
        }
    }

    fn absolute_test_path(label: impl AsRef<std::path::Path>) -> PathBuf {
        std::env::temp_dir().join("mondrian-preview-decode-contract").join(label)
    }

    fn video_stream(
        index: u32,
        pixel_format: PixelFormat,
        pixel_format_proven: bool,
    ) -> VideoStreamInfo {
        VideoStreamInfo {
            index,
            codec: VideoCodec::H265,
            duration: Some(Duration::from_secs(1)),
            codec_profile: VideoCodecProfile::HevcMain10,
            width: 3840,
            height: 2160,
            frame_rate: Rational::new(25, 1),
            frame_rate_proven: true,
            pixel_format,
            pixel_format_proven,
            color_range: DecodedVideoRange::Limited,
            color_interpretation: DetectedColorInterpretation::decoder_unavailable(),
            color_metadata: None,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            bit_depth: pixel_format.bit_depth(),
            has_alpha: pixel_format.has_alpha(),
            avg_bitrate: 1,
            total_frames: Some(25),
        }
    }

    fn proxy_manifest(encoding: ProxyEncodingProfile) -> ProxyArtifactManifest {
        ProxyArtifactManifest {
            version: PROXY_MANIFEST_VERSION,
            source: ProxySourceFingerprint {
                len: Some(1),
                modified_secs: Some(2),
                modified_nanos: Some(3),
                object_identity: Some(MediaFileObjectIdentity::Unix { device: 4, inode: 5 }),
                change_stamp: Some(MediaFileChangeStamp::Unix { seconds: 6, nanoseconds: 7 }),
            },
            color: ProxyColorContract::try_new(ColorSpace::Rec709, 8, DecodedVideoRange::Limited)
                .expect("valid proxy color"),
            settings: ProxyArtifactSettings { resolution: ProxyResolution::P720, crf: 20 },
            encoding,
        }
    }

    fn source_color() -> PreviewSourceColorContract {
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
    }

    #[test]
    fn probed_source_preserves_absolute_stream_and_sampling_hint() {
        let stream = video_stream(7, PixelFormat::P010, true);
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(10),
            &stream,
        )
        .expect("valid probed source");

        assert_eq!(source.video_stream_index(), 7);
        assert_eq!(
            source.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::P010)
        );
        assert_eq!(source.alpha_presence(), PreviewDecodeAlphaPresence::Opaque);
    }

    #[test]
    fn unproven_sampling_incomplete_revision_and_relative_path_are_rejected() {
        let unproven = video_stream(2, PixelFormat::P010, false);
        assert!(matches!(
            PreviewDecodeSource::from_probed_stream(
                absolute_test_path("media/source.mov"),
                exact_fingerprint(11),
                &unproven,
            ),
            Err(PreviewDecodeContractError::UnprovenSourceSampling { stream_index: 2 })
        ));

        assert!(matches!(
            PreviewDecodeSource::from_frozen_cpu_stream(
                absolute_test_path("media/source.mov"),
                MediaFileFingerprint::default(),
                2,
            ),
            Err(PreviewDecodeContractError::IncompleteSourceRevision { .. })
        ));

        assert!(matches!(
            PreviewDecodeSource::from_frozen_cpu_stream(
                "relative/source.mov",
                exact_fingerprint(12),
                2,
            ),
            Err(PreviewDecodeContractError::RelativeSourcePath { .. })
        ));
    }

    #[test]
    fn proxy_uses_output_stream_zero_and_profile_surface_instead_of_source_facts() {
        let original = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(12),
            &video_stream(7, PixelFormat::P010, true),
        )
        .expect("valid original source");
        let proxy = PreviewDecodeSource::from_proxy_artifact(
            absolute_test_path("cache/proxy.mp4"),
            exact_fingerprint(13),
            &proxy_manifest(ProxyEncodingProfile::H264High8),
        )
        .expect("valid proxy source");

        assert_eq!(original.video_stream_index(), 7);
        assert_eq!(
            original.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::P010)
        );
        assert_eq!(proxy.video_stream_index(), 0);
        assert_eq!(
            proxy.native_surface_hint(),
            Some(PreviewNativeSurfaceHint::Nv12)
        );
    }

    #[test]
    fn every_proxy_profile_has_one_explicit_native_surface_projection() {
        let cases = [
            (
                ProxyEncodingProfile::H264High8,
                Some(PreviewNativeSurfaceHint::Nv12),
            ),
            (
                ProxyEncodingProfile::H265Main10,
                Some(PreviewNativeSurfaceHint::P010),
            ),
            (ProxyEncodingProfile::DnxHrSq8, None),
            (ProxyEncodingProfile::DnxHrHqx10, None),
        ];
        for (index, (encoding, expected)) in cases.into_iter().enumerate() {
            let source = PreviewDecodeSource::from_proxy_artifact(
                absolute_test_path(format!("cache/proxy-{index}.mov")),
                exact_fingerprint(index as u64 + 20),
                &proxy_manifest(encoding),
            )
            .expect("valid proxy source");
            assert_eq!(source.video_stream_index(), 0);
            assert_eq!(source.native_surface_hint(), expected);
            assert_eq!(source.alpha_presence(), PreviewDecodeAlphaPresence::Opaque);
        }
    }

    #[test]
    fn canonical_geometry_separates_cpu_and_native_output_contracts() {
        let native_source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(30),
            &video_stream(0, PixelFormat::P010, true),
        )
        .expect("valid native source");
        let requested = Resolution { width: 960, height: 540 };

        assert_eq!(
            PreviewDecodeGeometry::canonical(
                &native_source,
                requested,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferGpuResident,
            )
            .expect("native geometry"),
            PreviewDecodeGeometry::NativeSource { target: requested }
        );
        let native = PreviewDecodeGeometry::NativeSource { target: requested };
        assert_eq!(native.maximum_dimensions(), (Some(960), Some(540)));
        assert_eq!(
            native.materialization_extent(Resolution { width: 4096, height: 2160 }),
            Resolution { width: 960, height: 506 }
        );
        assert_eq!(
            PreviewDecodeGeometry::canonical(
                &native_source,
                requested,
                PreviewDecodePayloadRequirement::CpuAddressable,
                PreviewHardwareDecodeRequest::PreferGpuResident,
            )
            .expect("CPU geometry"),
            PreviewDecodeGeometry::FitWithin(requested)
        );

        let alpha_source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/alpha.mov"),
            exact_fingerprint(31),
            &video_stream(0, PixelFormat::Rgba, true),
        )
        .expect("valid alpha source");
        assert_eq!(
            PreviewDecodeGeometry::canonical(
                &alpha_source,
                requested,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::PreferGpuResident,
            )
            .expect("preferred native may safely downgrade"),
            PreviewDecodeGeometry::FitWithin(requested)
        );
        assert!(matches!(
            PreviewDecodeGeometry::canonical(
                &alpha_source,
                requested,
                PreviewDecodePayloadRequirement::NativeAllowed,
                PreviewHardwareDecodeRequest::RequireGpuResident,
            ),
            Err(PreviewDecodeContractError::RequiredNativeOutputUnavailable { .. })
        ));
    }

    #[test]
    fn key_rejects_negative_time_and_legacy_request_projection_is_exact() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(40),
            &video_stream(5, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        let geometry = PreviewDecodeGeometry::FitWithin(Resolution { width: 1280, height: 720 });
        assert!(matches!(
            PreviewDecodeKey::new(
                source.clone(),
                SourceSampleTarget::covering(
                    TimelineTime::new(-1, 1).expect("valid negative rational"),
                ),
                geometry,
                source_color(),
            ),
            Err(PreviewDecodeContractError::NegativeSourceTime { .. })
        ));

        let key = PreviewDecodeKey::new(
            source,
            SourceSampleTarget::covering(TimelineTime::new(1, 2).expect("valid source time")),
            geometry,
            source_color(),
        )
        .expect("valid decode key");
        let request = super::super::PreviewDecodeRequest::from_key(
            &key,
            super::super::PreviewDecodeAccessMode::PlaybackCursor,
        );

        assert_eq!(request.path, key.source().path());
        assert_eq!(
            request.video_stream_index,
            Some(key.source().video_stream_index())
        );
        assert_eq!(request.fingerprint, Some(key.source().fingerprint()));
        assert_eq!(request.source_sample, key.source_sample());
        assert_eq!(request.max_width, Some(1280));
        assert_eq!(request.max_height, Some(720));
        assert_eq!(request.source_color, key.source_color());
    }

    #[test]
    fn key_identity_distinguishes_covering_and_strict_predecessor_targets() {
        let source = PreviewDecodeSource::from_probed_stream(
            absolute_test_path("media/source.mov"),
            exact_fingerprint(41),
            &video_stream(5, PixelFormat::Yuv420p, true),
        )
        .expect("valid source");
        let geometry = PreviewDecodeGeometry::FitWithin(Resolution { width: 1280, height: 720 });
        let source_time = TimelineTime::new(1, 1).expect("valid source time");
        let covering = PreviewDecodeKey::new(
            source.clone(),
            SourceSampleTarget::covering(source_time),
            geometry,
            source_color(),
        )
        .expect("covering key");
        let strict = PreviewDecodeKey::new(
            source.clone(),
            SourceSampleTarget::strict_predecessor(source_time),
            geometry,
            source_color(),
        )
        .expect("strict predecessor key");

        assert_ne!(covering, strict);
        assert!(matches!(
            PreviewDecodeKey::new(
                source,
                SourceSampleTarget::strict_predecessor(TimelineTime::ZERO),
                geometry,
                source_color(),
            ),
            Err(PreviewDecodeContractError::SourceSampleBeforeOrigin { .. })
        ));
    }
}
