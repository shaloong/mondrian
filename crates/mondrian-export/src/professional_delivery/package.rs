use crate::preset::{ProfessionalDeliveryMetadata, ProfessionalDeliveryProfile};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use mondrian_core::Rational;
use roxmltree::Document;
use sha1::{Digest as _, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const MAX_XML_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PACKAGE_ASSETS: usize = 32;

macro_rules! package_id {
    ($name:ident) => {
        #[doc = concat!("Strongly typed professional-package ", stringify!($name), ".")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            #[doc = concat!("Generate a random ", stringify!($name), ".")]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            #[doc = concat!("Return the UUID URN for this ", stringify!($name), ".")]
            pub fn urn(self) -> String {
                format!("urn:uuid:{}", self.0)
            }

            #[doc = concat!("Construct a typed ", stringify!($name), " from one UUID.")]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            #[doc = concat!("Return the canonical UUID for this ", stringify!($name), ".")]
            pub const fn uuid(self) -> Uuid {
                self.0
            }

            fn parse(value: &str) -> Result<Self, ProfessionalPackageError> {
                let raw = value.strip_prefix("urn:uuid:").ok_or_else(|| {
                    ProfessionalPackageError::InvalidXml("package ID is not a UUID URN".to_owned())
                })?;
                Uuid::parse_str(raw).map(Self).map_err(|error| {
                    ProfessionalPackageError::InvalidXml(format!("invalid package UUID: {error}"))
                })
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

package_id!(PackageAssetId);
package_id!(CompositionPlaylistId);
package_id!(PackingListId);
package_id!(PackageElementId);

/// Normalized package-relative path with no traversal or alternate separator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageRelativePath(String);

impl PackageRelativePath {
    /// Admit one safe UTF-8 leaf path.
    pub fn new(value: impl Into<String>) -> Result<Self, ProfessionalPackageError> {
        let value = value.into();
        let path = Path::new(&value);
        if value.is_empty()
            || value.contains('\\')
            || path.is_absolute()
            || path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
        {
            return Err(ProfessionalPackageError::UnsafePath(value));
        }
        Ok(Self(value))
    }

    /// UTF-8 package-relative representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn join(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

/// Semantic role of one package object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PackageAssetRole {
    /// Asset-map XML.
    AssetMap,
    /// Packing-list XML.
    PackingList,
    /// Composition-playlist XML.
    CompositionPlaylist,
    /// Picture track file.
    PictureTrack,
    /// Primary audio track file.
    AudioTrack,
}

/// Profile-owned package digest algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageDigestAlgorithm {
    /// SHA-1 represented as Base64, as required by these pinned editions.
    Sha1,
}

/// Digest value retained with validation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDigest {
    /// Exact algorithm.
    pub algorithm: PackageDigestAlgorithm,
    /// Base64-encoded digest bytes.
    pub base64: String,
}

/// Exact validated package asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAssetEvidence {
    /// Stable package asset ID.
    pub id: PackageAssetId,
    /// Safe relative path.
    pub path: PackageRelativePath,
    /// Semantic role.
    pub role: PackageAssetRole,
    /// Exact byte length.
    pub size: u64,
    /// Profile-owned content digest.
    pub digest: PackageDigest,
}

/// Closed package inventory, ordered by relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInventory {
    /// Exact assets in deterministic path order.
    pub assets: Vec<PackageAssetEvidence>,
}

/// Existing essence track supplied to package XML construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfessionalPackageTrack {
    /// UUID written into the track file by the wrapping Adapter.
    pub id: PackageAssetId,
    /// Safe track-file name under the package root.
    pub path: PackageRelativePath,
    /// Track role.
    pub role: PackageAssetRole,
    /// IMF-only metadata extracted from the actual wrapped Track File.
    pub imf: Option<ImfTrackMetadata>,
}

/// RegXML and timing evidence extracted from one actual IMF Track File.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImfTrackMetadata {
    /// CPL-local identity of the embedded Essence Descriptor.
    pub essence_descriptor_id: PackageElementId,
    /// Bounded RegXML descriptor fragment emitted by a qualified MXF reader.
    pub essence_descriptor_xml: String,
    /// Native resource edit rate.
    pub edit_rate: Rational,
    /// Exact Track File duration in native edit units.
    pub intrinsic_duration: u64,
    /// Exact resource duration selected by the composition in native edit units.
    pub source_duration: u64,
}

/// Frozen identities used while serializing one package document graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfessionalPackageDocumentIds {
    /// AssetMap document identity.
    pub asset_map: PackageAssetId,
    /// Composition content-version identity.
    pub content_version: PackageElementId,
    /// Single segment or reel identity.
    pub segment_or_reel: PackageElementId,
    /// Main-image sequence identity.
    pub picture_sequence: PackageElementId,
    /// Main-image virtual-track identity.
    pub picture_virtual_track: PackageElementId,
    /// Main-image resource identity.
    pub picture_resource: PackageElementId,
    /// Main-audio sequence identity.
    pub audio_sequence: PackageElementId,
    /// Main-audio virtual-track identity.
    pub audio_virtual_track: PackageElementId,
    /// Main-audio resource identity.
    pub audio_resource: PackageElementId,
}

impl ProfessionalPackageDocumentIds {
    /// Allocate a complete package-document identity set once per frozen attempt.
    pub fn new() -> Self {
        Self {
            asset_map: PackageAssetId::new(),
            content_version: PackageElementId::new(),
            segment_or_reel: PackageElementId::new(),
            picture_sequence: PackageElementId::new(),
            picture_virtual_track: PackageElementId::new(),
            picture_resource: PackageElementId::new(),
            audio_sequence: PackageElementId::new(),
            audio_virtual_track: PackageElementId::new(),
            audio_resource: PackageElementId::new(),
        }
    }
}

impl Default for ProfessionalPackageDocumentIds {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete package graph request after essence wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfessionalPackageBuildRequest {
    /// Exact IMF or DCP profile.
    pub profile: ProfessionalDeliveryProfile,
    /// Frozen human-authored metadata.
    pub metadata: ProfessionalDeliveryMetadata,
    /// Composition ID.
    pub composition_id: CompositionPlaylistId,
    /// Packing-list ID.
    pub packing_list_id: PackingListId,
    /// Frozen issue time shared by every package document.
    pub issued_at: DateTime<Utc>,
    /// Frozen internal document identities.
    pub document_ids: ProfessionalPackageDocumentIds,
    /// Picture track.
    pub picture: ProfessionalPackageTrack,
    /// Optional primary audio track.
    pub audio: Option<ProfessionalPackageTrack>,
    /// Exact edit rate.
    pub edit_rate: Rational,
    /// Exact composition duration in edit units.
    pub duration: u64,
}

/// Non-forgeable package evidence returned only after graph/hash/XML validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProfessionalPackage {
    profile: ProfessionalDeliveryProfile,
    root: PathBuf,
    composition_id: CompositionPlaylistId,
    inventory: PackageInventory,
}

