//! 代理文件生成器（Proxy）
//!
//! 后台将高码率原始素材转码为低码率代理文件，用于编辑时的流畅预览。
//! 导出时自动切换回原始文件。

use crate::{
    DecodedVideoRange, MediaFileChangeStamp, MediaFileFingerprint, MediaFileObjectIdentity,
    SupervisedProcessPolicy, SupervisedStreamCapture,
};
use mondrian_core::{
    types::AssetId, types::ColorSpace, ExecutionCancellationToken, MondrianError, Result,
};
use mondrian_storage::{FilePublicationFailure, FilePublicationMode, OwnedPublicationFile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// 代理分辨率预设
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyResolution {
    P360,
    P480,
    P720,
    P1080,
}

impl ProxyResolution {
    pub fn height(self) -> u32 {
        match self {
            Self::P360 => 360,
            Self::P480 => 480,
            Self::P720 => 720,
            Self::P1080 => 1080,
        }
    }
}

/// 代理编码格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyCodec {
    /// Select H.264 for ordinary 8-bit SDR and H.265 Main10 for HDR, Log, or high-bit sources.
    Auto,
    /// Force an 8-bit H.264 proxy. Incompatible source contracts are rejected.
    H264,
    /// Force a 10-bit H.265 Main10 proxy.
    H265Main10,
    /// Force an edit-friendly DNxHR proxy in a MOV container.
    DnxHr,
}

/// 代理生成配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProxyConfig {
    pub resolution: ProxyResolution,
    pub codec: ProxyCodec,
    /// CRF 质量（H.264: 0-51，越小质量越高）
    pub crf: u8,
    /// 并行转码任务数
    pub concurrent_jobs: u8,
    /// 代理文件存储根目录（默认 ~/.mondrian/proxy）
    pub cache_dir: PathBuf,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            resolution: ProxyResolution::P720,
            codec: ProxyCodec::Auto,
            crf: 23,
            concurrent_jobs: 2,
            cache_dir: default_proxy_dir(),
        }
    }
}

impl ProxyConfig {
    /// Freeze the cache namespace as one normalized absolute root.
    ///
    /// Admission code must call this before the config participates in demand
    /// identity, concurrency accounting, or artifact planning.
    pub fn freeze_cache_root(mut self) -> Result<Self> {
        self.cache_dir = std::path::absolute(&self.cache_dir).map_err(|error| {
            MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "proxy cache root could not be made absolute ({}): {error}",
                    self.cache_dir.display()
                ),
            }
        })?;
        Ok(self)
    }
}

const PROXY_COLOR_CONTRACT_VERSION: u16 = 2;
pub(crate) const PROXY_MANIFEST_VERSION: u16 = 3;
/// Generated proxy commands map their sole picture stream before optional audio.
///
/// `PreviewDecodeSource::from_proxy_artifact` consumes this same contract so a
/// proxy can never inherit the original container's absolute stream index.
pub(crate) const PROXY_PRIMARY_VIDEO_STREAM_INDEX: u32 = 0;

/// Invalid source sampling metadata for proxy generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProxyColorContractError {
    /// Ingest did not resolve the encoded video range.
    #[error("proxy generation requires an explicit source video range")]
    UnknownSourceRange,
    /// The reported component precision cannot be represented by supported proxy encoders.
    #[error("unsupported proxy source bit depth: {0}")]
    UnsupportedSourceBitDepth(u8),
}

/// Color identity that a generated proxy must preserve.
///
/// Proxies remain source-referred optimized media. Working-space and display
/// transforms are deliberately excluded because they belong to render and
/// presentation boundaries, not derived source media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProxyColorContract {
    /// Contract schema version used in persistent proxy identity.
    version: u16,
    /// Effective source/input color space resolved by the application.
    source_color_space: ColorSpace,
    /// Nominal source component precision reported by ingest.
    source_bit_depth: u8,
    /// Encoded video range when the decoder reported it reliably.
    source_range: DecodedVideoRange,
}

impl ProxyColorContract {
    /// Build the current proxy color contract for an ingested video stream.
    pub fn try_new(
        source_color_space: ColorSpace,
        source_bit_depth: u8,
        source_range: DecodedVideoRange,
    ) -> std::result::Result<Self, ProxyColorContractError> {
        let contract = Self {
            version: PROXY_COLOR_CONTRACT_VERSION,
            source_color_space,
            source_bit_depth,
            source_range,
        };
        contract.validate()?;
        Ok(contract)
    }

    /// Effective encoded source color space preserved by the artifact.
    pub fn source_color_space(self) -> ColorSpace {
        self.source_color_space
    }

    /// Nominal source component precision resolved by ingest.
    pub fn source_bit_depth(self) -> u8 {
        self.source_bit_depth
    }

    /// Explicit encoded source range preserved by the artifact.
    pub fn source_range(self) -> DecodedVideoRange {
        self.source_range
    }

    fn validate(self) -> std::result::Result<(), ProxyColorContractError> {
        if self.source_range == DecodedVideoRange::Unknown {
            return Err(ProxyColorContractError::UnknownSourceRange);
        }
        if !(8..=16).contains(&self.source_bit_depth) {
            return Err(ProxyColorContractError::UnsupportedSourceBitDepth(
                self.source_bit_depth,
            ));
        }
        Ok(())
    }

    fn needs_high_precision(self) -> bool {
        self.source_bit_depth > 8
            || self.source_color_space.is_hdr()
            || self.source_color_space.is_scene_linear()
            || self.source_color_space.encoding().is_scene_log()
    }
}

/// Concrete encoder/pixel-format contract selected for one proxy artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyEncodingProfile {
    /// H.264 High, 8-bit 4:2:0.
    H264High8,
    /// H.265 Main10, 10-bit 4:2:0.
    H265Main10,
    /// DNxHR SQ, 8-bit 4:2:2.
    DnxHrSq8,
    /// DNxHR HQX, 10-bit 4:2:2.
    DnxHrHqx10,
}

impl ProxyEncodingProfile {
    fn output_extension(self) -> &'static str {
        match self {
            Self::H264High8 | Self::H265Main10 => "mp4",
            Self::DnxHrSq8 | Self::DnxHrHqx10 => "mov",
        }
    }

    fn output_container(self) -> &'static str {
        match self {
            Self::H264High8 | Self::H265Main10 => "mp4",
            Self::DnxHrSq8 | Self::DnxHrHqx10 => "mov",
        }
    }

    fn identity_label(self) -> &'static str {
        match self {
            Self::H264High8 => "h264-high8",
            Self::H265Main10 => "h265-main10",
            Self::DnxHrSq8 => "dnxhr-sq8",
            Self::DnxHrHqx10 => "dnxhr-hqx10",
        }
    }
}

/// Bounded source file-revision evidence persisted in a proxy manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySourceFingerprint {
    /// Source file length in bytes.
    pub len: Option<u64>,
    /// Source modification time in Unix seconds.
    pub modified_secs: Option<u64>,
    /// Source modification time nanosecond fraction.
    pub modified_nanos: Option<u32>,
    /// Filesystem object identity of the admitted source.
    pub object_identity: Option<MediaFileObjectIdentity>,
    /// Filesystem-owned change generation of that object.
    pub change_stamp: Option<MediaFileChangeStamp>,
}

impl From<MediaFileFingerprint> for ProxySourceFingerprint {
    fn from(value: MediaFileFingerprint) -> Self {
        Self {
            len: value.len,
            modified_secs: value.modified_secs,
            modified_nanos: value.modified_nanos,
            object_identity: value.object_identity,
            change_stamp: value.change_stamp,
        }
    }
}

/// Artifact-affecting proxy configuration persisted in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyArtifactSettings {
    /// Spatial proxy resolution preset.
    pub resolution: ProxyResolution,
    /// Effective bounded CRF used by inter-frame encoders.
    pub crf: u8,
}

/// Versioned sidecar contract proving the identity of a proxy artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyArtifactManifest {
    /// Manifest schema version.
    pub version: u16,
    /// Source file fingerprint held stable across generation.
    pub source: ProxySourceFingerprint,
    /// Source-referred color contract preserved by the proxy.
    pub color: ProxyColorContract,
    /// Artifact-affecting proxy settings.
    pub settings: ProxyArtifactSettings,
    /// Concrete encoder and pixel format used.
    pub encoding: ProxyEncodingProfile,
}

fn default_proxy_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    home.join(".mondrian").join("proxy")
}

/// 代理生成任务进度
#[derive(Debug, Clone)]
pub struct ProxyProgress {
    pub asset_id: AssetId,
    pub progress: f32, // 0.0 ~ 1.0
    pub is_done: bool,
    pub error: Option<String>,
}

/// Publication phase that failed while committing one proxy artifact pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyPublicationPhase {
    /// Encoded media artifact publication.
    Media,
    /// Exact sidecar manifest publication.
    Manifest,
}

impl ProxyPublicationPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Manifest => "manifest",
        }
    }
}

/// Namespace/durability classification of a proxy publication failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyPublicationFailureKind {
    /// No irreversible namespace operation is known to have completed.
    BeforeNamespace,
    /// The target names the intended bytes, but crash durability is not proven.
    DurabilityUnconfirmed,
    /// Target ownership cannot be proven from postconditions.
    NamespaceIndeterminate,
}

/// Domain-owned proxy publication failure without storage implementation types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyPublicationFailure {
    /// Artifact phase that failed.
    pub phase: ProxyPublicationPhase,
    /// Exhaustive namespace/durability classification.
    pub kind: ProxyPublicationFailureKind,
    /// Intended absolute target path.
    pub target_path: PathBuf,
    /// Verified surviving source name for new bytes, when available.
    pub retained_new_path: Option<PathBuf>,
    /// Human-readable implementation diagnostic.
    pub detail: String,
}

impl fmt::Display for ProxyPublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "proxy {} publication {:?} at {}: {}",
            self.phase.label(),
            self.kind,
            self.target_path.display(),
            self.detail
        )?;
        if let Some(path) = &self.retained_new_path {
            write!(formatter, " (verified retained bytes: {})", path.display())?;
        }
        if matches!(
            self.kind,
            ProxyPublicationFailureKind::DurabilityUnconfirmed
                | ProxyPublicationFailureKind::NamespaceIndeterminate
        ) {
            write!(
                formatter,
                "; publication is quarantined pending exact artifact-pair revalidation"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ProxyPublicationFailure {}

/// Durable evidence that both proxy media and its exact sidecar are published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyPublicationEvidence {
    /// Absolute published media path.
    pub media_path: PathBuf,
    /// Absolute published sidecar path.
    pub manifest_path: PathBuf,
}

impl ProxyPublicationEvidence {
    /// Construct evidence after both storage publications returned durable success.
    pub fn durable(media_path: PathBuf, manifest_path: PathBuf) -> Self {
        Self { media_path, manifest_path }
    }
}

/// Typed proxy execution failure.
#[derive(Debug, thiserror::Error)]
pub enum ProxyGenerationError {
    /// Generation failed outside namespace publication.
    #[error(transparent)]
    Execution(#[from] MondrianError),
    /// Artifact publication reached a classified terminal state.
    #[error(transparent)]
    Publication(#[from] ProxyPublicationFailure),
}

/// Terminal result of one cancellable proxy-generation execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyGenerationOutcome {
    /// A new artifact and matching manifest were durably published.
    Completed(ProxyPublicationEvidence),
    /// The exact artifact was already fresh and no transcode ran.
    Reused(PathBuf),
    /// Cooperative cancellation won before artifact publication.
    Canceled,
}

/// Freshness state for a project's expected proxy media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyStatus {
    /// No proxy file exists at the configured proxy path.
    Missing,
    /// Proxy media and sidecar exactly match the expected source and color contract.
    Fresh,
    /// The proxy file exists but should not be used for playback.
    Stale,
}

impl ProxyStatus {
    /// Returns true when preview playback may decode the proxy file.
    pub fn is_fresh(self) -> bool {
        self == Self::Fresh
    }
}

const STABLE_PROXY_HASH_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const STABLE_PROXY_HASH_PRIME: u64 = 0x0000_0100_0000_01b3;
const PROXY_SOURCE_PATH_IDENTITY_DOMAIN_V1: &[u8] = b"mondrian.proxy.source-path.v1";

fn extend_stable_proxy_hash(seed: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(STABLE_PROXY_HASH_PRIME)
    })
}

fn stable_proxy_hash(bytes: &[u8]) -> u64 {
    extend_stable_proxy_hash(STABLE_PROXY_HASH_OFFSET_BASIS, bytes)
}

#[cfg(unix)]
fn stable_proxy_source_path_hash(source_path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;

    let hash = extend_stable_proxy_hash(
        STABLE_PROXY_HASH_OFFSET_BASIS,
        PROXY_SOURCE_PATH_IDENTITY_DOMAIN_V1,
    );
    let hash = extend_stable_proxy_hash(hash, b"\0unix-bytes\0");
    Ok(extend_stable_proxy_hash(
        hash,
        source_path.as_os_str().as_bytes(),
    ))
}

#[cfg(windows)]
fn stable_proxy_source_path_hash(source_path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    let mut hash = extend_stable_proxy_hash(
        STABLE_PROXY_HASH_OFFSET_BASIS,
        PROXY_SOURCE_PATH_IDENTITY_DOMAIN_V1,
    );
    hash = extend_stable_proxy_hash(hash, b"\0windows-utf16le\0");
    for unit in source_path.as_os_str().encode_wide() {
        hash = extend_stable_proxy_hash(hash, &unit.to_le_bytes());
    }
    Ok(hash)
}

#[cfg(not(any(unix, windows)))]
fn stable_proxy_source_path_hash(source_path: &Path) -> Result<u64> {
    let source_path =
        source_path
            .as_os_str()
            .to_str()
            .ok_or_else(|| {
                MondrianError::ProxyGenerationFailed {
            reason:
                "proxy source path identity is unsupported for non-Unicode paths on this platform"
                    .to_owned(),
        }
            })?;
    let hash = extend_stable_proxy_hash(
        STABLE_PROXY_HASH_OFFSET_BASIS,
        PROXY_SOURCE_PATH_IDENTITY_DOMAIN_V1,
    );
    let hash = extend_stable_proxy_hash(hash, b"\0other-utf8\0");
    Ok(extend_stable_proxy_hash(hash, source_path.as_bytes()))
}