impl ValidatedProfessionalPackage {
    /// Exact validated profile.
    pub fn profile(&self) -> ProfessionalDeliveryProfile {
        self.profile
    }

    /// Validated package root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validated composition identity.
    pub fn composition_id(&self) -> CompositionPlaylistId {
        self.composition_id
    }

    /// Closed validated inventory.
    pub fn inventory(&self) -> &PackageInventory {
        &self.inventory
    }
}

/// Package construction or reimport failure.
#[derive(Debug, thiserror::Error)]
pub enum ProfessionalPackageError {
    /// Profile does not publish a directory package.
    #[error("profile {0:?} does not use IMF/DCP package XML")]
    UnsupportedProfile(ProfessionalDeliveryProfile),
    /// One relative path could escape or alias the package root.
    #[error("unsafe professional package relative path: {0}")]
    UnsafePath(String),
    /// Package object graph is incomplete or inconsistent.
    #[error("invalid professional package graph: {0}")]
    InvalidGraph(String),
    /// XML is unsafe, malformed, or uses the wrong pinned schema identity.
    #[error("invalid professional package XML: {0}")]
    InvalidXml(String),
    /// A file is missing, duplicated, or has an unexpected identity.
    #[error("invalid professional package inventory: {0}")]
    InvalidInventory(String),
    /// Filesystem I/O failed.
    #[error("professional package I/O failed for {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Write the pinned XML documents and immediately reimport/validate the whole package.
pub fn build_and_validate_package(
    root: &Path,
    request: &ProfessionalPackageBuildRequest,
) -> Result<ValidatedProfessionalPackage, ProfessionalPackageError> {
    validate_build_request(root, request)?;
    let names = package_names(request.profile)?;
    let picture = evidence_for_track(root, &request.picture)?;
    let audio = request
        .audio
        .as_ref()
        .map(|track| evidence_for_track(root, track))
        .transpose()?;
    let cpl = build_cpl_xml(request, &picture, audio.as_ref())?;
    write_new(root.join(names.cpl), cpl.as_bytes())?;
    let cpl_evidence = evidence_for_xml(
        root,
        PackageAssetId(request.composition_id.0),
        names.cpl,
        PackageAssetRole::CompositionPlaylist,
    )?;
    let mut pkl_assets = vec![picture.clone(), cpl_evidence.clone()];
    if let Some(audio) = audio.clone() {
        pkl_assets.push(audio);
    }
    let pkl = build_pkl_xml(request, &pkl_assets)?;
    write_new(root.join(names.pkl), pkl.as_bytes())?;
    let pkl_evidence = evidence_for_xml(
        root,
        PackageAssetId(request.packing_list_id.0),
        names.pkl,
        PackageAssetRole::PackingList,
    )?;
    let mut mapped = pkl_assets;
    mapped.push(pkl_evidence);
    let asset_map = build_asset_map_xml(request, &mapped)?;
    write_new(root.join(names.asset_map), asset_map.as_bytes())?;
    reimport_and_validate_package(root, request.profile)
}

/// Reopen a complete package tree and independently validate references, sizes, and hashes.
pub fn reimport_and_validate_package(
    root: &Path,
    profile: ProfessionalDeliveryProfile,
) -> Result<ValidatedProfessionalPackage, ProfessionalPackageError> {
    let names = package_names(profile)?;
    let asset_map_text = read_bounded_xml(&root.join(names.asset_map))?;
    let pkl_text = read_bounded_xml(&root.join(names.pkl))?;
    let cpl_text = read_bounded_xml(&root.join(names.cpl))?;
    let asset_map = parse_xml(&asset_map_text, "AssetMap", asset_map_namespace())?;
    let pkl = parse_xml(&pkl_text, "PackingList", pkl_namespace(profile))?;
    let cpl = parse_xml(&cpl_text, "CompositionPlaylist", cpl_namespace(profile))?;

    let composition_id = parse_root_id::<CompositionPlaylistId>(&cpl)?;
    let pkl_id = parse_root_id::<PackingListId>(&pkl)?;
    let asset_map_id = parse_root_id::<PackageAssetId>(&asset_map)?;
    let mapped_paths = collect_asset_map_paths(&asset_map)?;
    let pkl_assets = collect_pkl_assets(&pkl)?;
    if pkl_assets.len() > MAX_PACKAGE_ASSETS {
        return Err(ProfessionalPackageError::InvalidInventory(
            "package exceeds the 32-asset bound".to_owned(),
        ));
    }
    let cpl_refs = collect_cpl_track_refs(&cpl, profile)?;
    validate_cpl_semantics(&cpl, profile)?;
    let pkl_ids: BTreeSet<_> = pkl_assets.keys().copied().collect();
    if !cpl_refs.keys().all(|id| pkl_ids.contains(id)) {
        return Err(ProfessionalPackageError::InvalidGraph(
            "CPL references an asset absent from the PKL".to_owned(),
        ));
    }
    if !pkl_assets.contains_key(&PackageAssetId(composition_id.0)) {
        return Err(ProfessionalPackageError::InvalidGraph(
            "PKL does not include the CPL asset".to_owned(),
        ));
    }
    let mapped_ids: BTreeSet<_> = mapped_paths.keys().copied().collect();
    let mut expected_mapped = pkl_ids.clone();
    expected_mapped.insert(PackageAssetId(pkl_id.0));
    if mapped_ids != expected_mapped {
        return Err(ProfessionalPackageError::InvalidGraph(
            "AssetMap and PKL object closure differ".to_owned(),
        ));
    }
    let mapped_packing_lists =
        mapped_paths.iter().filter(|(_, entry)| entry.packing_list).collect::<Vec<_>>();
    if mapped_packing_lists.len() != 1
        || *mapped_packing_lists[0].0 != PackageAssetId(pkl_id.0)
        || mapped_packing_lists[0].1.path.as_str() != names.pkl
    {
        return Err(ProfessionalPackageError::InvalidGraph(
            "AssetMap must identify exactly the imported PKL as its PackingList".to_owned(),
        ));
    }
    for (id, reference) in &cpl_refs {
        let Some(pkl_asset) = pkl_assets.get(id) else {
            continue;
        };
        if pkl_asset.hash != reference.hash {
            return Err(ProfessionalPackageError::InvalidGraph(
                "CPL and PKL Track File hashes differ".to_owned(),
            ));
        }
    }

    let mut evidence = Vec::with_capacity(mapped_paths.len() + 1);
    for (id, mapped) in mapped_paths {
        let role = if mapped.path.as_str() == names.pkl {
            PackageAssetRole::PackingList
        } else if mapped.path.as_str() == names.cpl {
            PackageAssetRole::CompositionPlaylist
        } else if let Some(reference) = cpl_refs.get(&id) {
            reference.role
        } else {
            return Err(ProfessionalPackageError::InvalidInventory(format!(
                "unclassified mapped asset {}",
                mapped.path.as_str()
            )));
        };
        let actual = evidence_for_path(root, id, mapped.path.clone(), role)?;
        if mapped.length != actual.size {
            return Err(ProfessionalPackageError::InvalidInventory(format!(
                "AssetMap length mismatch for {}",
                mapped.path.as_str()
            )));
        }
        if let Some(expected) = pkl_assets.get(&id)
            && (expected.size != actual.size
                || expected.hash != actual.digest.base64
                || expected.path != mapped.path)
        {
            return Err(ProfessionalPackageError::InvalidInventory(format!(
                "size or SHA-1 mismatch for {}",
                mapped.path.as_str()
            )));
        }
        if let Some(expected) = pkl_assets.get(&id) {
            let expected_type = if role == PackageAssetRole::CompositionPlaylist {
                "text/xml"
            } else {
                "application/mxf"
            };
            if expected.media_type != expected_type {
                return Err(ProfessionalPackageError::InvalidInventory(format!(
                    "unexpected PKL media type for {}",
                    mapped.path.as_str()
                )));
            }
        }
        evidence.push(actual);
    }
    evidence.push(evidence_for_xml(
        root,
        asset_map_id,
        names.asset_map,
        PackageAssetRole::AssetMap,
    )?);
    evidence.sort_by(|left, right| left.path.cmp(&right.path));
    validate_closed_directory(root, &evidence)?;
    Ok(ValidatedProfessionalPackage {
        profile,
        root: root.to_path_buf(),
        composition_id,
        inventory: PackageInventory { assets: evidence },
    })
}

#[derive(Clone, Copy)]
struct PackageNames {
    asset_map: &'static str,
    pkl: &'static str,
    cpl: &'static str,
}

#[derive(Debug, Clone)]
struct AssetMapEntry {
    path: PackageRelativePath,
    length: u64,
    packing_list: bool,
}

#[derive(Debug, Clone)]
struct PackingListEntry {
    size: u64,
    hash: String,
    path: PackageRelativePath,
    media_type: String,
}

#[derive(Debug, Clone)]
struct CplTrackReference {
    role: PackageAssetRole,
    hash: String,
}

fn package_names(
    profile: ProfessionalDeliveryProfile,
) -> Result<PackageNames, ProfessionalPackageError> {
    match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => Ok(PackageNames {
            asset_map: "ASSETMAP.xml",
            pkl: "PKL.xml",
            cpl: "CPL.xml",
        }),
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => Ok(PackageNames {
            asset_map: "ASSETMAP.xml",
            pkl: "PKL.xml",
            cpl: "CPL.xml",
        }),
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            Err(ProfessionalPackageError::UnsupportedProfile(profile))
        }
    }
}

fn asset_map_namespace() -> &'static str {
    "http://www.smpte-ra.org/schemas/429-9/2007/AM"
}

fn pkl_namespace(profile: ProfessionalDeliveryProfile) -> &'static str {
    match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            "http://www.smpte-ra.org/schemas/2067-2/2016/PKL"
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            "http://www.smpte-ra.org/schemas/429-8/2007/PKL"
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => "",
    }
}

fn cpl_namespace(profile: ProfessionalDeliveryProfile) -> &'static str {
    match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            "http://www.smpte-ra.org/schemas/2067-3/2016"
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            "http://www.smpte-ra.org/schemas/429-7/2006/CPL"
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => "",
    }
}

fn validate_build_request(
    root: &Path,
    request: &ProfessionalPackageBuildRequest,
) -> Result<(), ProfessionalPackageError> {
    package_names(request.profile)?;
    if request.duration == 0 {
        return Err(ProfessionalPackageError::InvalidGraph(
            "composition duration must be non-zero".to_owned(),
        ));
    }
    if request.picture.role != PackageAssetRole::PictureTrack
        || request
            .audio
            .as_ref()
            .is_some_and(|track| track.role != PackageAssetRole::AudioTrack)
    {
        return Err(ProfessionalPackageError::InvalidGraph(
            "track roles do not match picture/audio slots".to_owned(),
        ));
    }
    let audio = request.audio.as_ref().ok_or_else(|| {
        ProfessionalPackageError::InvalidGraph(
            "professional package requires primary audio".to_owned(),
        )
    })?;
    if request.picture.id == audio.id || request.picture.path == audio.path {
        return Err(ProfessionalPackageError::InvalidGraph(
            "picture and audio Track Files must have distinct identities and paths".to_owned(),
        ));
    }
    match request.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            let picture = request.picture.imf.as_ref().ok_or_else(|| {
                ProfessionalPackageError::InvalidGraph(
                    "IMF picture descriptor evidence is required".to_owned(),
                )
            })?;
            let audio = audio.imf.as_ref().ok_or_else(|| {
                ProfessionalPackageError::InvalidGraph(
                    "IMF audio descriptor evidence is required".to_owned(),
                )
            })?;
            if picture.edit_rate != request.edit_rate
                || picture.source_duration != request.duration
                || picture.intrinsic_duration < picture.source_duration
            {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF picture timing does not close over the composition".to_owned(),
                ));
            }
            if audio.edit_rate != Rational::new(48_000, 1)
                || audio.source_duration == 0
                || audio.intrinsic_duration < audio.source_duration
            {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF audio timing is not 48 kHz or exceeds intrinsic duration".to_owned(),
                ));
            }
            let audio_span = i128::from(audio.source_duration)
                * i128::from(request.edit_rate.num)
                * i128::from(audio.edit_rate.den);
            let picture_span = i128::from(request.duration)
                * i128::from(audio.edit_rate.num)
                * i128::from(request.edit_rate.den);
            if audio_span != picture_span {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF picture and audio resources have different durations".to_owned(),
                ));
            }
            validated_regxml_fragment(&picture.essence_descriptor_xml)?;
            validated_regxml_fragment(&audio.essence_descriptor_xml)?;
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            if request.picture.imf.is_some() || audio.imf.is_some() {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "DCP Track Files cannot carry IMF descriptor evidence".to_owned(),
                ));
            }
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            return Err(ProfessionalPackageError::UnsupportedProfile(
                request.profile,
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|source| ProfessionalPackageError::Io { path: root.to_path_buf(), source })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProfessionalPackageError::InvalidInventory(
            "package root is not a direct directory".to_owned(),
        ));
    }
    Ok(())
}