fn canonical_proxy_source_identity(source_path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(source_path) {
        Ok(path) => Ok(path),
        // `proxy_path` is also a pure planning seam used before a source has
        // materialized. Preserve that use while requiring every observable
        // physical source alias to converge on its canonical identity.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(source_path.to_path_buf()),
        Err(error) => Err(MondrianError::ProxyGenerationFailed {
            reason: format!(
                "canonical proxy source identity unavailable ({}): {error}",
                source_path.display()
            ),
        }),
    }
}

fn proxy_manifest_path(proxy_path: &Path) -> PathBuf {
    proxy_path.with_extension(format!(
        "{}.color.json",
        proxy_path.extension().and_then(|value| value.to_str()).unwrap_or("proxy")
    ))
}

fn write_proxy_manifest(
    manifest: &ProxyArtifactManifest,
    output_path: &Path,
) -> std::result::Result<(), ProxyGenerationError> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|err| {
        ProxyGenerationError::Execution(MondrianError::ProxyGenerationFailed {
            reason: format!("proxy manifest serialization failed: {err}"),
        })
    })?;
    let mut staging =
        OwnedPublicationFile::create_sibling(output_path, "proxy-manifest").map_err(|error| {
            proxy_before_namespace_failure(ProxyPublicationPhase::Manifest, output_path, error)
        })?;
    staging
        .file_mut()
        .and_then(|file| file.write_all(&bytes).map_err(Into::into))
        .map_err(|error| {
            proxy_before_namespace_failure(ProxyPublicationPhase::Manifest, output_path, error)
        })?;
    publish_proxy_staging(staging, ProxyPublicationPhase::Manifest, output_path)
}

fn publish_proxy_staging(
    staging: OwnedPublicationFile,
    phase: ProxyPublicationPhase,
    output_path: &Path,
) -> std::result::Result<(), ProxyGenerationError> {
    let evidence = staging
        .publish(FilePublicationMode::ReplaceExisting)
        .map_err(|failure| proxy_publication_failure(phase, output_path, failure))?;
    debug_assert_eq!(evidence.published_path(), output_path);
    Ok(())
}

fn proxy_before_namespace_failure(
    phase: ProxyPublicationPhase,
    output_path: &Path,
    error: impl std::fmt::Display,
) -> ProxyGenerationError {
    ProxyPublicationFailure {
        phase,
        kind: ProxyPublicationFailureKind::BeforeNamespace,
        target_path: output_path.to_path_buf(),
        retained_new_path: None,
        detail: format!("staging failed before namespace publication: {error}"),
    }
    .into()
}

fn proxy_publication_failure(
    phase: ProxyPublicationPhase,
    output_path: &Path,
    failure: FilePublicationFailure,
) -> ProxyGenerationError {
    let (kind, retained_new_path) = match &failure {
        FilePublicationFailure::BeforeNamespace(_) => {
            (ProxyPublicationFailureKind::BeforeNamespace, None)
        }
        FilePublicationFailure::DurabilityUnconfirmed(_) => {
            (ProxyPublicationFailureKind::DurabilityUnconfirmed, None)
        }
        FilePublicationFailure::NamespaceIndeterminate(evidence) => (
            ProxyPublicationFailureKind::NamespaceIndeterminate,
            evidence.retained_new_path().map(Path::to_path_buf),
        ),
    };
    ProxyPublicationFailure {
        phase,
        kind,
        target_path: output_path.to_path_buf(),
        retained_new_path,
        detail: failure.to_string(),
    }
    .into()
}

/// 代理文件生成器
pub struct ProxyGenerator {
    config: ProxyConfig,
}

impl ProxyGenerator {
    pub fn new(config: ProxyConfig) -> Self {
        Self { config }
    }