fn evidence_for_track(
    root: &Path,
    track: &ProfessionalPackageTrack,
) -> Result<PackageAssetEvidence, ProfessionalPackageError> {
    evidence_for_path(root, track.id, track.path.clone(), track.role)
}

fn evidence_for_xml(
    root: &Path,
    id: PackageAssetId,
    name: &str,
    role: PackageAssetRole,
) -> Result<PackageAssetEvidence, ProfessionalPackageError> {
    evidence_for_path(root, id, PackageRelativePath::new(name)?, role)
}

fn evidence_for_path(
    root: &Path,
    id: PackageAssetId,
    path: PackageRelativePath,
    role: PackageAssetRole,
) -> Result<PackageAssetEvidence, ProfessionalPackageError> {
    let absolute = path.join(root);
    let metadata = std::fs::symlink_metadata(&absolute)
        .map_err(|source| ProfessionalPackageError::Io { path: absolute.clone(), source })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ProfessionalPackageError::InvalidInventory(format!(
            "{} is not a direct regular file",
            path.as_str()
        )));
    }
    let bytes = std::fs::read(&absolute)
        .map_err(|source| ProfessionalPackageError::Io { path: absolute, source })?;
    let digest = Sha1::digest(&bytes);
    Ok(PackageAssetEvidence {
        id,
        path,
        role,
        size: metadata.len(),
        digest: PackageDigest {
            algorithm: PackageDigestAlgorithm::Sha1,
            base64: STANDARD.encode(digest),
        },
    })
}

fn build_asset_map_xml(
    request: &ProfessionalPackageBuildRequest,
    assets: &[PackageAssetEvidence],
) -> Result<String, ProfessionalPackageError> {
    let annotation = match request.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => "Mondrian IMF package",
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => "Mondrian SMPTE DCP",
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            return Err(ProfessionalPackageError::UnsupportedProfile(
                request.profile,
            ));
        }
    };
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<AssetMap xmlns=\"{}\"><Id>{}</Id><AnnotationText>{}</AnnotationText><Creator>{}</Creator><VolumeCount>1</VolumeCount><IssueDate>{}</IssueDate><Issuer>{}</Issuer><AssetList>",
        asset_map_namespace(),
        request.document_ids.asset_map.urn(),
        escape_xml(annotation),
        escape_xml(&request.metadata.creator),
        xml_issue_date(request.issued_at),
        escape_xml(&request.metadata.issuer),
    );
    for asset in assets {
        xml.push_str(&format!(
            "<Asset><Id>{}</Id><PackingList>{}</PackingList><ChunkList><Chunk><Path>{}</Path><VolumeIndex>1</VolumeIndex><Offset>0</Offset><Length>{}</Length></Chunk></ChunkList></Asset>",
            asset.id.urn(),
            if asset.role == PackageAssetRole::PackingList { "true" } else { "false" },
            escape_xml(asset.path.as_str()),
            asset.size
        ));
    }
    xml.push_str("</AssetList></AssetMap>\n");
    Ok(xml)
}

fn build_pkl_xml(
    request: &ProfessionalPackageBuildRequest,
    assets: &[PackageAssetEvidence],
) -> Result<String, ProfessionalPackageError> {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<PackingList xmlns=\"{}\"><Id>{}</Id><AnnotationText>{}</AnnotationText><IssueDate>{}</IssueDate><Issuer>{}</Issuer><Creator>{}</Creator><AssetList>",
        pkl_namespace(request.profile),
        request.packing_list_id.urn(),
        escape_xml(&request.metadata.title),
        xml_issue_date(request.issued_at),
        escape_xml(&request.metadata.issuer),
        escape_xml(&request.metadata.creator)
    );
    for asset in assets {
        let hash_algorithm =
            if request.profile == ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 {
                "<HashAlgorithm Algorithm=\"http://www.w3.org/2000/09/xmldsig#sha1\"/>"
            } else {
                ""
            };
        xml.push_str(&format!(
            "<Asset><Id>{}</Id><Hash>{}</Hash><Size>{}</Size><Type>{}</Type><OriginalFileName>{}</OriginalFileName>{hash_algorithm}</Asset>",
            asset.id.urn(),
            asset.digest.base64,
            asset.size,
            if asset.role == PackageAssetRole::CompositionPlaylist { "text/xml" } else { "application/mxf" },
            escape_xml(asset.path.as_str())
        ));
    }
    xml.push_str("</AssetList></PackingList>\n");
    Ok(xml)
}

fn build_cpl_xml(
    request: &ProfessionalPackageBuildRequest,
    picture: &PackageAssetEvidence,
    audio: Option<&PackageAssetEvidence>,
) -> Result<String, ProfessionalPackageError> {
    match request.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            build_imf_rdd45_cpl(request, picture, audio)
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            build_smpte_dcp_cpl(request, picture, audio)
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => Err(
            ProfessionalPackageError::UnsupportedProfile(request.profile),
        ),
    }
}

fn build_imf_rdd45_cpl(
    request: &ProfessionalPackageBuildRequest,
    picture: &PackageAssetEvidence,
    audio: Option<&PackageAssetEvidence>,
) -> Result<String, ProfessionalPackageError> {
    let picture_track = request.picture.imf.as_ref().ok_or_else(|| {
        ProfessionalPackageError::InvalidGraph(
            "IMF picture Track File lacks extracted descriptor evidence".to_owned(),
        )
    })?;
    let audio_evidence = audio.ok_or_else(|| {
        ProfessionalPackageError::InvalidGraph("IMF package requires primary audio".to_owned())
    })?;
    let audio_track =
        request.audio.as_ref().and_then(|track| track.imf.as_ref()).ok_or_else(|| {
            ProfessionalPackageError::InvalidGraph(
                "IMF audio Track File lacks extracted descriptor evidence".to_owned(),
            )
        })?;
    let picture_descriptor = validated_regxml_fragment(&picture_track.essence_descriptor_xml)?;
    let audio_descriptor = validated_regxml_fragment(&audio_track.essence_descriptor_xml)?;
    let rate = format!("{} {}", request.edit_rate.num, request.edit_rate.den);
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CompositionPlaylist xmlns=\"{}\" xmlns:cc=\"http://www.smpte-ra.org/ns/2067-2/2020\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><Id>{}</Id><Annotation>{}</Annotation><IssueDate>{}</IssueDate><Issuer>{}</Issuer><Creator>{}</Creator><ContentOriginator>{}</ContentOriginator><ContentTitle>{}</ContentTitle><ContentKind>feature</ContentKind><ContentVersionList><ContentVersion><Id>{}</Id><LabelText>{}</LabelText></ContentVersion></ContentVersionList><EssenceDescriptorList><EssenceDescriptor><Id>{}</Id>{}</EssenceDescriptor><EssenceDescriptor><Id>{}</Id>{}</EssenceDescriptor></EssenceDescriptorList><EditRate>{rate}</EditRate><LocaleList><Locale><LanguageList><Language>{}</Language></LanguageList></Locale></LocaleList><ExtensionProperties><cc:ApplicationIdentification>tag:apple.com,2017:imf:rdd45:2022</cc:ApplicationIdentification></ExtensionProperties><SegmentList><Segment><Id>{}</Id><SequenceList><cc:MainImageSequence><Id>{}</Id><TrackId>{}</TrackId><ResourceList><Resource xsi:type=\"TrackFileResourceType\"><Id>{}</Id><EditRate>{} {}</EditRate><IntrinsicDuration>{}</IntrinsicDuration><EntryPoint>0</EntryPoint><SourceDuration>{}</SourceDuration><SourceEncoding>{}</SourceEncoding><TrackFileId>{}</TrackFileId><Hash>{}</Hash><HashAlgorithm Algorithm=\"http://www.w3.org/2000/09/xmldsig#sha1\"/></Resource></ResourceList></cc:MainImageSequence>",
        cpl_namespace(request.profile),
        request.composition_id.urn(),
        escape_xml(&request.metadata.title),
        xml_issue_date(request.issued_at),
        escape_xml(&request.metadata.issuer),
        escape_xml(&request.metadata.creator),
        escape_xml(&request.metadata.issuer),
        escape_xml(&request.metadata.title),
        request.document_ids.content_version.urn(),
        escape_xml(&request.metadata.title),
        picture_track.essence_descriptor_id.urn(),
        picture_descriptor,
        audio_track.essence_descriptor_id.urn(),
        audio_descriptor,
        escape_xml(&request.metadata.language),
        request.document_ids.segment_or_reel.urn(),
        request.document_ids.picture_sequence.urn(),
        request.document_ids.picture_virtual_track.urn(),
        request.document_ids.picture_resource.urn(),
        picture_track.edit_rate.num,
        picture_track.edit_rate.den,
        picture_track.intrinsic_duration,
        picture_track.source_duration,
        picture_track.essence_descriptor_id.urn(),
        picture.id.urn(),
        picture.digest.base64,
    );
    xml.push_str(&format!(
        "<cc:MainAudioSequence><Id>{}</Id><TrackId>{}</TrackId><ResourceList><Resource xsi:type=\"TrackFileResourceType\"><Id>{}</Id><EditRate>{} {}</EditRate><IntrinsicDuration>{}</IntrinsicDuration><EntryPoint>0</EntryPoint><SourceDuration>{}</SourceDuration><SourceEncoding>{}</SourceEncoding><TrackFileId>{}</TrackFileId><Hash>{}</Hash><HashAlgorithm Algorithm=\"http://www.w3.org/2000/09/xmldsig#sha1\"/></Resource></ResourceList></cc:MainAudioSequence></SequenceList></Segment></SegmentList></CompositionPlaylist>\n",
        request.document_ids.audio_sequence.urn(),
        request.document_ids.audio_virtual_track.urn(),
        request.document_ids.audio_resource.urn(),
        audio_track.edit_rate.num,
        audio_track.edit_rate.den,
        audio_track.intrinsic_duration,
        audio_track.source_duration,
        audio_track.essence_descriptor_id.urn(),
        audio_evidence.id.urn(),
        audio_evidence.digest.base64,
    ));
    Ok(xml)
}

fn build_smpte_dcp_cpl(
    request: &ProfessionalPackageBuildRequest,
    picture: &PackageAssetEvidence,
    audio: Option<&PackageAssetEvidence>,
) -> Result<String, ProfessionalPackageError> {
    let audio = audio.ok_or_else(|| {
        ProfessionalPackageError::InvalidGraph("SMPTE DCP requires primary audio".to_owned())
    })?;
    let rate = format!("{} {}", request.edit_rate.num, request.edit_rate.den);
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CompositionPlaylist xmlns=\"{}\"><Id>{}</Id><AnnotationText>{}</AnnotationText><IssueDate>{}</IssueDate><Issuer>{}</Issuer><Creator>{}</Creator><ContentTitleText>{}</ContentTitleText><ContentKind>feature</ContentKind><ContentVersion><Id>{}</Id><LabelText>{}</LabelText></ContentVersion><RatingList/><ReelList><Reel><Id>{}</Id><AssetList><MainPicture><Id>{}</Id><EditRate>{rate}</EditRate><IntrinsicDuration>{}</IntrinsicDuration><EntryPoint>0</EntryPoint><Duration>{}</Duration><Hash>{}</Hash><FrameRate>{rate}</FrameRate><ScreenAspectRatio>1998 1080</ScreenAspectRatio></MainPicture><MainSound><Id>{}</Id><EditRate>{rate}</EditRate><IntrinsicDuration>{}</IntrinsicDuration><EntryPoint>0</EntryPoint><Duration>{}</Duration><Hash>{}</Hash><Language>{}</Language></MainSound></AssetList></Reel></ReelList></CompositionPlaylist>\n",
        cpl_namespace(request.profile),
        request.composition_id.urn(),
        escape_xml(&request.metadata.title),
        xml_issue_date(request.issued_at),
        escape_xml(&request.metadata.issuer),
        escape_xml(&request.metadata.creator),
        escape_xml(&request.metadata.title),
        request.document_ids.content_version.urn(),
        escape_xml(&request.metadata.title),
        request.document_ids.segment_or_reel.urn(),
        picture.id.urn(),
        request.duration,
        request.duration,
        picture.digest.base64,
        audio.id.urn(),
        request.duration,
        request.duration,
        audio.digest.base64,
        escape_xml(&request.metadata.language),
    ))
}

fn xml_issue_date(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn validated_regxml_fragment(value: &str) -> Result<&str, ProfessionalPackageError> {
    if value.len() as u64 > MAX_XML_BYTES {
        return Err(ProfessionalPackageError::InvalidXml(
            "RegXML descriptor exceeds the 2 MiB bound".to_owned(),
        ));
    }
    let upper = value.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(ProfessionalPackageError::InvalidXml(
            "DTD and entity declarations are prohibited in RegXML".to_owned(),
        ));
    }
    let fragment = if value.trim_start().starts_with("<?xml") {
        value
            .find("?>")
            .map(|end| &value[end + 2..])
            .ok_or_else(|| {
                ProfessionalPackageError::InvalidXml("unterminated XML declaration".to_owned())
            })?
            .trim()
    } else {
        value.trim()
    };
    let document = Document::parse(fragment)
        .map_err(|error| ProfessionalPackageError::InvalidXml(error.to_string()))?;
    if document.root_element().tag_name().namespace().is_none() {
        return Err(ProfessionalPackageError::InvalidXml(
            "RegXML descriptor root must use a registered namespace".to_owned(),
        ));
    }
    Ok(fragment)
}