    /// Resolve the concrete encoding profile for a source color contract.
    pub fn encoding_profile(&self, color: ProxyColorContract) -> Result<ProxyEncodingProfile> {
        color.validate().map_err(proxy_color_contract_error)?;
        let high_precision = color.needs_high_precision();
        match (self.config.codec, high_precision) {
            (ProxyCodec::Auto, false) => Ok(ProxyEncodingProfile::H264High8),
            (ProxyCodec::Auto, true) => Ok(ProxyEncodingProfile::H265Main10),
            (ProxyCodec::H264, false) => Ok(ProxyEncodingProfile::H264High8),
            (ProxyCodec::H264, true) => Err(MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "H.264 8-bit proxy cannot preserve {:?} {}-bit source; select Auto, H.265 Main10, or DNxHR",
                    color.source_color_space, color.source_bit_depth
                ),
            }),
            (ProxyCodec::H265Main10, _) => Ok(ProxyEncodingProfile::H265Main10),
            (ProxyCodec::DnxHr, false) => Ok(ProxyEncodingProfile::DnxHrSq8),
            (ProxyCodec::DnxHr, true) => Ok(ProxyEncodingProfile::DnxHrHqx10),
        }
    }

    /// 计算指定素材的代理文件路径
    pub fn proxy_path(&self, source_path: &Path, color: ProxyColorContract) -> Result<PathBuf> {
        let cache_root = self.config.clone().freeze_cache_root()?.cache_dir;
        let encoding = self.encoding_profile(color)?;
        let canonical_source = canonical_proxy_source_identity(source_path)?;
        let source_hash = stable_proxy_source_path_hash(&canonical_source)?;
        let identity = ProxyArtifactManifest {
            version: PROXY_MANIFEST_VERSION,
            source: ProxySourceFingerprint {
                len: None,
                modified_secs: None,
                modified_nanos: None,
                object_identity: None,
                change_stamp: None,
            },
            color,
            settings: self.artifact_settings(),
            encoding,
        };
        let identity_bytes =
            serde_json::to_vec(&identity).map_err(|err| MondrianError::ProxyGenerationFailed {
                reason: format!("proxy identity serialization failed: {err}"),
            })?;
        let contract_hash = stable_proxy_hash(&identity_bytes);
        let height = self.config.resolution.height();
        Ok(cache_root.join(format!(
            "{source_hash:016x}_{contract_hash:016x}_{height}p_{}.{}",
            encoding.identity_label(),
            encoding.output_extension()
        )))
    }

    fn artifact_settings(&self) -> ProxyArtifactSettings {
        ProxyArtifactSettings {
            resolution: self.config.resolution,
            crf: self.config.crf.min(51),
        }
    }

    /// Build the manifest expected for the current source and generator settings.
    pub fn expected_manifest(
        &self,
        source_path: &Path,
        color: ProxyColorContract,
    ) -> Result<ProxyArtifactManifest> {
        let source_revision = MediaFileFingerprint::capture(source_path);
        require_proxy_source_revision(source_path, source_revision)?;
        Ok(ProxyArtifactManifest {
            version: PROXY_MANIFEST_VERSION,
            source: source_revision.into(),
            color,
            settings: self.artifact_settings(),
            encoding: self.encoding_profile(color)?,
        })
    }

    /// Return the sidecar manifest path for a proxy media path.
    pub fn manifest_path(proxy_path: &Path) -> PathBuf {
        proxy_manifest_path(proxy_path)
    }

    #[cfg(test)]
    pub(crate) fn install_test_manifest(
        &self,
        source_path: &Path,
        color: ProxyColorContract,
    ) -> PathBuf {
        let proxy_path = self.proxy_path(source_path, color).expect("test proxy path");
        let manifest = self.expected_manifest(source_path, color).expect("test manifest");
        let manifest_path = proxy_manifest_path(&proxy_path);
        let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize test manifest");
        std::fs::write(manifest_path, bytes).expect("write test manifest");
        proxy_path
    }

    /// 检查代理文件是否存在。
    ///
    /// This intentionally reports existence only. Use [`Self::proxy_status`]
    /// or [`Self::proxy_is_fresh`] for preview/playback scheduling.
    pub fn proxy_exists(&self, source_path: &Path, color: ProxyColorContract) -> bool {
        self.proxy_path(source_path, color).is_ok_and(|path| path.exists())
    }

    /// Returns the freshness state for the configured proxy of `source_path`.
    pub fn proxy_status(&self, source_path: &Path, color: ProxyColorContract) -> ProxyStatus {
        self.proxy_status_for_source_fingerprint(
            source_path,
            MediaFileFingerprint::capture(source_path),
            color,
        )
        .unwrap_or(ProxyStatus::Stale)
    }

    /// Resolve freshness against the exact source revision admitted by an
    /// application scheduler. Source drift is an error, not a stale/fresh guess.
    pub fn proxy_status_for_source_fingerprint(
        &self,
        source_path: &Path,
        admitted_source: MediaFileFingerprint,
        color: ProxyColorContract,
    ) -> Result<ProxyStatus> {
        require_proxy_source_revision(source_path, admitted_source)?;
        let proxy_path = self.proxy_path(source_path, color)?;
        if !proxy_path.exists() {
            return Ok(ProxyStatus::Missing);
        }
        let expected = self.expected_manifest(source_path, color)?;
        if expected.source != admitted_source.into() {
            return Err(MondrianError::ProxyGenerationFailed {
                reason: "source file changed while resolving exact proxy freshness".to_owned(),
            });
        }
        let manifest_path = proxy_manifest_path(&proxy_path);
        let Ok(bytes) = std::fs::read(manifest_path) else {
            return Ok(ProxyStatus::Stale);
        };
        Ok(
            match serde_json::from_slice::<ProxyArtifactManifest>(&bytes) {
                Ok(actual) if actual == expected => ProxyStatus::Fresh,
                _ => ProxyStatus::Stale,
            },
        )
    }

    /// Returns true when the configured proxy exists and is safe to decode.
    pub fn proxy_is_fresh(&self, source_path: &Path, color: ProxyColorContract) -> bool {
        self.proxy_status(source_path, color).is_fresh()
    }

    /// 异步生成代理文件（后台 FFmpeg 转码）
    ///
    /// 通过 `mpsc::Sender` 实时回报进度。
    pub async fn generate(
        &self,
        asset_id: AssetId,
        source_path: PathBuf,
        color: ProxyColorContract,
        progress_tx: mpsc::Sender<ProxyProgress>,
    ) -> Result<PathBuf> {
        match self
            .generate_cancellable(
                asset_id,
                source_path.clone(),
                MediaFileFingerprint::capture(&source_path),
                color,
                progress_tx,
                ExecutionCancellationToken::new(),
            )
            .await
            .map_err(|error| MondrianError::ProxyGenerationFailed { reason: error.to_string() })?
        {
            ProxyGenerationOutcome::Completed(evidence) => Ok(evidence.media_path),
            ProxyGenerationOutcome::Reused(path) => Ok(path),
            ProxyGenerationOutcome::Canceled => Err(MondrianError::ProxyGenerationFailed {
                reason: "proxy generation was canceled".to_owned(),
            }),
        }
    }

    /// Generate one proxy while observing a monotonic cancellation token at
    /// concurrency admission, FFmpeg execution, and artifact publication.
    pub async fn generate_cancellable(
        &self,
        asset_id: AssetId,
        source_path: PathBuf,
        admitted_source: MediaFileFingerprint,
        color: ProxyColorContract,
        progress_tx: mpsc::Sender<ProxyProgress>,
        cancellation: ExecutionCancellationToken,
    ) -> std::result::Result<ProxyGenerationOutcome, ProxyGenerationError> {
        if cancellation.is_canceled() {
            return Ok(ProxyGenerationOutcome::Canceled);
        }
        if !source_path.exists() {
            return Err(mondrian_core::MondrianError::MediaOpen {
                path: source_path.display().to_string(),
                reason: "source file not found".to_string(),
            }
            .into());
        }
        require_proxy_source_revision(&source_path, admitted_source)?;
        let execution_source = MediaFileFingerprint::capture(&source_path);
        require_proxy_source_revision(&source_path, execution_source)?;
        if execution_source != admitted_source {
            return Err(MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "source file changed before proxy execution (admitted={admitted_source:?}, execution={execution_source:?})"
                ),
            }
            .into());
        }

        let encoding = self.encoding_profile(color)?;
        let output_path = self.proxy_path(&source_path, color)?;
        let manifest_path = proxy_manifest_path(&output_path);
        let manifest = self.expected_manifest(&source_path, color)?;
        if manifest.source != admitted_source.into() {
            return Err(MondrianError::ProxyGenerationFailed {
                reason: "source file changed while proxy request was entering execution".to_owned(),
            }
            .into());
        }

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                proxy_before_namespace_failure(ProxyPublicationPhase::Media, &output_path, error)
            })?;
        }

        if self.proxy_status_for_source_fingerprint(&source_path, admitted_source, color)?
            == ProxyStatus::Fresh
        {
            let _ = progress_tx
                .send(ProxyProgress {
                    asset_id,
                    progress: 1.0,
                    is_done: true,
                    error: None,
                })
                .await;
            return Ok(ProxyGenerationOutcome::Reused(output_path));
        }

        let output_staging = OwnedPublicationFile::create_sibling(&output_path, "proxy-media")
            .map_err(|error| {
                proxy_before_namespace_failure(ProxyPublicationPhase::Media, &output_path, error)
            })?;
        let output_reservation = output_staging.release_for_external_writer();
        let tmp_output_path = output_reservation.path().to_path_buf();

        tracing::info!(
            "Generating proxy for asset {asset_id}: {:?} → {:?}",
            source_path,
            output_path
        );

        let _ = progress_tx
            .send(ProxyProgress {
                asset_id,
                progress: 0.05,
                is_done: false,
                error: None,
            })
            .await;

        let height = self.config.resolution.height();
        let crf = self.config.crf.min(51);
        let source_for_cmd = source_path.clone();
        let output_for_cmd = tmp_output_path.clone();
        let concurrent_jobs = self.config.concurrent_jobs;
        let limiter = proxy_generation_limiter(
            output_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| output_path.clone()),
        );
        let permit_cancellation = cancellation.clone();
        let permit = tokio::task::spawn_blocking(move || {
            limiter.acquire(concurrent_jobs, &permit_cancellation)
        })
        .await
        .map_err(|e| MondrianError::ProxyGenerationFailed {
            reason: format!("proxy concurrency permit task join failed: {e}"),
        })?;
        let Some(permit) = permit else {
            return Ok(ProxyGenerationOutcome::Canceled);
        };

        let transcode_cancellation = cancellation.clone();
        let transcode_result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            run_ffmpeg_proxy_transcode_cancellable(
                encoding,
                crf,
                height,
                color,
                &source_for_cmd,
                &output_for_cmd,
                &transcode_cancellation,
            )
        })
        .await
        .map_err(|e| MondrianError::ProxyGenerationFailed {
            reason: format!("proxy task join failed: {e}"),
        })?;

        let transcode_result = transcode_result.and_then(|outcome| {
            if outcome == ProxyTranscodeOutcome::Canceled || cancellation.is_canceled() {
                return Ok(ProxyTranscodeOutcome::Canceled);
            }
            let completed_manifest = self.expected_manifest(&source_path, color)?;
            if completed_manifest.source != manifest.source {
                return Err(MondrianError::ProxyGenerationFailed {
                    reason: "source file changed while proxy generation was in progress".to_owned(),
                });
            }
            Ok(ProxyTranscodeOutcome::Completed)
        });

        match transcode_result {
            Ok(ProxyTranscodeOutcome::Completed) => {}
            Ok(ProxyTranscodeOutcome::Canceled) => {
                drop(output_reservation);
                return Ok(ProxyGenerationOutcome::Canceled);
            }
            Err(err) => {
                drop(output_reservation);
                let _ = progress_tx
                    .send(ProxyProgress {
                        asset_id,
                        progress: 1.0,
                        is_done: true,
                        error: Some(err.to_string()),
                    })
                    .await;
                return Err(err.into());
            }
        }

        if cancellation.is_canceled() {
            drop(output_reservation);
            return Ok(ProxyGenerationOutcome::Canceled);
        }

        let output_staging = output_reservation.reclaim().map_err(|error| {
            proxy_before_namespace_failure(ProxyPublicationPhase::Media, &output_path, error)
        })?;
        publish_proxy_staging(output_staging, ProxyPublicationPhase::Media, &output_path)?;
        write_proxy_manifest(&manifest, &manifest_path)?;

        let _ = progress_tx
            .send(ProxyProgress {
                asset_id,
                progress: 1.0,
                is_done: true,
                error: None,
            })
            .await;

        Ok(ProxyGenerationOutcome::Completed(
            ProxyPublicationEvidence::durable(output_path, manifest_path),
        ))
    }
}