trait ParsedPackageId: Sized {
    fn parse_urn(value: &str) -> Result<Self, ProfessionalPackageError>;
}

impl ParsedPackageId for CompositionPlaylistId {
    fn parse_urn(value: &str) -> Result<Self, ProfessionalPackageError> {
        Self::parse(value)
    }
}

impl ParsedPackageId for PackageAssetId {
    fn parse_urn(value: &str) -> Result<Self, ProfessionalPackageError> {
        Self::parse(value)
    }
}

impl ParsedPackageId for PackageElementId {
    fn parse_urn(value: &str) -> Result<Self, ProfessionalPackageError> {
        Self::parse(value)
    }
}

impl ParsedPackageId for PackingListId {
    fn parse_urn(value: &str) -> Result<Self, ProfessionalPackageError> {
        Self::parse(value)
    }
}

fn parse_root_id<T: ParsedPackageId>(
    document: &Document<'_>,
) -> Result<T, ProfessionalPackageError> {
    let value = direct_child_text(document.root_element(), "Id")?;
    T::parse_urn(value)
}

fn collect_asset_map_paths(
    document: &Document<'_>,
) -> Result<BTreeMap<PackageAssetId, AssetMapEntry>, ProfessionalPackageError> {
    if direct_child_text(document.root_element(), "VolumeCount")? != "1" {
        return Err(ProfessionalPackageError::InvalidGraph(
            "AssetMap must describe exactly one volume".to_owned(),
        ));
    }
    require_non_empty_direct_text(document.root_element(), "IssueDate")?;
    require_non_empty_direct_text(document.root_element(), "Issuer")?;
    require_non_empty_direct_text(document.root_element(), "Creator")?;
    let asset_list = direct_child(document.root_element(), "AssetList")?;
    let mut result = BTreeMap::new();
    let mut folded_paths = BTreeSet::new();
    for asset in asset_list
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "Asset")
    {
        let id = PackageAssetId::parse(direct_child_text(asset, "Id")?)?;
        let packing_list = match direct_child_text(asset, "PackingList")? {
            "true" => true,
            "false" => false,
            _ => {
                return Err(ProfessionalPackageError::InvalidXml(
                    "AssetMap PackingList is not a canonical boolean".to_owned(),
                ));
            }
        };
        let chunk_list = direct_child(asset, "ChunkList")?;
        let chunks = chunk_list
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "Chunk")
            .collect::<Vec<_>>();
        if chunks.len() != 1 {
            return Err(ProfessionalPackageError::InvalidGraph(
                "each AssetMap asset must contain exactly one direct chunk".to_owned(),
            ));
        }
        let chunk = chunks[0];
        let path = PackageRelativePath::new(direct_child_text(chunk, "Path")?)?;
        if direct_child_text(chunk, "VolumeIndex")? != "1"
            || direct_child_text(chunk, "Offset")? != "0"
        {
            return Err(ProfessionalPackageError::InvalidGraph(
                "AssetMap chunks must be whole direct files on volume 1".to_owned(),
            ));
        }
        let length = parse_positive_u64(direct_child_text(chunk, "Length")?, "AssetMap length")?;
        if !folded_paths.insert(path.as_str().to_ascii_lowercase()) {
            return Err(ProfessionalPackageError::InvalidGraph(
                "duplicate or case-aliasing AssetMap path".to_owned(),
            ));
        }
        if result.insert(id, AssetMapEntry { path, length, packing_list }).is_some() {
            return Err(ProfessionalPackageError::InvalidGraph(
                "duplicate AssetMap asset ID".to_owned(),
            ));
        }
    }
    if result.is_empty() || result.len() > MAX_PACKAGE_ASSETS {
        return Err(ProfessionalPackageError::InvalidInventory(
            "AssetMap asset count is outside the bounded package inventory".to_owned(),
        ));
    }
    Ok(result)
}

fn collect_pkl_assets(
    document: &Document<'_>,
) -> Result<BTreeMap<PackageAssetId, PackingListEntry>, ProfessionalPackageError> {
    require_non_empty_direct_text(document.root_element(), "IssueDate")?;
    require_non_empty_direct_text(document.root_element(), "Issuer")?;
    require_non_empty_direct_text(document.root_element(), "Creator")?;
    let asset_list = direct_child(document.root_element(), "AssetList")?;
    let mut result = BTreeMap::new();
    let mut folded_paths = BTreeSet::new();
    for asset in asset_list
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "Asset")
    {
        let id = PackageAssetId::parse(direct_child_text(asset, "Id")?)?;
        let size = parse_positive_u64(direct_child_text(asset, "Size")?, "PKL size")?;
        let hash = direct_child_text(asset, "Hash")?.to_owned();
        validate_sha1_base64(&hash)?;
        let path = PackageRelativePath::new(direct_child_text(asset, "OriginalFileName")?)?;
        let media_type = direct_child_text(asset, "Type")?.to_owned();
        if document.root_element().tag_name().namespace()
            == Some(pkl_namespace(
                ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25,
            ))
        {
            let algorithm = direct_child(asset, "HashAlgorithm")?;
            if algorithm.attribute("Algorithm") != Some("http://www.w3.org/2000/09/xmldsig#sha1") {
                return Err(ProfessionalPackageError::InvalidXml(
                    "IMF PKL uses an unsupported hash algorithm".to_owned(),
                ));
            }
        }
        if !folded_paths.insert(path.as_str().to_ascii_lowercase()) {
            return Err(ProfessionalPackageError::InvalidGraph(
                "duplicate or case-aliasing PKL path".to_owned(),
            ));
        }
        if result.insert(id, PackingListEntry { size, hash, path, media_type }).is_some() {
            return Err(ProfessionalPackageError::InvalidGraph(
                "duplicate PKL asset ID".to_owned(),
            ));
        }
    }
    Ok(result)
}

fn collect_cpl_track_refs(
    document: &Document<'_>,
    profile: ProfessionalDeliveryProfile,
) -> Result<BTreeMap<PackageAssetId, CplTrackReference>, ProfessionalPackageError> {
    let mut result = BTreeMap::new();
    let references = match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => [
            (
                "MainImageSequence",
                "TrackFileId",
                PackageAssetRole::PictureTrack,
            ),
            (
                "MainAudioSequence",
                "TrackFileId",
                PackageAssetRole::AudioTrack,
            ),
        ],
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => [
            ("MainPicture", "Id", PackageAssetRole::PictureTrack),
            ("MainSound", "Id", PackageAssetRole::AudioTrack),
        ],
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            return Err(ProfessionalPackageError::UnsupportedProfile(profile));
        }
    };
    for (container_name, id_name, role) in references {
        for container in document.descendants().filter(|node| node.has_tag_name(container_name)) {
            let id_text = if profile == ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 {
                descendant_text(container, id_name)?
            } else {
                direct_child_text(container, id_name)?
            };
            let id = PackageAssetId::parse(id_text)?;
            let hash = descendant_text(container, "Hash")?.to_owned();
            validate_sha1_base64(&hash)?;
            if result.insert(id, CplTrackReference { role, hash }).is_some() {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "CPL references a Track File more than once".to_owned(),
                ));
            }
        }
    }
    if result
        .values()
        .filter(|item| item.role == PackageAssetRole::PictureTrack)
        .count()
        != 1
        || result.values().filter(|item| item.role == PackageAssetRole::AudioTrack).count() != 1
    {
        return Err(ProfessionalPackageError::InvalidGraph(
            "CPL must reference exactly one picture and one primary audio Track File".to_owned(),
        ));
    }
    Ok(result)
}

fn validate_cpl_semantics(
    document: &Document<'_>,
    profile: ProfessionalDeliveryProfile,
) -> Result<(), ProfessionalPackageError> {
    let root = document.root_element();
    for name in ["IssueDate", "Issuer", "Creator"] {
        require_non_empty_direct_text(root, name)?;
    }
    match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            require_non_empty_direct_text(root, "ContentTitle")?;
            let root_rate = parse_rate(direct_child_text(root, "EditRate")?, "IMF CPL EditRate")?;
            if root_rate != (25, 1) {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF CPL root EditRate is not 25/1".to_owned(),
                ));
            }
            let applications = document
                .descendants()
                .filter(|node| {
                    node.is_element()
                        && node.tag_name().name() == "ApplicationIdentification"
                        && node.tag_name().namespace()
                            == Some("http://www.smpte-ra.org/ns/2067-2/2020")
                })
                .collect::<Vec<_>>();
            if applications.len() != 1
                || applications[0].text().map(str::trim)
                    != Some("tag:apple.com,2017:imf:rdd45:2022")
            {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF CPL must carry exactly the pinned RDD 45 application identity".to_owned(),
                ));
            }
            let descriptor_list = direct_child(root, "EssenceDescriptorList")?;
            let mut descriptor_ids = BTreeSet::new();
            for descriptor in descriptor_list
                .children()
                .filter(|node| node.is_element() && node.tag_name().name() == "EssenceDescriptor")
            {
                descriptor_ids.insert(PackageElementId::parse(direct_child_text(
                    descriptor, "Id",
                )?)?);
            }
            if descriptor_ids.len() != 2 {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF CPL must contain exactly two distinct Essence Descriptors".to_owned(),
                ));
            }
            let picture = exactly_one_descendant(document, "MainImageSequence")?;
            let audio = exactly_one_descendant(document, "MainAudioSequence")?;
            let picture_resource = exactly_one_descendant_of(picture, "Resource")?;
            let audio_resource = exactly_one_descendant_of(audio, "Resource")?;
            for resource in [picture_resource, audio_resource] {
                let source_encoding =
                    PackageElementId::parse(descendant_text(resource, "SourceEncoding")?)?;
                if !descriptor_ids.contains(&source_encoding) {
                    return Err(ProfessionalPackageError::InvalidGraph(
                        "IMF Track File resource SourceEncoding is outside EssenceDescriptorList"
                            .to_owned(),
                    ));
                }
                let algorithm = exactly_one_descendant_of(resource, "HashAlgorithm")?;
                if algorithm.attribute("Algorithm")
                    != Some("http://www.w3.org/2000/09/xmldsig#sha1")
                {
                    return Err(ProfessionalPackageError::InvalidXml(
                        "IMF CPL resource uses an unsupported hash algorithm".to_owned(),
                    ));
                }
            }
            let picture_rate = parse_rate(
                descendant_text(picture_resource, "EditRate")?,
                "IMF picture EditRate",
            )?;
            let audio_rate = parse_rate(
                descendant_text(audio_resource, "EditRate")?,
                "IMF audio EditRate",
            )?;
            if picture_rate != (25, 1) || audio_rate != (48_000, 1) {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF resource edit rates do not match the pinned picture/audio rates"
                        .to_owned(),
                ));
            }
            let picture_duration = validate_resource_duration(picture_resource, "IMF picture")?;
            let audio_duration = validate_resource_duration(audio_resource, "IMF audio")?;
            if u128::from(picture_duration) * u128::from(audio_rate.0) * u128::from(picture_rate.1)
                != u128::from(audio_duration)
                    * u128::from(picture_rate.0)
                    * u128::from(audio_rate.1)
            {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "IMF picture and audio CPL resources have different durations".to_owned(),
                ));
            }
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            require_non_empty_direct_text(root, "ContentTitleText")?;
            require_non_empty_direct_text(root, "ContentKind")?;
            let content_version = direct_child(root, "ContentVersion")?;
            PackageElementId::parse(direct_child_text(content_version, "Id")?)?;
            require_non_empty_direct_text(content_version, "LabelText")?;
            direct_child(root, "RatingList")?;
            let reel_list = direct_child(root, "ReelList")?;
            let reels = reel_list
                .children()
                .filter(|node| node.is_element() && node.tag_name().name() == "Reel")
                .collect::<Vec<_>>();
            if reels.len() != 1 {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "DCP CPL must contain exactly one Reel".to_owned(),
                ));
            }
            let picture = exactly_one_descendant_of(reels[0], "MainPicture")?;
            let audio = exactly_one_descendant_of(reels[0], "MainSound")?;
            for asset in [picture, audio] {
                if parse_rate(direct_child_text(asset, "EditRate")?, "DCP EditRate")? != (24, 1) {
                    return Err(ProfessionalPackageError::InvalidGraph(
                        "DCP CPL resource EditRate is not 24/1".to_owned(),
                    ));
                }
            }
            if direct_child_text(picture, "FrameRate")? != "24 1"
                || direct_child_text(picture, "ScreenAspectRatio")? != "1998 1080"
            {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "DCP picture signaling is not the pinned 2K Flat 24 profile".to_owned(),
                ));
            }
            let picture_duration = validate_dcp_asset_duration(picture, "DCP picture")?;
            let audio_duration = validate_dcp_asset_duration(audio, "DCP audio")?;
            if picture_duration != audio_duration {
                return Err(ProfessionalPackageError::InvalidGraph(
                    "DCP picture and audio assets have different durations".to_owned(),
                ));
            }
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            return Err(ProfessionalPackageError::UnsupportedProfile(profile));
        }
    }
    Ok(())
}