fn require_proxy_source_revision(source_path: &Path, revision: MediaFileFingerprint) -> Result<()> {
    if revision.authorizes_reuse() {
        Ok(())
    } else {
        Err(MondrianError::MediaSourceRevisionUnavailable {
            path: source_path.display().to_string(),
            reason:
                "proxy freshness requires filesystem object identity and change-generation evidence"
                    .to_owned(),
        })
    }
}

#[derive(Default)]
struct ProxyConcurrencyState {
    active_jobs: usize,
    max_jobs: usize,
}

struct ProxyConcurrencyLimiter {
    state: Mutex<ProxyConcurrencyState>,
    changed: Condvar,
}

impl ProxyConcurrencyLimiter {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProxyConcurrencyState::default()),
            changed: Condvar::new(),
        }
    }

    fn acquire(
        self: Arc<Self>,
        concurrent_jobs: u8,
        cancellation: &ExecutionCancellationToken,
    ) -> Option<ProxyConcurrencyPermit> {
        let max_jobs = usize::from(concurrent_jobs.max(1));
        let mut state = lock_proxy_concurrency_state(&self.state);
        state.max_jobs = max_jobs;
        while state.active_jobs >= state.max_jobs {
            if cancellation.is_canceled() {
                return None;
            }
            state = match self.changed.wait_timeout(state, Duration::from_millis(10)) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
            state.max_jobs = max_jobs;
        }
        if cancellation.is_canceled() {
            return None;
        }
        state.active_jobs = state.active_jobs.saturating_add(1);
        drop(state);
        Some(ProxyConcurrencyPermit { limiter: self })
    }
}

struct ProxyConcurrencyPermit {
    limiter: Arc<ProxyConcurrencyLimiter>,
}

impl Drop for ProxyConcurrencyPermit {
    fn drop(&mut self) {
        let mut state = lock_proxy_concurrency_state(&self.limiter.state);
        state.active_jobs = state.active_jobs.saturating_sub(1);
        self.limiter.changed.notify_one();
    }
}