fn validate_resource_duration(
    resource: roxmltree::Node<'_, '_>,
    label: &str,
) -> Result<u64, ProfessionalPackageError> {
    let intrinsic = parse_positive_u64(descendant_text(resource, "IntrinsicDuration")?, label)?;
    let source = parse_positive_u64(descendant_text(resource, "SourceDuration")?, label)?;
    if descendant_text(resource, "EntryPoint")? != "0" || source > intrinsic {
        return Err(ProfessionalPackageError::InvalidGraph(format!(
            "{label} resource duration exceeds its intrinsic span"
        )));
    }
    Ok(source)
}

fn validate_dcp_asset_duration(
    asset: roxmltree::Node<'_, '_>,
    label: &str,
) -> Result<u64, ProfessionalPackageError> {
    let intrinsic = parse_positive_u64(direct_child_text(asset, "IntrinsicDuration")?, label)?;
    let duration = parse_positive_u64(direct_child_text(asset, "Duration")?, label)?;
    if direct_child_text(asset, "EntryPoint")? != "0" || duration > intrinsic {
        return Err(ProfessionalPackageError::InvalidGraph(format!(
            "{label} duration exceeds its intrinsic span"
        )));
    }
    Ok(duration)
}

fn exactly_one_descendant<'a>(
    document: &'a Document<'a>,
    name: &str,
) -> Result<roxmltree::Node<'a, 'a>, ProfessionalPackageError> {
    exactly_one_descendant_of(document.root_element(), name)
}

fn exactly_one_descendant_of<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &str,
) -> Result<roxmltree::Node<'a, 'a>, ProfessionalPackageError> {
    let matches = node
        .descendants()
        .filter(|child| child.is_element() && child.tag_name().name() == name)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(ProfessionalPackageError::InvalidGraph(format!(
            "expected exactly one {name}, found {}",
            matches.len()
        )));
    }
    Ok(matches[0])
}

fn parse_rate(value: &str, label: &str) -> Result<(u64, u64), ProfessionalPackageError> {
    let values = value
        .split_ascii_whitespace()
        .map(|part| part.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            ProfessionalPackageError::InvalidXml(format!("invalid {label}: {error}"))
        })?;
    if values.len() != 2 || values[0] == 0 || values[1] == 0 {
        return Err(ProfessionalPackageError::InvalidXml(format!(
            "invalid {label} rational"
        )));
    }
    Ok((values[0], values[1]))
}

fn parse_positive_u64(value: &str, label: &str) -> Result<u64, ProfessionalPackageError> {
    let value = value.parse::<u64>().map_err(|error| {
        ProfessionalPackageError::InvalidXml(format!("invalid {label}: {error}"))
    })?;
    if value == 0 {
        return Err(ProfessionalPackageError::InvalidXml(format!(
            "{label} must be positive"
        )));
    }
    Ok(value)
}

fn validate_sha1_base64(value: &str) -> Result<(), ProfessionalPackageError> {
    let bytes = STANDARD.decode(value).map_err(|error| {
        ProfessionalPackageError::InvalidXml(format!("invalid Base64 SHA-1 digest: {error}"))
    })?;
    if bytes.len() != 20 {
        return Err(ProfessionalPackageError::InvalidXml(
            "SHA-1 digest is not exactly 20 bytes".to_owned(),
        ));
    }
    Ok(())
}

fn parse_xml<'a>(
    text: &'a str,
    root_name: &str,
    namespace: &str,
) -> Result<Document<'a>, ProfessionalPackageError> {
    let upper = text.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(ProfessionalPackageError::InvalidXml(
            "DTD and entity declarations are prohibited".to_owned(),
        ));
    }
    let document = Document::parse(text)
        .map_err(|error| ProfessionalPackageError::InvalidXml(error.to_string()))?;
    let root = document.root_element();
    if root.tag_name().name() != root_name || root.tag_name().namespace() != Some(namespace) {
        return Err(ProfessionalPackageError::InvalidXml(format!(
            "unexpected root element {{{:?}}}{}",
            root.tag_name().namespace(),
            root.tag_name().name(),
        )));
    }
    Ok(document)
}

fn direct_child<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &str,
) -> Result<roxmltree::Node<'a, 'a>, ProfessionalPackageError> {
    let matches = node
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == name)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(ProfessionalPackageError::InvalidXml(format!(
            "expected exactly one direct {name}, found {}",
            matches.len()
        )));
    }
    Ok(matches[0])
}

fn require_non_empty_direct_text<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &str,
) -> Result<&'a str, ProfessionalPackageError> {
    let value = direct_child_text(node, name)?;
    if value.trim().is_empty() {
        return Err(ProfessionalPackageError::InvalidXml(format!(
            "{name} must not be empty"
        )));
    }
    Ok(value)
}

fn direct_child_text<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &str,
) -> Result<&'a str, ProfessionalPackageError> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .and_then(|child| child.text())
        .ok_or_else(|| ProfessionalPackageError::InvalidXml(format!("missing {name}")))
}

fn descendant_text<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &str,
) -> Result<&'a str, ProfessionalPackageError> {
    node.descendants()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .and_then(|child| child.text())
        .ok_or_else(|| ProfessionalPackageError::InvalidXml(format!("missing {name}")))
}

fn read_bounded_xml(path: &Path) -> Result<String, ProfessionalPackageError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| ProfessionalPackageError::Io { path: path.to_path_buf(), source })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_XML_BYTES {
        return Err(ProfessionalPackageError::InvalidXml(format!(
            "XML object is not a bounded direct file: {}",
            path.display()
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|source| ProfessionalPackageError::Io { path: path.to_path_buf(), source })
}

fn write_new(path: PathBuf, bytes: &[u8]) -> Result<(), ProfessionalPackageError> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| ProfessionalPackageError::Io { path: path.clone(), source })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| ProfessionalPackageError::Io { path, source })
}

fn validate_closed_directory(
    root: &Path,
    expected: &[PackageAssetEvidence],
) -> Result<(), ProfessionalPackageError> {
    let expected: BTreeSet<_> =
        expected.iter().map(|asset| asset.path.as_str().to_owned()).collect();
    let mut actual = BTreeSet::new();
    for entry in std::fs::read_dir(root)
        .map_err(|source| ProfessionalPackageError::Io { path: root.to_path_buf(), source })?
    {
        let entry = entry
            .map_err(|source| ProfessionalPackageError::Io { path: root.to_path_buf(), source })?;
        let file_type = entry
            .file_type()
            .map_err(|source| ProfessionalPackageError::Io { path: entry.path(), source })?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(ProfessionalPackageError::InvalidInventory(format!(
                "non-file object in package: {}",
                entry.path().display()
            )));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            ProfessionalPackageError::InvalidInventory("non-UTF-8 package filename".to_owned())
        })?;
        actual.insert(name);
    }
    if expected != actual {
        return Err(ProfessionalPackageError::InvalidInventory(format!(
            "closed inventory mismatch: expected={expected:?}, actual={actual:?}"
        )));
    }
    Ok(())
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