fn lock_proxy_concurrency_state(
    state: &Mutex<ProxyConcurrencyState>,
) -> MutexGuard<'_, ProxyConcurrencyState> {
    match state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn proxy_generation_limiter(cache_dir: PathBuf) -> Arc<ProxyConcurrencyLimiter> {
    static LIMITERS: OnceLock<Mutex<HashMap<PathBuf, Arc<ProxyConcurrencyLimiter>>>> =
        OnceLock::new();
    let limiters = LIMITERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match limiters.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .entry(cache_dir)
        .or_insert_with(|| Arc::new(ProxyConcurrencyLimiter::new()))
        .clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyTranscodeOutcome {
    Completed,
    Canceled,
}

const PROXY_FFMPEG_STDERR_TAIL_CAPACITY: usize = 64 * 1024;

fn run_ffmpeg_proxy_transcode_cancellable(
    encoding: ProxyEncodingProfile,
    crf: u8,
    height: u32,
    color: ProxyColorContract,
    source_path: &Path,
    output_path: &Path,
    cancellation: &ExecutionCancellationToken,
) -> Result<ProxyTranscodeOutcome> {
    if cancellation.is_canceled() {
        return Ok(ProxyTranscodeOutcome::Canceled);
    }
    let mut cmd = ffmpeg_proxy_command(encoding, crf, height, color, source_path, output_path)?;
    let output = match crate::run_supervised_command(
        &mut cmd,
        None,
        SupervisedProcessPolicy {
            stdout: SupervisedStreamCapture::Drain,
            stderr: SupervisedStreamCapture::Tail {
                limit_bytes: PROXY_FFMPEG_STDERR_TAIL_CAPACITY,
            },
            ..SupervisedProcessPolicy::default()
        },
        cancellation,
    ) {
        Ok(output) => output,
        Err(error) if error.is_canceled() => return Ok(ProxyTranscodeOutcome::Canceled),
        Err(error) => {
            return Err(MondrianError::ProxyGenerationFailed {
                reason: format!("proxy FFmpeg process failed: {error}"),
            });
        }
    };

    if !output.status.success() {
        return Err(mondrian_core::MondrianError::ProxyGenerationFailed {
            reason: format!(
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    if !output_path.exists() {
        return Err(mondrian_core::MondrianError::ProxyGenerationFailed {
            reason: "ffmpeg exited successfully but output file is missing".to_string(),
        });
    }

    Ok(ProxyTranscodeOutcome::Completed)
}

fn ffmpeg_proxy_command(
    encoding: ProxyEncodingProfile,
    crf: u8,
    height: u32,
    color: ProxyColorContract,
    source_path: &Path,
    output_path: &Path,
) -> Result<Command> {
    color.validate().map_err(proxy_color_contract_error)?;
    let (frame_range, scale_range, output_range) = match color.source_range {
        DecodedVideoRange::Limited => ("limited", "tv", "tv"),
        DecodedVideoRange::Full => ("full", "pc", "pc"),
        DecodedVideoRange::Unknown => {
            return Err(proxy_color_contract_error(
                ProxyColorContractError::UnknownSourceRange,
            ));
        }
    };
    let tags = color.source_color_space.ffmpeg_tags();
    let mut setparams = format!("setparams=range={frame_range}");
    if let Some(tags) = tags {
        setparams.push_str(&format!(
            ":color_primaries={}:color_trc={}:colorspace={}",
            tags.color_primaries, tags.color_trc, tags.colorspace
        ));
    }
    let filter_graph = format!(
        "{setparams},scale=-2:{height}:flags=lanczos:in_range={scale_range}:out_range={scale_range}"
    );
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(source_path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a?")
        .arg("-vf")
        .arg(filter_graph)
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("128k");

    if let Some(tags) = tags {
        cmd.arg("-color_primaries")
            .arg(tags.color_primaries)
            .arg("-color_trc")
            .arg(tags.color_trc)
            .arg("-colorspace")
            .arg(tags.colorspace);
    } else {
        tracing::warn!(
            source_color_space = ?color.source_color_space,
            "proxy source has no trustworthy standardized FFmpeg tags; sidecar color contract remains authoritative"
        );
    }
    cmd.arg("-color_range").arg(output_range);

    match encoding {
        ProxyEncodingProfile::H264High8 => {
            cmd.arg("-pix_fmt")
                .arg("yuv420p")
                .arg("-c:v")
                .arg("libx264")
                .arg("-profile:v")
                .arg("high")
                .arg("-preset")
                .arg("veryfast")
                .arg("-crf")
                .arg(crf.to_string())
                .arg("-movflags")
                .arg("+faststart");
        }
        ProxyEncodingProfile::H265Main10 => {
            cmd.arg("-pix_fmt")
                .arg("yuv420p10le")
                .arg("-c:v")
                .arg("libx265")
                .arg("-profile:v")
                .arg("main10")
                .arg("-preset")
                .arg("veryfast")
                .arg("-crf")
                .arg(crf.to_string())
                .arg("-movflags")
                .arg("+faststart");
        }
        ProxyEncodingProfile::DnxHrSq8 => {
            cmd.arg("-pix_fmt")
                .arg("yuv422p")
                .arg("-c:v")
                .arg("dnxhd")
                .arg("-profile:v")
                .arg("dnxhr_sq")
                .arg("-f")
                .arg("mov");
        }
        ProxyEncodingProfile::DnxHrHqx10 => {
            cmd.arg("-pix_fmt")
                .arg("yuv422p10le")
                .arg("-c:v")
                .arg("dnxhd")
                .arg("-profile:v")
                .arg("dnxhr_hqx")
                .arg("-f")
                .arg("mov");
        }
    }

    // Atomic publication deliberately writes through a `.part` suffix. Never
    // ask FFmpeg to infer a muxer from that temporary path: container identity
    // is part of the proxy encoding contract.
    cmd.arg("-f").arg(encoding.output_container()).arg(output_path);
    Ok(cmd)
}

fn proxy_color_contract_error(error: ProxyColorContractError) -> MondrianError {
    MondrianError::ProxyGenerationFailed { reason: error.to_string() }
}

#[cfg(test)]
mod tests {
    use super::{
        ffmpeg_proxy_command, publish_proxy_staging, ProxyCodec, ProxyColorContract,
        ProxyColorContractError, ProxyConcurrencyLimiter, ProxyConfig, ProxyEncodingProfile,
        ProxyGenerationOutcome, ProxyGenerator, ProxyPublicationFailureKind, ProxyPublicationPhase,
        ProxyStatus, PROXY_COLOR_CONTRACT_VERSION, PROXY_PRIMARY_VIDEO_STREAM_INDEX,
    };
    use crate::{DecodedVideoRange, MediaFileFingerprint};
    use mondrian_core::{types::ColorSpace, ExecutionCancellationToken};
    use mondrian_storage::OwnedPublicationFile;
    use std::io::Write;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_proxy_config(cache_dir: std::path::PathBuf) -> ProxyConfig {
        ProxyConfig { cache_dir, ..ProxyConfig::default() }
    }

    fn rec709_contract() -> ProxyColorContract {
        ProxyColorContract::try_new(ColorSpace::Rec709, 8, DecodedVideoRange::Limited)
            .expect("valid Rec.709 proxy contract")
    }

    #[test]
    fn proxy_color_contract_rejects_implicit_range_and_invalid_precision() {
        assert_eq!(
            ProxyColorContract::try_new(ColorSpace::Rec709, 8, DecodedVideoRange::Unknown),
            Err(ProxyColorContractError::UnknownSourceRange)
        );
        assert_eq!(
            ProxyColorContract::try_new(ColorSpace::Rec709, 0, DecodedVideoRange::Limited),
            Err(ProxyColorContractError::UnsupportedSourceBitDepth(0))
        );
        assert_eq!(
            ProxyColorContract::try_new(ColorSpace::Rec709, 17, DecodedVideoRange::Limited),
            Err(ProxyColorContractError::UnsupportedSourceBitDepth(17))
        );
    }

    #[test]
    fn generator_revalidates_deserialized_proxy_contracts() {
        let invalid = serde_json::from_value::<ProxyColorContract>(serde_json::json!({
            "version": PROXY_COLOR_CONTRACT_VERSION,
            "source_color_space": "Rec709",
            "source_bit_depth": 8,
            "source_range": "Unknown"
        }))
        .expect("schema-valid proxy contract");

        let error = ProxyGenerator::new(ProxyConfig::default())
            .encoding_profile(invalid)
            .expect_err("unknown range must fail after deserialization");

        assert!(error.to_string().contains("explicit source video range"));
    }

    #[test]
    fn proxy_status_reports_missing_when_proxy_file_does_not_exist() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mp4");
        std::fs::write(&source, b"source").expect("source");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));

        let color = rec709_contract();
        assert_eq!(generator.proxy_status(&source, color), ProxyStatus::Missing);
        assert!(!generator.proxy_is_fresh(&source, color));
    }

    #[test]
    fn proxy_status_requires_matching_manifest() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mp4");
        std::fs::write(&source, b"source").expect("source");
        std::thread::sleep(Duration::from_millis(20));

        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let color = rec709_contract();
        let proxy = generator.proxy_path(&source, color).expect("proxy path");
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");

        assert_eq!(generator.proxy_status(&source, color), ProxyStatus::Stale);
        generator.install_test_manifest(&source, color);
        assert_eq!(generator.proxy_status(&source, color), ProxyStatus::Fresh);
        assert!(generator.proxy_is_fresh(&source, color));
    }

    #[test]
    fn proxy_status_reports_stale_when_source_is_newer_than_proxy() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mp4");
        std::fs::write(&source, b"source-v1").expect("initial source revision");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let color = rec709_contract();
        let proxy = generator.proxy_path(&source, color).expect("proxy path");
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");
        generator.install_test_manifest(&source, color);
        assert_eq!(generator.proxy_status(&source, color), ProxyStatus::Fresh);
        std::thread::sleep(Duration::from_millis(20));
        // Preserve the length and path so freshness depends on the strong
        // object/change-generation revision rather than the legacy size check.
        std::fs::write(&source, b"source-v2").expect("new source revision");

        assert_eq!(generator.proxy_status(&source, color), ProxyStatus::Stale);
        assert!(!generator.proxy_is_fresh(&source, color));
    }

    #[test]
    fn automatic_profile_preserves_hdr_and_log_precision() {
        let generator = ProxyGenerator::new(ProxyConfig::default());
        let pq = ProxyColorContract::try_new(ColorSpace::Rec2100Pq, 10, DecodedVideoRange::Limited)
            .expect("valid PQ contract");
        let log = ProxyColorContract::try_new(
            ColorSpace::SonySLog3SGamut3Cine,
            10,
            DecodedVideoRange::Full,
        )
        .expect("valid log contract");

        assert_eq!(
            generator.encoding_profile(pq).expect("PQ profile"),
            ProxyEncodingProfile::H265Main10
        );
        assert_eq!(
            generator.encoding_profile(log).expect("Log profile"),
            ProxyEncodingProfile::H265Main10
        );
    }

    #[test]
    fn forced_h264_rejects_high_precision_source() {
        let config = ProxyConfig { codec: ProxyCodec::H264, ..ProxyConfig::default() };
        let generator = ProxyGenerator::new(config);
        let pq = ProxyColorContract::try_new(ColorSpace::Rec2100Pq, 10, DecodedVideoRange::Limited)
            .expect("valid PQ contract");

        let error = generator.encoding_profile(pq).expect_err("H.264 must reject PQ");

        assert!(error.to_string().contains("cannot preserve"));
    }

    #[test]
    fn proxy_identity_changes_with_color_contract() {
        let generator = ProxyGenerator::new(ProxyConfig::default());
        let source = std::path::Path::new("E:/media/source.mov");
        let rec709 = rec709_contract();
        let pq = ProxyColorContract::try_new(ColorSpace::Rec2100Pq, 10, DecodedVideoRange::Limited)
            .expect("valid PQ contract");

        assert_ne!(
            generator.proxy_path(source, rec709).expect("Rec.709 path"),
            generator.proxy_path(source, pq).expect("PQ path")
        );
    }

    #[test]
    fn proxy_planning_normalizes_relative_cache_root_to_absolute_namespace() {
        let generator = ProxyGenerator::new(test_proxy_config(
            std::path::PathBuf::from("target").join("relative-proxy-cache"),
        ));

        let path = generator
            .proxy_path(
                std::path::Path::new("unmaterialized-source.mov"),
                rec709_contract(),
            )
            .expect("absolute proxy plan");

        assert!(path.is_absolute());
        assert!(path
            .parent()
            .expect("cache parent")
            .ends_with(std::path::Path::new("target").join("relative-proxy-cache")));
    }

    #[test]
    fn proxy_identity_canonicalizes_an_observable_source_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mov");
        std::fs::write(&source, b"source").expect("source");
        let canonical_source = std::fs::canonicalize(&source).expect("canonical source");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let color = rec709_contract();

        assert_eq!(
            generator.proxy_path(&source, color).expect("lexical proxy path"),
            generator.proxy_path(&canonical_source, color).expect("canonical proxy path")
        );
    }

    #[cfg(unix)]
    #[test]
    fn proxy_identity_preserves_non_utf8_native_path_units() {
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir().expect("tempdir");
        let first = root.path().join(std::ffi::OsString::from_vec(b"source-\x80.mov".to_vec()));
        let second = root.path().join(std::ffi::OsString::from_vec(b"source-\x81.mov".to_vec()));
        std::fs::write(&first, b"first").expect("first non-UTF8 source");
        std::fs::write(&second, b"second").expect("second non-UTF8 source");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let color = rec709_contract();

        assert_ne!(
            generator.proxy_path(&first, color).expect("first proxy path"),
            generator.proxy_path(&second, color).expect("second proxy path"),
            "distinct native path units must not collapse through a lossy Unicode projection"
        );
    }

    #[test]
    fn hdr_proxy_command_declares_main10_and_cicp_tags() {
        let color =
            ProxyColorContract::try_new(ColorSpace::Rec2100Pq, 10, DecodedVideoRange::Limited)
                .expect("valid PQ contract");
        let command = ffmpeg_proxy_command(
            ProxyEncodingProfile::H265Main10,
            20,
            720,
            color,
            std::path::Path::new("source.mov"),
            std::path::Path::new("proxy.mp4.part"),
        )
        .expect("valid FFmpeg proxy command");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-pix_fmt", "yuv420p10le"]));
        assert!(args.windows(2).any(|pair| pair == ["-profile:v", "main10"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_trc", "smpte2084"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_range", "tv"]));
        assert!(args.windows(2).any(|pair| pair == ["-f", "mp4"]));
        let filter = args
            .windows(2)
            .find_map(|pair| (pair[0] == "-vf").then_some(pair[1].as_str()))
            .expect("video filter graph");
        assert!(filter.contains("setparams=range=limited"));
        assert!(filter.contains("color_primaries=bt2020"));
        assert!(filter.contains("color_trc=smpte2084"));
        assert!(filter.contains("colorspace=bt2020nc"));
        assert!(filter.contains("in_range=tv:out_range=tv"));
    }

    #[test]
    fn proxy_command_and_physical_contract_bind_output_video_stream_zero() {
        let command = ffmpeg_proxy_command(
            ProxyEncodingProfile::H264High8,
            20,
            720,
            rec709_contract(),
            std::path::Path::new("source.mov"),
            std::path::Path::new("proxy.mp4.part"),
        )
        .expect("valid FFmpeg proxy command");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-map", "0:v:0"]));
        assert_eq!(PROXY_PRIMARY_VIDEO_STREAM_INDEX, 0);
    }

    #[test]
    fn log_proxy_command_does_not_emit_false_standardized_tags() {
        let color = ProxyColorContract::try_new(
            ColorSpace::SonySLog3SGamut3Cine,
            10,
            DecodedVideoRange::Full,
        )
        .expect("valid log contract");
        let command = ffmpeg_proxy_command(
            ProxyEncodingProfile::H265Main10,
            20,
            720,
            color,
            std::path::Path::new("source.mov"),
            std::path::Path::new("proxy.mp4.part"),
        )
        .expect("valid FFmpeg proxy command");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(!args.iter().any(|arg| arg == "-color_primaries"));
        assert!(!args.iter().any(|arg| arg == "-color_trc"));
        assert!(!args.iter().any(|arg| arg == "-colorspace"));
        assert!(args.windows(2).any(|pair| pair == ["-color_range", "pc"]));
        assert!(args.windows(2).any(|pair| pair == ["-f", "mp4"]));
        let filter = args
            .windows(2)
            .find_map(|pair| (pair[0] == "-vf").then_some(pair[1].as_str()))
            .expect("video filter graph");
        assert_eq!(
            filter,
            "setparams=range=full,scale=-2:720:flags=lanczos:in_range=pc:out_range=pc"
        );
    }

    #[test]
    fn dnxhr_proxy_command_pins_mov_muxer_for_atomic_partial_path() {
        let command = ffmpeg_proxy_command(
            ProxyEncodingProfile::DnxHrSq8,
            20,
            720,
            rec709_contract(),
            std::path::Path::new("source.mov"),
            std::path::Path::new("proxy.mov.part"),
        )
        .expect("valid DNxHR proxy command");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-f", "mov"]));
    }

    #[test]
    fn proxy_publication_replaces_existing_file_without_backup_namespace() {
        let root = tempfile::tempdir().expect("tempdir");
        let output = root.path().join("proxy.mp4");
        std::fs::write(&output, b"old proxy").expect("old proxy");
        let mut staging =
            OwnedPublicationFile::create_sibling(&output, "proxy-test").expect("staging");
        let staging_path = staging.path().to_path_buf();
        staging.file_mut().expect("writer").write_all(b"new proxy").expect("write");

        publish_proxy_staging(staging, ProxyPublicationPhase::Media, &output).expect("publish");

        assert_eq!(std::fs::read(&output).expect("output"), b"new proxy");
        assert!(!staging_path.exists());
        assert_eq!(
            std::fs::read_dir(root.path()).expect("read root").count(),
            1,
            "publication must not expose an intermediate backup namespace"
        );
    }

    #[test]
    fn external_proxy_writer_reclaims_reserved_file_identity_before_publish() {
        let root = tempfile::tempdir().expect("tempdir");
        let output = root.path().join("proxy.mp4");
        let staging = OwnedPublicationFile::create_sibling(&output, "proxy-test").expect("staging");
        let reservation = staging.release_for_external_writer();
        let staging_path = reservation.path().to_path_buf();

        std::fs::write(&staging_path, b"ffmpeg output").expect("external writer");
        let staging = reservation.reclaim().expect("same reserved file identity");
        publish_proxy_staging(staging, ProxyPublicationPhase::Media, &output).expect("publish");

        assert_eq!(std::fs::read(&output).expect("output"), b"ffmpeg output");
        assert!(!staging_path.exists());
    }

    #[test]
    fn pre_namespace_proxy_failure_preserves_existing_target() {
        let root = tempfile::tempdir().expect("tempdir");
        let output = root.path().join("proxy.mp4");
        std::fs::create_dir(&output).expect("foreign target directory");
        let mut staging =
            OwnedPublicationFile::create_sibling(&output, "proxy-test").expect("staging");
        let staging_path = staging.path().to_path_buf();
        staging.file_mut().expect("writer").write_all(b"new proxy").expect("write");

        let error = publish_proxy_staging(staging, ProxyPublicationPhase::Media, &output)
            .expect_err("shape must be rejected");

        let super::ProxyGenerationError::Publication(failure) = error else {
            panic!("expected typed publication failure");
        };
        assert_eq!(failure.phase, ProxyPublicationPhase::Media);
        assert_eq!(failure.kind, ProxyPublicationFailureKind::BeforeNamespace);
        assert_eq!(failure.target_path, output);
        assert!(output.is_dir(), "foreign target must remain untouched");
        assert!(
            !staging_path.exists(),
            "identity-owned pre-namespace staging should be safely cleaned"
        );
    }

    #[test]
    fn proxy_source_bans_legacy_backup_and_restore_publication() {
        let source = include_str!("proxy.rs");
        let backup_marker = ["replace", "-backup"].concat();
        let legacy_finalize = ["fn finalize_", "proxy_output"].concat();
        let direct_target_rename = ["std::fs::rename(output_", "path"].concat();

        assert!(!source.contains(&backup_marker));
        assert!(!source.contains(&legacy_finalize));
        assert!(!source.contains(&direct_target_rename));
    }

    #[test]
    fn proxy_concurrency_limiter_blocks_when_single_job_is_active() {
        let limiter = Arc::new(ProxyConcurrencyLimiter::new());
        let first_permit = limiter.clone().acquire(1, &ExecutionCancellationToken::new());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let worker_limiter = Arc::clone(&limiter);

        let worker = std::thread::spawn(move || {
            ready_tx.send(()).expect("ready");
            let _second_permit = worker_limiter.acquire(1, &ExecutionCancellationToken::new());
            acquired_tx.send(()).expect("acquired");
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).expect("worker ready");
        assert!(acquired_rx.recv_timeout(Duration::from_millis(50)).is_err());

        drop(first_permit);
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second permit acquired after release");
        worker.join().expect("worker");
    }

    #[test]
    fn proxy_concurrency_limiter_allows_configured_parallel_jobs() {
        let limiter = Arc::new(ProxyConcurrencyLimiter::new());
        let first_permit = limiter.clone().acquire(2, &ExecutionCancellationToken::new());
        let second_permit = limiter.clone().acquire(2, &ExecutionCancellationToken::new());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let worker_limiter = Arc::clone(&limiter);

        let worker = std::thread::spawn(move || {
            ready_tx.send(()).expect("ready");
            let _third_permit = worker_limiter.acquire(2, &ExecutionCancellationToken::new());
            acquired_tx.send(()).expect("acquired");
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).expect("worker ready");
        assert!(acquired_rx.recv_timeout(Duration::from_millis(50)).is_err());

        drop(first_permit);
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("third permit acquired after one release");
        worker.join().expect("worker");
        drop(second_permit);
    }

    #[test]
    fn proxy_concurrency_wait_observes_cancellation() {
        let limiter = Arc::new(ProxyConcurrencyLimiter::new());
        let first = limiter
            .clone()
            .acquire(1, &ExecutionCancellationToken::new())
            .expect("first permit");
        let cancellation = ExecutionCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_limiter = Arc::clone(&limiter);
        let worker =
            std::thread::spawn(move || worker_limiter.acquire(1, &worker_cancellation).is_none());
        std::thread::sleep(Duration::from_millis(20));
        cancellation.cancel();
        assert!(worker.join().expect("cancellation waiter"));
        drop(first);
    }

    #[test]
    fn pre_canceled_generation_does_not_probe_or_open_source() {
        let root = tempfile::tempdir().expect("tempdir");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().to_path_buf()));
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let outcome = runtime
            .block_on(generator.generate_cancellable(
                mondrian_core::AssetId::new(),
                root.path().join("missing.mov"),
                MediaFileFingerprint {
                    len: None,
                    modified_secs: None,
                    modified_nanos: None,
                    ..MediaFileFingerprint::default()
                },
                rec709_contract(),
                progress_tx,
                cancellation,
            ))
            .expect("cancellation is not an execution failure");
        assert_eq!(outcome, ProxyGenerationOutcome::Canceled);
    }

    #[test]
    fn generation_rejects_source_replaced_after_admission_before_ffmpeg() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mov");
        std::fs::write(&source, b"admitted").expect("source");
        let admitted = MediaFileFingerprint::capture(&source);
        std::fs::write(&source, b"replacement with different length").expect("replace source");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().to_path_buf()));
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = runtime
            .block_on(generator.generate_cancellable(
                mondrian_core::AssetId::new(),
                source,
                admitted,
                rec709_contract(),
                progress_tx,
                ExecutionCancellationToken::new(),
            ))
            .expect_err("source revision drift must fail before FFmpeg");
        assert!(error.to_string().contains("changed before proxy execution"));
    }
}
