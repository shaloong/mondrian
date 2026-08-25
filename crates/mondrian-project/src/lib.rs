//! Project document and `.mdp` container contract.
//!
//! Mondrian uses a Premiere-style lightweight project file. The `.mdp`
//! archive stores the canonical project document and the project asset-library
//! index; generated caches, proxies, waveforms, and preview renders live
//! outside the project archive.

use anyhow::Context;
use mondrian_core::{
    AssetId, AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError,
    AuthoringSet, ProjectColorEnvironment, ProjectId, ProjectMeta, ProjectSettings, SequenceId,
};
use mondrian_storage::{
    FilePublicationFailure as StoragePublicationFailure, FilePublicationMode, OwnedPublicationFile,
};
use mondrian_timeline::{
    Sequence, SequenceAuthorContractCertificate, SequenceCollection, SequenceDependencyCertificate,
    SequenceSettings,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod migration;

use migration::JsonMigrationRegistry;

/// Current `.mdp` container format version.
pub const PROJECT_FORMAT_VERSION: u32 = 1;
/// Current canonical project document schema version.
pub const PROJECT_DOCUMENT_SCHEMA_VERSION: u32 = 25;
/// Current embedded asset-library SQLite schema version.
pub const PROJECT_LIBRARY_SCHEMA_VERSION: u32 = 5;

/// Final namespace semantics for one atomic Project archive publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectArchivePublication {
    /// Atomically replace an existing target, or create it when absent.
    ReplaceExisting,
    /// Atomically create the target and fail without changing it if any entry
    /// already occupies the destination.
    CreateNew,
}

/// Immutable point-in-time construction evidence returned after one exact
/// Project archive is durably published.
///
/// The fields are private so downstream code cannot manufacture publication
/// evidence from a path and author metadata alone. The content identity is
/// measured from the retained, entry-verified temporary file object before
/// the atomic namespace operation; this evidence is returned only after that
/// operation succeeds. It does not replace later path revalidation when the
/// archive is discovered or opened outside the publication lease.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectArchivePublicationEvidence {
    namespace: ProjectArchiveNamespacePublicationEvidence,
}

/// Point-in-time evidence that an atomic Project archive namespace operation
/// completed, without claiming that the containing directory was durably
/// synchronized.
///
/// This is intentionally a different type from
/// [`ProjectArchivePublicationEvidence`]. It is exposed only by
/// [`ProjectArchivePublicationDurabilityUnconfirmed`] after the irreversible
/// namespace boundary has been crossed and must never authorize a saved
/// baseline or Recovery Manifest.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectArchiveNamespacePublicationEvidence {
    published_path: PathBuf,
    publication: ProjectArchivePublication,
    project_id: ProjectId,
    document_revision: u64,
    archive_len: u64,
    archive_sha256: [u8; 32],
}

impl ProjectArchivePublicationEvidence {
    /// Exact target path supplied to the successful publication.
    pub fn published_path(&self) -> &Path {
        self.namespace.published_path()
    }

    /// Final namespace semantics used by the successful publication.
    pub fn publication(&self) -> ProjectArchivePublication {
        self.namespace.publication()
    }

    /// Project identity encoded by the entry-verified document.
    pub fn project_id(&self) -> ProjectId {
        self.namespace.project_id()
    }

    /// Durable document revision encoded by the entry-verified document.
    pub fn document_revision(&self) -> u64 {
        self.namespace.document_revision()
    }

    /// Exact byte length of the published archive object.
    pub fn archive_len(&self) -> u64 {
        self.namespace.archive_len()
    }

    /// SHA-256 identity of the complete published archive bytes.
    pub fn archive_sha256(&self) -> [u8; 32] {
        self.namespace.archive_sha256()
    }

    /// Lowercase hexadecimal SHA-256 identity for durable text manifests.
    pub fn archive_sha256_hex(&self) -> String {
        self.namespace.archive_sha256_hex()
    }
}

impl ProjectArchiveNamespacePublicationEvidence {
    /// Exact target path supplied to the namespace publication.
    pub fn published_path(&self) -> &Path {
        &self.published_path
    }

    /// Final namespace semantics used by the completed atomic operation.
    pub fn publication(&self) -> ProjectArchivePublication {
        self.publication
    }

    /// Project identity encoded by the entry-verified document.
    pub fn project_id(&self) -> ProjectId {
        self.project_id
    }

    /// Document revision encoded by the entry-verified document.
    pub fn document_revision(&self) -> u64 {
        self.document_revision
    }

    /// Exact byte length of the namespace-published archive object.
    pub fn archive_len(&self) -> u64 {
        self.archive_len
    }

    /// SHA-256 identity of the complete namespace-published archive bytes.
    pub fn archive_sha256(&self) -> [u8; 32] {
        self.archive_sha256
    }

    /// Lowercase hexadecimal SHA-256 identity for diagnostics.
    pub fn archive_sha256_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(64);
        for byte in self.archive_sha256 {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        value
    }
}

/// Error returned after a Project archive namespace operation completed but
/// synchronization of its containing directory failed.
///
/// The target may already name the new archive. Retrying `CreateNew` as though
/// publication never happened is therefore incorrect. The embedded namespace
/// evidence is deliberately not durable publication evidence.
#[derive(Debug)]
pub struct ProjectArchivePublicationDurabilityUnconfirmed {
    namespace: ProjectArchiveNamespacePublicationEvidence,
    source: mondrian_storage::FilePublicationDurabilityUnconfirmed,
}

impl ProjectArchivePublicationDurabilityUnconfirmed {
    /// Evidence for the completed namespace operation.
    pub fn namespace_evidence(&self) -> &ProjectArchiveNamespacePublicationEvidence {
        &self.namespace
    }
}

impl fmt::Display for ProjectArchivePublicationDurabilityUnconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Project archive namespace was published at {}, but the platform durability barrier was not confirmed: {}",
            self.namespace.published_path.display(),
            self.source
        )
    }
}

impl std::error::Error for ProjectArchivePublicationDurabilityUnconfirmed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Error returned when a Project archive namespace operation has an
/// indeterminate postcondition.
///
/// This carries identity facts for diagnostics and recovery tooling, but it is
/// not publication evidence and cannot authorize a saved baseline.
#[derive(Debug)]
pub struct ProjectArchivePublicationNamespaceIndeterminate {
    intended_path: PathBuf,
    publication: ProjectArchivePublication,
    project_id: ProjectId,
    document_revision: u64,
    archive_len: u64,
    archive_sha256: [u8; 32],
    source: mondrian_storage::FilePublicationNamespaceIndeterminate,
}

impl ProjectArchivePublicationNamespaceIndeterminate {
    /// Intended final target of the ambiguous namespace operation.
    pub fn intended_path(&self) -> &Path {
        &self.intended_path
    }

    /// Requested namespace semantics.
    pub fn publication(&self) -> ProjectArchivePublication {
        self.publication
    }

    /// Project identity encoded by the verified archive candidate.
    pub fn project_id(&self) -> ProjectId {
        self.project_id
    }

    /// Document revision encoded by the verified archive candidate.
    pub fn document_revision(&self) -> u64 {
        self.document_revision
    }

    /// Exact byte length of the verified archive candidate.
    pub fn archive_len(&self) -> u64 {
        self.archive_len
    }

    /// SHA-256 identity of the verified archive candidate.
    pub fn archive_sha256(&self) -> [u8; 32] {
        self.archive_sha256
    }

    /// Verified surviving source name for the new archive, when observed.
    pub fn retained_new_path(&self) -> Option<&Path> {
        self.source.retained_new_path()
    }
}

impl fmt::Display for ProjectArchivePublicationNamespaceIndeterminate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Project archive publication at {} has an indeterminate namespace postcondition; no durable publication evidence was issued",
            self.intended_path.display()
        )?;
        if let Some(path) = self.source.retained_new_path() {
            write!(
                formatter,
                " and verified new bytes remain at {}",
                path.display()
            )?;
        }
        write!(formatter, ": {}", self.source)
    }
}

impl std::error::Error for ProjectArchivePublicationNamespaceIndeterminate {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Exhaustive failure states for Project archive publication.
#[derive(Debug)]
pub enum ProjectArchivePublicationFailure {
    /// No irreversible namespace operation is known to have completed.
    BeforeNamespace(anyhow::Error),
    /// The target names the new archive, but crash durability is unconfirmed.
    DurabilityUnconfirmed(Box<ProjectArchivePublicationDurabilityUnconfirmed>),
    /// Object-identity postconditions cannot prove either unchanged or
    /// published state.
    NamespaceIndeterminate(Box<ProjectArchivePublicationNamespaceIndeterminate>),
}

impl fmt::Display for ProjectArchivePublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeNamespace(error) => write!(formatter, "{error:#}"),
            Self::DurabilityUnconfirmed(error) => error.fmt(formatter),
            Self::NamespaceIndeterminate(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProjectArchivePublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BeforeNamespace(error) => Some(error.as_ref()),
            Self::DurabilityUnconfirmed(error) => Some(error.as_ref()),
            Self::NamespaceIndeterminate(error) => Some(error.as_ref()),
        }
    }
}

impl From<anyhow::Error> for ProjectArchivePublicationFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::BeforeNamespace(error)
    }
}

/// Entry name for the archive manifest.
pub const MANIFEST_ENTRY: &str = "manifest.json";
/// Entry name for the canonical project document.
pub const PROJECT_ENTRY: &str = "project.json";
/// Entry name for the embedded project asset-library SQLite database.
pub const LIBRARY_ENTRY: &str = "library/index.db";

const PROJECT_FORMAT_NAME: &str = "mondrian-project";
const DOCUMENT_LAYOUT_SINGLE_JSON: &str = "single-project-json";
const ARCHIVE_MIGRATIONS: JsonMigrationRegistry = JsonMigrationRegistry::new(
    "project archive",
    "format_version",
    PROJECT_FORMAT_VERSION,
    &[],
);
const DOCUMENT_MIGRATIONS: JsonMigrationRegistry = JsonMigrationRegistry::new(
    "project document",
    "schema_version",
    PROJECT_DOCUMENT_SCHEMA_VERSION,
    &[],
);
const REQUIRED_ARCHIVE_ENTRIES: [&str; 3] = [MANIFEST_ENTRY, PROJECT_ENTRY, LIBRARY_ENTRY];

/// Admission limits for opening one untrusted `.mdp` archive.
///
/// These are resource budgets, not statements about media duration. Callers
/// may choose a tighter or larger budget for their execution environment. The
/// default admits the current 120-minute stress Project (about 578 MiB at its
/// largest recovery checkpoint) while retaining a hard sub-GiB JSON bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectArchiveReadBudget {
    /// Maximum compressed archive file length.
    pub max_archive_bytes: u64,
    /// Maximum uncompressed `manifest.json` length.
    pub max_manifest_bytes: u64,
    /// Maximum uncompressed `project.json` length.
    pub max_project_bytes: u64,
    /// Maximum uncompressed `library/index.db` length.
    pub max_library_bytes: u64,
}

impl ProjectArchiveReadBudget {
    /// Default balanced admission budget for ordinary product open.
    pub const DEFAULT: Self = Self {
        max_archive_bytes: 2 * 1024 * 1024 * 1024,
        max_manifest_bytes: 64 * 1024,
        max_project_bytes: 768 * 1024 * 1024,
        max_library_bytes: 1536 * 1024 * 1024,
    };

    fn entry_limit(self, entry_name: &str) -> anyhow::Result<u64> {
        match entry_name {
            MANIFEST_ENTRY => Ok(self.max_manifest_bytes),
            PROJECT_ENTRY => Ok(self.max_project_bytes),
            LIBRARY_ENTRY => Ok(self.max_library_bytes),
            _ => anyhow::bail!("archive admission requested for unknown entry: {entry_name}"),
        }
    }

    fn admit_archive_file(self, file: &fs::File) -> anyhow::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            anyhow::bail!("Project archive handle does not name a regular file object");
        }
        let observed = metadata.len();
        if observed > self.max_archive_bytes {
            anyhow::bail!(
                "project archive compressed length exceeds admission budget: {observed} > {}",
                self.max_archive_bytes
            );
        }
        Ok(())
    }

    fn admit_declared_entry(self, entry_name: &str, declared: u64) -> anyhow::Result<()> {
        let limit = self.entry_limit(entry_name)?;
        if declared > limit {
            anyhow::bail!(
                "project archive entry '{entry_name}' declared length exceeds admission budget: {declared} > {limit}"
            );
        }
        Ok(())
    }
}

impl Default for ProjectArchiveReadBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

struct BudgetedArchiveEntryReader<R> {
    inner: R,
    entry_name: &'static str,
    declared_len: u64,
    limit: u64,
    observed_len: u64,
    reached_eof: bool,
}

impl<R> BudgetedArchiveEntryReader<R> {
    fn new(inner: R, entry_name: &'static str, declared_len: u64, limit: u64) -> Self {
        Self {
            inner,
            entry_name,
            declared_len,
            limit,
            observed_len: 0,
            reached_eof: false,
        }
    }

    fn verify_complete(&self) -> anyhow::Result<()> {
        if !self.reached_eof {
            anyhow::bail!(
                "project archive entry '{}' was not read to EOF",
                self.entry_name
            );
        }
        if self.observed_len != self.declared_len {
            anyhow::bail!(
                "project archive entry '{}' actual length differs from ZIP declaration: {} != {}",
                self.entry_name,
                self.observed_len,
                self.declared_len
            );
        }
        Ok(())
    }
}

impl<R: Read> Read for BudgetedArchiveEntryReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = self.limit.saturating_sub(self.observed_len);
        if remaining == 0 {
            let mut probe = [0_u8; 1];
            let read = self.inner.read(&mut probe)?;
            if read == 0 {
                self.reached_eof = true;
                return Ok(0);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "project archive entry '{}' actual length exceeds admission budget {}",
                    self.entry_name, self.limit
                ),
            ));
        }
        let requested = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let admitted = usize::try_from(remaining.min(requested)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "archive entry admission length does not fit usize",
            )
        })?;
        let read = self.inner.read(&mut bytes[..admitted])?;
        if read == 0 {
            self.reached_eof = true;
            return Ok(0);
        }
        self.observed_len = self
            .observed_len
            .checked_add(u64::try_from(read).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "archive entry observed length does not fit u64",
                )
            })?)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "archive entry observed length overflowed u64",
                )
            })?;
        Ok(read)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchiveEntryEvidence {
    name: &'static str,
    uncompressed_len: u64,
    sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectArchiveWriteEvidence {
    manifest: ArchiveEntryEvidence,
    project: ArchiveEntryEvidence,
    library: ArchiveEntryEvidence,
}

impl ProjectArchiveWriteEvidence {
    fn entries(&self) -> [&ArchiveEntryEvidence; 3] {
        [&self.manifest, &self.project, &self.library]
    }
}

struct EvidenceWriter<'a, W> {
    inner: &'a mut W,
    hasher: Sha256,
    uncompressed_len: u64,
}

impl<'a, W> EvidenceWriter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self { inner, hasher: Sha256::new(), uncompressed_len: 0 }
    }

    fn finish(self, name: &'static str) -> ArchiveEntryEvidence {
        ArchiveEntryEvidence {
            name,
            uncompressed_len: self.uncompressed_len,
            sha256: self.hasher.finalize().into(),
        }
    }
}

impl<W: Write> Write for EvidenceWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.uncompressed_len = self
            .uncompressed_len
            .checked_add(u64::try_from(written).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "archive entry write length does not fit in u64",
                )
            })?)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "archive entry write length overflowed u64",
                )
            })?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// `.mdp` archive manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    /// Archive family identifier. Current value is `mondrian-project`.
    pub format: String,
    /// Container-level format version.
    pub format_version: u32,
    /// Document layout strategy within the archive.
    pub document_layout: String,
    /// Archive entry containing the canonical project document.
    pub project_entry: String,
    /// Archive entry containing the project asset-library SQLite database.
    pub library_entry: String,
    /// Expected SQLite schema version after runtime-copy migration.
    pub library_schema_version: u32,
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            format: PROJECT_FORMAT_NAME.to_string(),
            format_version: PROJECT_FORMAT_VERSION,
            document_layout: DOCUMENT_LAYOUT_SINGLE_JSON.to_string(),
            project_entry: PROJECT_ENTRY.to_string(),
            library_entry: LIBRARY_ENTRY.to_string(),
            library_schema_version: PROJECT_LIBRARY_SCHEMA_VERSION,
        }
    }
}

impl ProjectManifest {
    /// Validate that the archive is a supported alpha project format.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format != PROJECT_FORMAT_NAME {
            anyhow::bail!("unsupported project archive format: {}", self.format);
        }
        if self.format_version != PROJECT_FORMAT_VERSION {
            anyhow::bail!(
                "unsupported project archive version: {}",
                self.format_version
            );
        }
        if self.document_layout != DOCUMENT_LAYOUT_SINGLE_JSON {
            anyhow::bail!(
                "unsupported project document layout: {}",
                self.document_layout
            );
        }
        if self.project_entry != PROJECT_ENTRY {
            anyhow::bail!("unsupported project entry: {}", self.project_entry);
        }
        if self.library_entry != LIBRARY_ENTRY {
            anyhow::bail!("unsupported library entry: {}", self.library_entry);
        }
        if self.library_schema_version > PROJECT_LIBRARY_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported project library schema version: {}",
                self.library_schema_version
            );
        }
        Ok(())
    }
}

/// Canonical persisted project document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDocument {
    /// JSON document schema version.
    pub schema_version: u32,
    /// Stable project identity used by caches and external references.
    pub project_id: ProjectId,
    /// Monotonic revision advanced on explicit project saves.
    pub document_revision: u64,
    /// User-facing project metadata.
    pub meta: ProjectMeta,
    /// Settings whose semantics genuinely belong to the Project container.
    pub settings: ProjectSettings,
    /// Exact color engine shared by every Sequence in this Project.
    pub color_environment: ProjectColorEnvironment,
    /// Complete template copied into a Sequence when it is created.
    ///
    /// Existing Sequences never consult this value during validation,
    /// playback, rendering, or export.
    pub new_sequence_defaults: SequenceSettings,
    /// Complete sequence collection for the current single-document layout.
    pub sequences: SequenceCollection,
    /// Assets currently forced into proxy playback mode.
    pub proxy_mode_assets: AuthoringSet<AssetId>,
}

impl AuthoringFootprint for ProjectDocument {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            schema_version: _,
            project_id: _,
            document_revision: _,
            meta,
            settings,
            color_environment,
            new_sequence_defaults,
            sequences,
            proxy_mode_assets,
        } = self;
        collector.collect(meta)?;
        collector.collect(settings)?;
        collector.collect(color_environment)?;
        collector.collect(new_sequence_defaults)?;
        collector.collect(sequences)?;
        collector.collect(proxy_mode_assets)
    }
}

const PROJECT_AUTHORING_VALIDATION_CERTIFICATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
struct ProjectAuthoringValidationContext {
    project_id: ProjectId,
    color_environment: ProjectColorEnvironment,
}

impl ProjectAuthoringValidationContext {
    fn from_document(document: &ProjectDocument) -> Self {
        Self {
            project_id: document.project_id,
            color_environment: document.color_environment.clone(),
        }
    }
}

/// Opaque process-local proof that one Project's complete authoring contract
/// and cross-Sequence dependency closure were validated together.
///
/// The certificate is deliberately non-serializable and non-cloneable. It
/// retains per-Sequence immutable validation evidence plus the sole anchored
/// dependency certificate; the canonical [`ProjectDocument`] remains the only author
/// authority.
#[derive(Debug)]
pub struct ProjectAuthoringValidationCertificate {
    version: u32,
    context: ProjectAuthoringValidationContext,
    sequence_certificates: BTreeMap<SequenceId, Arc<SequenceAuthorContractCertificate>>,
    dependency_certificate: SequenceDependencyCertificate,
}

/// Fully prepared, infallibly installable Project state for one validated
/// Sequence replacement.
///
/// The document and certificate are kept together so the installed Sequence
/// collection and the dependency certificate retain the exact same outer COW
/// root. This ticket has no persistence representation and cannot be cloned.
#[derive(Debug)]
pub struct PreparedProjectSequenceReplacement {
    document: ProjectDocument,
    certificate: ProjectAuthoringValidationCertificate,
    replacement_index: usize,
}

impl PreparedProjectSequenceReplacement {
    /// Read the already-validated replacement for History preparation.
    pub fn replacement(&self) -> &Sequence {
        &self.document.sequences.sequences[self.replacement_index]
    }

    /// Consume this ticket into the only document/certificate pair it proves.
    ///
    /// All fallible validation and allocation happened before this call.
    pub fn into_installation(self) -> (ProjectDocument, ProjectAuthoringValidationCertificate) {
        (self.document, self.certificate)
    }
}

impl ProjectAuthoringValidationCertificate {
    /// Prepare a certificate for replacing exactly one existing Sequence.
    ///
    /// Success returns all evidence required for the same atomic installation
    /// boundary as the replacement document and History record.
    pub fn prepare_sequence_replacement(
        &self,
        current: &ProjectDocument,
        replacement: Sequence,
    ) -> anyhow::Result<PreparedProjectSequenceReplacement> {
        self.validate_current_baseline(current)?;
        let target_index = current
            .sequences
            .sequences
            .iter()
            .position(|sequence| sequence.id == replacement.id)
            .with_context(|| {
                format!(
                    "replacement Sequence does not exist in the current Project: {}",
                    replacement.id
                )
            })?;
        let mut next_sequences = current.sequences.sequences.clone();
        next_sequences[target_index] = replacement;
        let replacement = &next_sequences[target_index];
        let current_sequence = &current.sequences.sequences[target_index];
        let sequence_certificate = self
            .sequence_certificates
            .get(&replacement.id)
            .context("current Sequence has no author-contract certificate")?
            .prepare_replacement(current_sequence, replacement, &current.color_environment)
            .with_context(|| {
                format!(
                    "sequence '{}' replacement author contract is invalid",
                    replacement.name
                )
            })?;
        let dependency_certificate = self
            .dependency_certificate
            .prepare_replacement_with_baseline(&current.sequences, replacement, &next_sequences)
            .context("replacement Sequence dependency closure is invalid")?;
        let mut sequence_certificates = self.sequence_certificates.clone();
        sequence_certificates.insert(replacement.id, Arc::new(sequence_certificate));
        let certificate = Self {
            version: PROJECT_AUTHORING_VALIDATION_CERTIFICATE_VERSION,
            context: self.context.clone(),
            sequence_certificates,
            dependency_certificate,
        };
        let mut document = current.clone();
        document.sequences.sequences = next_sequences;
        Ok(PreparedProjectSequenceReplacement {
            document,
            certificate,
            replacement_index: target_index,
        })
    }

    /// Fully validate a prospective Project replacement after proving this
    /// certificate still belongs to the exact current author baseline.
    pub fn prepare_project_replacement(
        &self,
        current: &ProjectDocument,
        replacement: &ProjectDocument,
    ) -> anyhow::Result<Self> {
        self.validate_current_baseline(current)?;
        if replacement.project_id != current.project_id {
            anyhow::bail!(
                "Project replacement identity changed from {} to {}",
                current.project_id,
                replacement.project_id
            );
        }
        replacement.validate_document_contract()?;
        let sequence_evidence_unchanged = replacement.color_environment
            == current.color_environment
            && replacement.sequences.default_sequence_id == current.sequences.default_sequence_id
            && replacement.sequences.sequences == current.sequences.sequences;
        if sequence_evidence_unchanged {
            self.dependency_certificate
                .validate_baseline(&replacement.sequences)
                .context(
                    "Project-only replacement has an invalid active Sequence or stale dependency baseline",
                )?;
            return Ok(Self {
                version: PROJECT_AUTHORING_VALIDATION_CERTIFICATE_VERSION,
                context: ProjectAuthoringValidationContext::from_document(replacement),
                sequence_certificates: self.sequence_certificates.clone(),
                dependency_certificate: self.dependency_certificate.clone(),
            });
        }
        replacement.prepare_authoring_validation_certificate()
    }

    fn validate_current_baseline(&self, current: &ProjectDocument) -> anyhow::Result<()> {
        if self.version != PROJECT_AUTHORING_VALIDATION_CERTIFICATE_VERSION {
            anyhow::bail!("Project authoring-validation certificate version is unsupported");
        }
        current.validate_document_contract()?;
        if self.context != ProjectAuthoringValidationContext::from_document(current) {
            anyhow::bail!(
                "Project authoring-validation certificate does not match the current validation context"
            );
        }
        self.dependency_certificate.validate_baseline(&current.sequences).context(
            "Project dependency certificate does not match the current Sequence baseline",
        )?;
        if self.sequence_certificates.len() != current.sequences.sequences.len() {
            anyhow::bail!(
                "Project authoring-validation certificate does not match the current Sequence set"
            );
        }
        for sequence in &current.sequences.sequences {
            self.sequence_certificates
                .get(&sequence.id)
                .context("current Sequence has no author-contract certificate")?
                .validate_baseline(sequence, &current.color_environment)
                .with_context(|| {
                    format!(
                        "sequence '{}' no longer matches its certified baseline",
                        sequence.name
                    )
                })?;
        }
        Ok(())
    }
}

impl ProjectDocument {
    /// Build a new canonical project document.
    pub fn new(
        name: impl Into<String>,
        sequences: SequenceCollection,
        color_environment: ProjectColorEnvironment,
        new_sequence_defaults: SequenceSettings,
        settings: ProjectSettings,
    ) -> Self {
        Self {
            schema_version: PROJECT_DOCUMENT_SCHEMA_VERSION,
            project_id: ProjectId::new(),
            document_revision: 1,
            meta: ProjectMeta::new(name),
            settings,
            color_environment,
            new_sequence_defaults,
            sequences,
            proxy_mode_assets: AuthoringSet::new(),
        }
    }

    /// Validate document-level invariants before opening or saving.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.prepare_authoring_validation_certificate().map(|_| ())
    }

    /// Fully validate the canonical author model and retain opaque,
    /// process-local evidence for later transactional replacement preparation.
    pub fn prepare_authoring_validation_certificate(
        &self,
    ) -> anyhow::Result<ProjectAuthoringValidationCertificate> {
        self.validate_document_contract()?;
        let mut sequence_certificates = BTreeMap::new();
        for sequence in &self.sequences.sequences {
            let certificate = sequence
                .prepare_author_contract_certificate(&self.color_environment)
                .with_context(|| {
                    format!("sequence '{}' author contract is invalid", sequence.name)
                })?;
            sequence_certificates.insert(sequence.id, Arc::new(certificate));
        }
        let dependency_certificate = SequenceDependencyCertificate::build(&self.sequences)
            .context("Project Sequence dependency closure is invalid")?;
        Ok(ProjectAuthoringValidationCertificate {
            version: PROJECT_AUTHORING_VALIDATION_CERTIFICATE_VERSION,
            context: ProjectAuthoringValidationContext::from_document(self),
            sequence_certificates,
            dependency_certificate,
        })
    }

    /// Validate one Sequence replacement against the canonical Project.
    ///
    /// This is the general stateless validation seam for callers that do not
    /// retain process-local validation evidence. It validates the replacement plus
    /// every collection-wide nesting and child-output obligation. The
    /// authoring Session hot path instead prepares and atomically installs its
    /// retained [`ProjectAuthoringValidationCertificate`].
    pub fn validate_sequence_replacement(&self, replacement: &Sequence) -> anyhow::Result<()> {
        self.prepare_authoring_validation_certificate()?
            .prepare_sequence_replacement(self, replacement.clone())
            .map(|_| ())
    }

    fn validate_document_contract(&self) -> anyhow::Result<()> {
        if self.schema_version != PROJECT_DOCUMENT_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported project document schema version: {}",
                self.schema_version
            );
        }
        if self.meta.name.trim().is_empty() {
            anyhow::bail!("project name cannot be empty");
        }
        self.new_sequence_defaults
            .validate_with_color_environment(&self.color_environment)
            .context("new Sequence defaults are invalid")?;
        Ok(())
    }

    /// Keep ID-list ordering deterministic for stable fingerprints.
    pub fn normalize_for_save(&mut self) {
        // Ordered collections and identity-bearing author entities already have
        // canonical serialization order. Keep normalization as the single save
        // hook for future non-semantic ordering rules.
    }

    /// Return a copy with deterministic ordering applied.
    pub fn normalized(mut self) -> Self {
        self.normalize_for_save();
        self
    }
}

/// Compute a stable serialized fingerprint for dirty checks.
pub fn project_document_fingerprint(document: ProjectDocument) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&document.normalized())?)
}

/// Read and validate only the canonical project document from an archive.
pub fn read_project_document_from_archive(project_file: &Path) -> anyhow::Result<ProjectDocument> {
    read_project_document_from_archive_with_budget(
        project_file,
        ProjectArchiveReadBudget::default(),
    )
}

/// Read and validate only the canonical Project document under an explicit
/// archive admission budget.
pub fn read_project_document_from_archive_with_budget(
    project_file: &Path,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<ProjectDocument> {
    let mut file = fs::File::open(project_file)?;
    read_project_document_from_open_archive_with_budget(&mut file, budget)
}

/// Read and validate the canonical Project document from one already-authorized
/// archive file object.
///
/// The caller retains namespace and sharing policy. The handle is rewound
/// before reading so verification and loading can use one exact file object
/// without reopening a potentially replaced path.
pub fn read_project_document_from_open_archive(
    file: &mut fs::File,
) -> anyhow::Result<ProjectDocument> {
    read_project_document_from_open_archive_with_budget(file, ProjectArchiveReadBudget::default())
}

/// Read and validate the canonical Project document from one retained file
/// object under an explicit admission budget.
pub fn read_project_document_from_open_archive_with_budget(
    file: &mut fs::File,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<ProjectDocument> {
    Ok(PreparedProjectArchive::from_open_file(file, budget)?.into_document())
}

/// Fully loaded archive payload with independently versioned SQLite evidence.
#[derive(Debug)]
pub struct LoadedProjectArchive {
    /// Migrated and validated canonical project document.
    pub document: ProjectDocument,
    /// SQLite schema version declared by the archive manifest.
    pub library_schema_version: u32,
}

/// One validated, single-consumption `.mdp` open ticket.
///
/// Preparation checks the compressed-file budget, exact archive-v1 entry set,
/// every declared uncompressed entry length, Manifest, and canonical Project
/// contract. The Project is deserialized exactly once. A caller may inspect its
/// stable Project identity to acquire runtime authority, then consume this
/// ticket to extract the Library into that authority's fresh generation.
pub struct PreparedProjectArchive<'archive> {
    archive: zip::ZipArchive<&'archive mut fs::File>,
    manifest: ProjectManifest,
    document: ProjectDocument,
    budget: ProjectArchiveReadBudget,
}

impl<'archive> PreparedProjectArchive<'archive> {
    /// Prepare one retained archive file object under an explicit read budget.
    pub fn from_open_file(
        file: &'archive mut fs::File,
        budget: ProjectArchiveReadBudget,
    ) -> anyhow::Result<Self> {
        budget.admit_archive_file(file)?;
        file.seek(SeekFrom::Start(0))?;
        let mut archive = zip::ZipArchive::new(&mut *file)?;
        validate_exact_archive_entry_set(&mut archive)?;
        validate_declared_archive_entry_budgets(&mut archive, budget)?;
        let (manifest, document) = read_project_archive_metadata_from_zip(&mut archive, budget)?;
        Ok(Self { archive, manifest, document, budget })
    }

    /// Stable Project identity available before runtime authority is acquired.
    pub fn project_id(&self) -> ProjectId {
        self.document.project_id
    }

    /// Consume the ticket without extracting its Library.
    pub fn into_document(self) -> ProjectDocument {
        self.document
    }

    /// Consume the ticket and extract its Library into a runtime generation.
    ///
    /// Extraction stages into an exclusively created sibling. Failure,
    /// including a decompression CRC error or actual-length violation, removes
    /// only the staging object owned by this call and never changes an existing
    /// `index.db`.
    pub fn load_into(
        mut self,
        runtime_library_root: &Path,
    ) -> anyhow::Result<LoadedProjectArchive> {
        ensure_direct_runtime_library_root(runtime_library_root)?;
        let db_path = runtime_library_root.join("index.db");
        reject_existing_non_file_or_link(&db_path, "runtime Project Library")?;
        let mut staging = OwnedPublicationFile::create_sibling(&db_path, "extract")?;

        {
            let db_entry = self
                .archive
                .by_name(LIBRARY_ENTRY)
                .with_context(|| format!("missing project archive entry: {LIBRARY_ENTRY}"))?;
            let declared_len = db_entry.size();
            self.budget.admit_declared_entry(LIBRARY_ENTRY, declared_len)?;
            let mut reader = BudgetedArchiveEntryReader::new(
                db_entry,
                LIBRARY_ENTRY,
                declared_len,
                self.budget.max_library_bytes,
            );
            std::io::copy(&mut reader, staging.file_mut()?)
                .context("failed to extract the complete Project Library archive entry")?;
            reader.verify_complete()?;
        }
        staging.file_mut()?.flush()?;
        staging.file_mut()?.sync_all()?;
        staging.publish(FilePublicationMode::ReplaceExisting)?;

        Ok(LoadedProjectArchive {
            document: self.document,
            library_schema_version: self.manifest.library_schema_version,
        })
    }
}

/// Open an `.mdp` archive, validate the document, and extract the library DB.
pub fn load_project_archive(
    archive_file: &Path,
    runtime_library_root: &Path,
) -> anyhow::Result<LoadedProjectArchive> {
    load_project_archive_with_budget(
        archive_file,
        runtime_library_root,
        ProjectArchiveReadBudget::default(),
    )
}

/// Open and extract an `.mdp` archive under an explicit admission budget.
pub fn load_project_archive_with_budget(
    archive_file: &Path,
    runtime_library_root: &Path,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<LoadedProjectArchive> {
    let mut file = fs::File::open(archive_file)?;
    load_project_archive_from_open_file_with_budget(&mut file, runtime_library_root, budget)
}

/// Load an `.mdp` archive from one already-authorized file object.
///
/// This is the object-evidence counterpart of [`load_project_archive`]. It
/// rewinds the handle and never resolves the archive path again.
pub fn load_project_archive_from_open_file(
    file: &mut fs::File,
    runtime_library_root: &Path,
) -> anyhow::Result<LoadedProjectArchive> {
    load_project_archive_from_open_file_with_budget(
        file,
        runtime_library_root,
        ProjectArchiveReadBudget::default(),
    )
}

/// Load from one retained archive object under an explicit admission budget.
pub fn load_project_archive_from_open_file_with_budget(
    file: &mut fs::File,
    runtime_library_root: &Path,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<LoadedProjectArchive> {
    PreparedProjectArchive::from_open_file(file, budget)?.load_into(runtime_library_root)
}

/// Save an `.mdp` archive atomically next to the target file.
pub fn save_project_archive(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &Path,
) -> Result<(), ProjectArchivePublicationFailure> {
    save_project_archive_with_publication(
        document,
        library_db_path,
        target_file,
        ProjectArchivePublication::ReplaceExisting,
    )
    .map(|_| ())
}

/// Save an `.mdp` archive with explicit final namespace semantics.
///
/// `CreateNew` carries the user's non-overwrite intent through the complete
/// archive build and performs the existence decision in the final atomic
/// namespace operation. A preceding `Path::exists` check is never treated as
/// publication authority.
pub fn save_project_archive_with_publication(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &Path,
    publication: ProjectArchivePublication,
) -> Result<ProjectArchivePublicationEvidence, ProjectArchivePublicationFailure> {
    let mut library_db = fs::File::open(library_db_path).with_context(|| {
        format!(
            "open exact Asset Library snapshot object failed: {}",
            library_db_path.display()
        )
    })?;
    save_project_archive_from_open_library_with_publication(
        document,
        &mut library_db,
        target_file,
        publication,
    )
}

/// Save an `.mdp` archive from one already-authorized Asset Library snapshot
/// object with explicit final namespace semantics.
///
/// The retained handle is rewound and streamed directly. This is the
/// production persistence boundary for identity-bound SQLite snapshots:
/// reopening a temporary pathname after validation is forbidden.
pub fn save_project_archive_from_open_library_with_publication(
    document: &ProjectDocument,
    library_db: &mut fs::File,
    target_file: &Path,
    publication: ProjectArchivePublication,
) -> Result<ProjectArchivePublicationEvidence, ProjectArchivePublicationFailure> {
    let document = document.clone().normalized();
    document.validate()?;
    let target_file = std::path::absolute(target_file)
        .context("failed to make Project publication target absolute")?;
    if !library_db
        .metadata()
        .context("inspect retained Asset Library snapshot object failed")?
        .is_file()
    {
        return Err(anyhow::anyhow!(
            "Asset Library snapshot handle does not name a regular file object"
        )
        .into());
    }
    save_project_archive_from_open_library_with_publication_impl(
        &document,
        library_db,
        &target_file,
        publication,
        |_| Ok(()),
    )
}

#[cfg(test)]
fn save_project_archive_with_publication_impl(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &Path,
    publication: ProjectArchivePublication,
    before_publication: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> Result<ProjectArchivePublicationEvidence, ProjectArchivePublicationFailure> {
    let mut library_db = fs::File::open(library_db_path).with_context(|| {
        format!(
            "open Asset Library test fixture failed: {}",
            library_db_path.display()
        )
    })?;
    save_project_archive_from_open_library_with_publication_impl(
        document,
        &mut library_db,
        target_file,
        publication,
        before_publication,
    )
}

fn save_project_archive_from_open_library_with_publication_impl(
    document: &ProjectDocument,
    library_db: &mut fs::File,
    target_file: &Path,
    publication: ProjectArchivePublication,
    before_publication: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> Result<ProjectArchivePublicationEvidence, ProjectArchivePublicationFailure> {
    let mut temp = OwnedPublicationFile::create_sibling(target_file, "archive")?;
    let evidence = write_project_archive_from_open_library_to_open_file(
        document,
        library_db,
        temp.file_mut()?,
    )?;
    verify_written_project_archive_from_open_file(temp.file_mut()?, &evidence)
        .context("new project archive failed retained-object validation")?;
    let (archive_sha256, archive_len) = hash_complete_open_file(temp.file_mut()?)?;
    if archive_len == 0 {
        return Err(
            anyhow::anyhow!("validated Project archive unexpectedly has zero length").into(),
        );
    }
    before_publication(temp.path())?;
    let namespace = ProjectArchiveNamespacePublicationEvidence {
        published_path: target_file.to_path_buf(),
        publication,
        project_id: document.project_id,
        document_revision: document.document_revision,
        archive_len,
        archive_sha256,
    };
    let mode = match publication {
        ProjectArchivePublication::ReplaceExisting => FilePublicationMode::ReplaceExisting,
        ProjectArchivePublication::CreateNew => FilePublicationMode::CreateNew,
    };
    match temp.publish(mode) {
        Ok(_) => Ok(ProjectArchivePublicationEvidence { namespace }),
        Err(StoragePublicationFailure::BeforeNamespace(source)) => {
            Err(ProjectArchivePublicationFailure::BeforeNamespace(source))
        }
        Err(StoragePublicationFailure::DurabilityUnconfirmed(source)) => {
            Err(ProjectArchivePublicationFailure::DurabilityUnconfirmed(
                Box::new(ProjectArchivePublicationDurabilityUnconfirmed { namespace, source }),
            ))
        }
        Err(StoragePublicationFailure::NamespaceIndeterminate(source)) => {
            Err(ProjectArchivePublicationFailure::NamespaceIndeterminate(
                Box::new(ProjectArchivePublicationNamespaceIndeterminate {
                    intended_path: namespace.published_path,
                    publication: namespace.publication,
                    project_id: namespace.project_id,
                    document_revision: namespace.document_revision,
                    archive_len: namespace.archive_len,
                    archive_sha256: namespace.archive_sha256,
                    source,
                }),
            ))
        }
    }
}

fn read_project_archive_metadata_from_zip<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<(ProjectManifest, ProjectDocument)> {
    let manifest = read_versioned_json_entry(
        archive,
        MANIFEST_ENTRY,
        PROJECT_FORMAT_VERSION,
        |manifest: &ProjectManifest| manifest.format_version,
        &ARCHIVE_MIGRATIONS,
        budget,
    )?;
    manifest.validate()?;

    let document = read_versioned_json_entry(
        archive,
        PROJECT_ENTRY,
        PROJECT_DOCUMENT_SCHEMA_VERSION,
        |document: &ProjectDocument| document.schema_version,
        &DOCUMENT_MIGRATIONS,
        budget,
    )?;
    document.validate()?;
    Ok((manifest, document.normalized()))
}

fn read_versioned_json_entry<R, T>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &'static str,
    current_version: u32,
    version: impl Fn(&T) -> u32,
    migrations: &JsonMigrationRegistry,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<T>
where
    R: Read + Seek,
    T: DeserializeOwned,
{
    let current = read_json_entry::<_, T>(archive, entry_name, budget);

    match current {
        Ok(value) if version(&value) == current_version => Ok(value),
        Ok(_) | Err(_) => {
            // The current schema remains a direct typed streaming read. Only an
            // older, future, or malformed payload pays for the in-memory Value
            // needed by the explicit migration Registry Seam.
            let value = read_json_entry(archive, entry_name, budget)
                .with_context(|| format!("invalid JSON in project archive entry: {entry_name}"))?;
            Ok(serde_json::from_value(migrations.migrate(value)?)?)
        }
    }
}

fn read_json_entry<R, T>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &'static str,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<T>
where
    R: Read + Seek,
    T: DeserializeOwned,
{
    let entry = archive
        .by_name(entry_name)
        .with_context(|| format!("missing project archive entry: {entry_name}"))?;
    let declared_len = entry.size();
    let limit = budget.entry_limit(entry_name)?;
    budget.admit_declared_entry(entry_name, declared_len)?;
    let mut reader = BudgetedArchiveEntryReader::new(entry, entry_name, declared_len, limit);
    let value = serde_json::from_reader(&mut reader)?;
    let mut sink = std::io::sink();
    std::io::copy(&mut reader, &mut sink)?;
    reader.verify_complete()?;
    Ok(value)
}

fn validate_exact_archive_entry_set<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> anyhow::Result<()> {
    let mut seen = [false; REQUIRED_ARCHIVE_ENTRIES.len()];
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() {
            anyhow::bail!(
                "directories are not permitted in project archive v1: {}",
                entry.name()
            );
        }
        let Some(required_index) =
            REQUIRED_ARCHIVE_ENTRIES.iter().position(|required| *required == entry.name())
        else {
            anyhow::bail!("unexpected project archive entry: {}", entry.name());
        };
        if seen[required_index] {
            anyhow::bail!("duplicate project archive entry: {}", entry.name());
        }
        seen[required_index] = true;
    }

    for (required, was_seen) in REQUIRED_ARCHIVE_ENTRIES.iter().zip(seen) {
        if !was_seen {
            anyhow::bail!("missing project archive entry: {required}");
        }
    }
    Ok(())
}

fn validate_declared_archive_entry_budgets<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    budget: ProjectArchiveReadBudget,
) -> anyhow::Result<()> {
    for entry_name in REQUIRED_ARCHIVE_ENTRIES {
        let entry = archive
            .by_name(entry_name)
            .with_context(|| format!("missing project archive entry: {entry_name}"))?;
        budget.admit_declared_entry(entry_name, entry.size())?;
    }
    Ok(())
}

#[cfg(test)]
fn write_project_archive(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &Path,
) -> anyhow::Result<ProjectArchiveWriteEvidence> {
    let mut file = fs::File::create(target_file)?;
    write_project_archive_to_open_file(document, library_db_path, &mut file)
}

#[cfg(test)]
fn write_project_archive_to_open_file(
    document: &ProjectDocument,
    library_db_path: &Path,
    target_file: &mut fs::File,
) -> anyhow::Result<ProjectArchiveWriteEvidence> {
    let mut library_db = fs::File::open(library_db_path)?;
    write_project_archive_from_open_library_to_open_file(document, &mut library_db, target_file)
}

fn write_project_archive_from_open_library_to_open_file(
    document: &ProjectDocument,
    library_db: &mut fs::File,
    target_file: &mut fs::File,
) -> anyhow::Result<ProjectArchiveWriteEvidence> {
    target_file.set_len(0)?;
    target_file.seek(SeekFrom::Start(0))?;
    let mut writer = zip::ZipWriter::new(&mut *target_file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    writer.start_file(MANIFEST_ENTRY, options)?;
    let mut manifest_writer = EvidenceWriter::new(&mut writer);
    {
        let mut buffered = BufWriter::new(&mut manifest_writer);
        serde_json::to_writer_pretty(&mut buffered, &ProjectManifest::default())?;
        buffered.flush()?;
    }
    let manifest = manifest_writer.finish(MANIFEST_ENTRY);

    // `large_file(true)` writes ZIP64 size fields from the start, so a large
    // Project or Library never crosses the 32-bit ZIP boundary after streaming
    // has already begun. Open-time admission remains a separate caller policy.
    writer.start_file(PROJECT_ENTRY, options.large_file(true))?;
    let mut project_writer = EvidenceWriter::new(&mut writer);
    {
        let mut buffered = BufWriter::new(&mut project_writer);
        serde_json::to_writer_pretty(&mut buffered, document)?;
        buffered.flush()?;
    }
    let project = project_writer.finish(PROJECT_ENTRY);

    writer.start_file(LIBRARY_ENTRY, options.large_file(true))?;
    library_db.seek(SeekFrom::Start(0))?;
    let mut db_reader = BufReader::new(library_db);
    let mut library_writer = EvidenceWriter::new(&mut writer);
    std::io::copy(&mut db_reader, &mut library_writer)?;
    let library = library_writer.finish(LIBRARY_ENTRY);

    writer.finish()?;
    drop(writer);
    target_file.sync_all()?;
    Ok(ProjectArchiveWriteEvidence { manifest, project, library })
}

#[cfg(test)]
fn verify_written_project_archive(
    archive_path: &Path,
    expected: &ProjectArchiveWriteEvidence,
) -> anyhow::Result<()> {
    let mut file = fs::File::open(archive_path)?;
    verify_written_project_archive_from_open_file(&mut file, expected)
}

fn verify_written_project_archive_from_open_file(
    archive_file: &mut fs::File,
    expected: &ProjectArchiveWriteEvidence,
) -> anyhow::Result<()> {
    archive_file.seek(SeekFrom::Start(0))?;
    let verifier = archive_file.try_clone()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(verifier))?;
    validate_exact_archive_entry_set(&mut archive)?;

    for expected_entry in expected.entries() {
        let mut entry = archive.by_name(expected_entry.name).with_context(|| {
            format!(
                "missing project archive entry during reopen verification: {}",
                expected_entry.name
            )
        })?;
        if entry.size() != expected_entry.uncompressed_len {
            anyhow::bail!(
                "project archive entry length mismatch for {}: expected {}, ZIP declares {}",
                expected_entry.name,
                expected_entry.uncompressed_len,
                entry.size()
            );
        }

        let mut sink = std::io::sink();
        let mut observed_writer = EvidenceWriter::new(&mut sink);
        std::io::copy(&mut entry, &mut observed_writer).with_context(|| {
            format!(
                "failed to read complete project archive entry during reopen verification: {}",
                expected_entry.name
            )
        })?;
        let observed = observed_writer.finish(expected_entry.name);
        if observed.uncompressed_len != expected_entry.uncompressed_len {
            anyhow::bail!(
                "project archive entry length mismatch for {}: expected {}, read {}",
                expected_entry.name,
                expected_entry.uncompressed_len,
                observed.uncompressed_len
            );
        }
        if observed.sha256 != expected_entry.sha256 {
            anyhow::bail!(
                "project archive entry SHA-256 mismatch during reopen verification: {}",
                expected_entry.name
            );
        }
    }
    Ok(())
}

fn hash_complete_open_file(archive_file: &mut fs::File) -> anyhow::Result<([u8; 32], u64)> {
    archive_file.seek(SeekFrom::Start(0))?;
    let expected_len = archive_file.metadata()?.len();
    let mut reader = BufReader::new(archive_file.try_clone()?);
    let mut hasher = Sha256::new();
    let mut observed_len = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed_len =
            observed_len
                .checked_add(u64::try_from(read).map_err(|_| {
                    anyhow::anyhow!("Project archive read length does not fit in u64")
                })?)
                .ok_or_else(|| anyhow::anyhow!("Project archive read length overflowed u64"))?;
        hasher.update(&buffer[..read]);
    }
    anyhow::ensure!(
        observed_len == expected_len,
        "Project archive whole-file hash observed {observed_len} bytes but retained object reports {expected_len}"
    );
    archive_file.seek(SeekFrom::Start(0))?;
    Ok((hasher.finalize().into(), observed_len))
}

fn reject_existing_non_file_or_link(path: &Path, description: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("{description} is not a direct regular file")
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn ensure_direct_runtime_library_root(root: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("runtime Project Library root is not a direct directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root)?;
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(root)?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && metadata.is_dir(),
        "runtime Project Library root is not a direct directory"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{
        InterpolationType, Keyframe, PropertyDescriptor, PropertyMutation, PropertyValue,
    };
    use mondrian_core::effect_data::{EffectNode, EffectType};
    use mondrian_core::mask_data::{MaskComponent, MaskEvaluation};
    use mondrian_core::{
        ExactAutomationCurve, ExactAutomationKeyframe, ParameterId, ParameterUnit, PropertyHost,
        TimelineTime,
    };
    use mondrian_timeline::audio::{
        AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID,
        BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, BUILTIN_SAMPLE_DELAY_DEFINITION_ID,
        GAIN_DB_PARAMETER_ID, LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID,
        SAMPLE_DELAY_FRAMES_PARAMETER_ID,
    };
    use mondrian_timeline::{Clip, Sequence};

    fn missing_custom_engine(path: PathBuf) -> mondrian_core::ColorEngine {
        mondrian_core::ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    mondrian_core::OcioConfigSource::Path { path },
                    "0".repeat(64),
                    "0".repeat(64),
                    "Linear Rec.2020".to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        mondrian_core::ColorSpace::Rec709,
                        "Test Display".to_owned(),
                        "Test View".to_owned(),
                        "Test Display Color Space".to_owned(),
                        mondrian_core::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Custom OCIO output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid missing Custom OCIO identity"),
            ),
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mondrian-project-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create temp dir");
        root
    }

    fn test_document() -> ProjectDocument {
        let sequence = Sequence::new("Main");
        let collection = SequenceCollection::new(sequence);
        ProjectDocument::new(
            "Main",
            collection,
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        )
    }

    fn evidence_for_bytes(name: &'static str, bytes: &[u8]) -> ArchiveEntryEvidence {
        ArchiveEntryEvidence {
            name,
            uncompressed_len: u64::try_from(bytes.len()).expect("fixture length fits u64"),
            sha256: Sha256::digest(bytes).into(),
        }
    }

    fn write_stored_test_archive(
        path: &Path,
        document: &ProjectDocument,
        library: &[u8],
        extra: Option<(&str, &[u8])>,
    ) -> ProjectArchiveWriteEvidence {
        let manifest =
            serde_json::to_vec_pretty(&ProjectManifest::default()).expect("serialize manifest");
        let project = serde_json::to_vec_pretty(document).expect("serialize project");
        let file = fs::File::create(path).expect("create stored fixture archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o644);
        for (name, bytes) in [
            (MANIFEST_ENTRY, manifest.as_slice()),
            (PROJECT_ENTRY, project.as_slice()),
            (LIBRARY_ENTRY, library),
        ] {
            writer.start_file(name, options).expect("start fixture entry");
            writer.write_all(bytes).expect("write fixture entry");
        }
        if let Some((name, bytes)) = extra {
            writer.start_file(name, options).expect("start extra fixture entry");
            writer.write_all(bytes).expect("write extra fixture entry");
        }
        writer.finish().expect("finish stored fixture archive");

        ProjectArchiveWriteEvidence {
            manifest: evidence_for_bytes(MANIFEST_ENTRY, &manifest),
            project: evidence_for_bytes(PROJECT_ENTRY, &project),
            library: evidence_for_bytes(LIBRARY_ENTRY, library),
        }
    }

    fn write_raw_stored_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(path).expect("create raw fixture archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o644);
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("start raw fixture entry");
            writer.write_all(bytes).expect("write raw fixture entry");
        }
        writer.finish().expect("finish raw fixture archive");
    }

    #[test]
    fn manifest_default_describes_current_archive_layout() {
        let manifest = ProjectManifest::default();

        assert_eq!(manifest.format, "mondrian-project");
        assert_eq!(manifest.format_version, PROJECT_FORMAT_VERSION);
        assert_eq!(manifest.document_layout, "single-project-json");
        assert_eq!(manifest.project_entry, PROJECT_ENTRY);
        assert_eq!(manifest.library_entry, LIBRARY_ENTRY);
        assert_eq!(
            manifest.library_schema_version,
            PROJECT_LIBRARY_SCHEMA_VERSION
        );
        manifest.validate().expect("default manifest should validate");
    }

    #[test]
    fn project_archive_round_trips_document_and_library() {
        let root = unique_temp_dir("round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("project.mdp");

        let document = test_document();
        save_project_archive(&document, &db_path, &project_path).expect("save archive");

        let opened = read_project_document_from_archive(&project_path).expect("read document");
        assert_eq!(opened.project_id, document.project_id);
        assert_eq!(opened.meta.name, "Main");
        assert_eq!(opened.sequences.sequences.len(), 1);
        let opened_sequence = opened.sequences.active().expect("active sequence after reopen");
        assert_eq!(
            opened_sequence.revision,
            document.sequences.active().expect("source sequence").revision
        );
        assert_eq!(
            opened_sequence.settings.color.program_output.workflow,
            mondrian_timeline::sequence::ColorWorkflow::SceneReferred
        );
        let opened_context =
            opened_sequence.settings.root_program_color_context(&opened.color_environment);
        assert_eq!(
            opened_context.engine,
            mondrian_core::ColorEngine::mondrian_standard()
        );
        assert_eq!(
            opened_context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );

        let runtime_library = root.join("runtime-library");
        let loaded =
            load_project_archive(&project_path, &runtime_library).expect("load project archive");
        assert_eq!(loaded.document.project_id, document.project_id);
        assert_eq!(
            loaded.library_schema_version,
            PROJECT_LIBRARY_SCHEMA_VERSION
        );
        let loaded_sequence =
            loaded.document.sequences.active().expect("active sequence after archive load");
        assert_eq!(
            loaded_sequence.settings.color.program_output.workflow,
            mondrian_timeline::sequence::ColorWorkflow::SceneReferred
        );
        assert_eq!(
            fs::read(runtime_library.join("index.db")).expect("read extracted db"),
            b"sqlite placeholder"
        );
    }

    #[test]
    fn retained_library_handle_prevents_snapshot_path_substitution() {
        let root = unique_temp_dir("retained-library-object");
        let db_path = root.join("snapshot.db");
        let original = b"exact SQLite snapshot object";
        let replacement = b"foreign replacement path";
        fs::write(&db_path, original).expect("write original snapshot");
        let mut retained = fs::File::open(&db_path).expect("retain original snapshot object");
        fs::remove_file(&db_path).expect("detach original snapshot name");
        fs::write(&db_path, replacement).expect("install replacement path");
        let archive_path = root.join("project.mdp");

        save_project_archive_from_open_library_with_publication(
            &test_document(),
            &mut retained,
            &archive_path,
            ProjectArchivePublication::CreateNew,
        )
        .expect("save from retained snapshot object");

        let archive_file = fs::File::open(&archive_path).expect("open archive");
        let mut archive = zip::ZipArchive::new(archive_file).expect("read archive");
        let mut library = archive.by_name(LIBRARY_ENTRY).expect("library entry");
        let mut stored = Vec::new();
        library.read_to_end(&mut stored).expect("read library entry");
        assert_eq!(stored, original);
        assert_eq!(
            fs::read(&db_path).expect("read replacement path"),
            replacement
        );
    }

    #[test]
    fn prepared_archive_ticket_exposes_identity_then_loads_without_reopening() {
        let root = unique_temp_dir("prepared-ticket");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"prepared library").expect("write library");
        let archive_path = root.join("prepared.mdp");
        let document = test_document();
        save_project_archive(&document, &db_path, &archive_path).expect("save archive");

        let mut archive_file = fs::File::open(&archive_path).expect("open retained archive object");
        let prepared = PreparedProjectArchive::from_open_file(
            &mut archive_file,
            ProjectArchiveReadBudget::default(),
        )
        .expect("prepare archive once");
        assert_eq!(prepared.project_id(), document.project_id);

        let runtime = root.join("runtime");
        let loaded = prepared.load_into(&runtime).expect("consume prepared archive");
        assert_eq!(loaded.document.project_id, document.project_id);
        assert_eq!(
            fs::read(runtime.join("index.db")).expect("read extracted library"),
            b"prepared library"
        );
    }

    #[test]
    fn default_archive_budget_admits_current_large_project_evidence_without_being_unbounded() {
        const KNOWN_120_MINUTE_STRESS_PROJECT_JSON_BYTES: u64 = 606_155_667;
        let budget = ProjectArchiveReadBudget::default();
        assert!(budget.max_project_bytes > KNOWN_120_MINUTE_STRESS_PROJECT_JSON_BYTES);
        assert!(budget.max_project_bytes < 1024 * 1024 * 1024);
        assert_eq!(budget.max_manifest_bytes, 64 * 1024);
        assert!(budget.max_archive_bytes < 3 * 1024 * 1024 * 1024);
        assert!(budget.max_library_bytes < 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn archive_budget_rejects_compressed_and_declared_lengths_before_json_parse() {
        let root = unique_temp_dir("archive-budget");
        let archive_path = root.join("budget.mdp");
        let document = test_document();
        let evidence = write_stored_test_archive(&archive_path, &document, b"library", None);
        let archive_len = fs::metadata(&archive_path).expect("archive metadata").len();

        let compressed_error = read_project_document_from_archive_with_budget(
            &archive_path,
            ProjectArchiveReadBudget {
                max_archive_bytes: archive_len.saturating_sub(1),
                ..ProjectArchiveReadBudget::default()
            },
        )
        .expect_err("compressed archive over budget must fail admission");
        assert!(
            format!("{compressed_error:#}").contains("compressed length exceeds"),
            "{compressed_error:#}"
        );

        let project_error = read_project_document_from_archive_with_budget(
            &archive_path,
            ProjectArchiveReadBudget {
                max_project_bytes: evidence.project.uncompressed_len.saturating_sub(1),
                ..ProjectArchiveReadBudget::default()
            },
        )
        .expect_err("declared Project JSON over budget must fail admission");
        assert!(
            format!("{project_error:#}").contains("declared length exceeds"),
            "{project_error:#}"
        );
    }

    #[test]
    fn archive_entry_reader_rejects_actual_length_over_budget_and_declared_mismatch() {
        let mut over_budget =
            BudgetedArchiveEntryReader::new(std::io::Cursor::new(b"1234"), PROJECT_ENTRY, 4, 3);
        let error = std::io::copy(&mut over_budget, &mut std::io::sink())
            .expect_err("actual bytes beyond the budget must fail");
        assert!(error.to_string().contains("actual length exceeds"));

        let mut wrong_declaration =
            BudgetedArchiveEntryReader::new(std::io::Cursor::new(b"1234"), PROJECT_ENTRY, 3, 8);
        std::io::copy(&mut wrong_declaration, &mut std::io::sink())
            .expect("copy bytes within budget");
        let error = wrong_declaration
            .verify_complete()
            .expect_err("actual bytes must equal the declared length");
        assert!(error.to_string().contains("differs from ZIP declaration"));
    }

    #[test]
    fn exact_entry_contract_precedes_manifest_parse_and_rejects_duplicates_and_directories() {
        let root = unique_temp_dir("exact-entry-contract");
        let document = serde_json::to_vec(&test_document()).expect("serialize Project");
        let duplicate = root.join("duplicate.mdp");
        write_raw_stored_archive(
            &duplicate,
            &[
                (MANIFEST_ENTRY, b"{"),
                (MANIFEST_ENTRY, b"{"),
                (PROJECT_ENTRY, &document),
                (LIBRARY_ENTRY, b"library"),
            ],
        );
        let error = read_project_document_from_archive(&duplicate)
            .expect_err("duplicate entry must fail before malformed Manifest parse");
        assert!(
            format!("{error:#}").contains("duplicate project archive entry"),
            "{error:#}"
        );

        let directory = root.join("directory.mdp");
        let file = fs::File::create(&directory).expect("create directory-entry archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.add_directory("unexpected/", options).expect("directory entry");
        writer.start_file(MANIFEST_ENTRY, options).expect("manifest");
        writer.write_all(b"{").expect("malformed manifest");
        writer.start_file(PROJECT_ENTRY, options).expect("Project");
        writer.write_all(&document).expect("Project bytes");
        writer.start_file(LIBRARY_ENTRY, options).expect("Library");
        writer.write_all(b"library").expect("Library bytes");
        writer.finish().expect("finish directory-entry archive");
        let error = read_project_document_from_archive(&directory)
            .expect_err("directory entry must fail before malformed Manifest parse");
        assert!(
            format!("{error:#}").contains("directories are not permitted"),
            "{error:#}"
        );
    }

    #[test]
    fn future_and_malformed_manifests_fail_closed_under_the_exact_layout() {
        let root = unique_temp_dir("manifest-failure");
        let project = serde_json::to_vec(&test_document()).expect("serialize Project");
        let future_manifest = ProjectManifest {
            format_version: PROJECT_FORMAT_VERSION + 1,
            ..ProjectManifest::default()
        };
        let future_manifest =
            serde_json::to_vec(&future_manifest).expect("serialize future Manifest");
        let future = root.join("future.mdp");
        write_raw_stored_archive(
            &future,
            &[
                (MANIFEST_ENTRY, &future_manifest),
                (PROJECT_ENTRY, &project),
                (LIBRARY_ENTRY, b"library"),
            ],
        );
        let error = read_project_document_from_archive(&future)
            .expect_err("future archive format must fail closed");
        assert!(
            format!("{error:#}").contains("unsupported project archive version"),
            "{error:#}"
        );

        let malformed = root.join("malformed.mdp");
        write_raw_stored_archive(
            &malformed,
            &[
                (MANIFEST_ENTRY, b"{"),
                (PROJECT_ENTRY, &project),
                (LIBRARY_ENTRY, b"library"),
            ],
        );
        let error = read_project_document_from_archive(&malformed)
            .expect_err("malformed Manifest must fail closed");
        assert!(format!("{error:#}").contains("invalid JSON"), "{error:#}");
    }

    #[test]
    fn library_crc_failure_cleans_staging_and_preserves_existing_target() {
        let root = unique_temp_dir("library-crc-staging");
        let archive_path = root.join("crc.mdp");
        let library = b"MONDRIAN_LIBRARY_STAGING_CRC_SENTINEL_74A892BC";
        let document = test_document();
        write_stored_test_archive(&archive_path, &document, library, None);
        let mut bytes = fs::read(&archive_path).expect("read stored archive");
        let offset = bytes
            .windows(library.len())
            .position(|candidate| candidate == library)
            .expect("unique library sentinel");
        bytes[offset + library.len() / 2] ^= 0x01;
        fs::write(&archive_path, bytes).expect("tamper Library entry");

        let runtime = root.join("runtime");
        fs::create_dir_all(&runtime).expect("runtime root");
        fs::write(runtime.join("index.db"), b"existing library").expect("existing target");
        let error = load_project_archive(&archive_path, &runtime)
            .expect_err("Library CRC failure must reject staging");
        let detail = format!("{error:#}");
        assert!(
            detail.contains("checksum") || detail.contains("complete Project Library"),
            "{detail}"
        );
        assert_eq!(
            fs::read(runtime.join("index.db")).expect("preserved existing target"),
            b"existing library"
        );
        assert!(
            fs::read_dir(&runtime).expect("runtime entries").all(|entry| !entry
                .expect("runtime entry")
                .file_name()
                .to_string_lossy()
                .contains("-extract-")),
            "failed extraction must RAII-clean its owned staging file"
        );
    }

    #[test]
    fn injected_prepublication_failure_cleans_only_the_owned_archive_temp() {
        let root = unique_temp_dir("prepublication-failure");
        let library = root.join("index.db");
        fs::write(&library, b"library").expect("write library");
        let target = root.join("project.mdp");
        let error = save_project_archive_with_publication_impl(
            &test_document(),
            &library,
            &target,
            ProjectArchivePublication::ReplaceExisting,
            |_| anyhow::bail!("injected prepublication failure"),
        )
        .expect_err("injected seam must abort publication");
        assert!(format!("{error:#}").contains("injected prepublication failure"));
        assert!(!target.exists());
        assert!(
            fs::read_dir(&root).expect("root entries").all(|entry| !entry
                .expect("root entry")
                .file_name()
                .to_string_lossy()
                .contains("-archive-")),
            "owned temporary archive must be cleaned"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepublication_identity_recheck_rejects_and_preserves_a_foreign_replacement() {
        let root = unique_temp_dir("prepublication-replacement");
        let library = root.join("index.db");
        fs::write(&library, b"library").expect("write library");
        let target = root.join("project.mdp");
        let mut foreign_path = None;
        let error = save_project_archive_with_publication_impl(
            &test_document(),
            &library,
            &target,
            ProjectArchivePublication::ReplaceExisting,
            |temp_path| {
                let displaced = root.join("verified-but-displaced.tmp");
                fs::rename(temp_path, &displaced).expect("displace verified object");
                fs::write(temp_path, b"foreign replacement").expect("install foreign object");
                foreign_path = Some(temp_path.to_path_buf());
                Ok(())
            },
        )
        .expect_err("namespace replacement must fail final identity admission");
        assert!(
            format!("{error:#}").contains("no longer names its owned file object"),
            "{error:#}"
        );
        let foreign_path = foreign_path.expect("hook observed temporary path");
        assert_eq!(
            fs::read(foreign_path).expect("foreign replacement preserved"),
            b"foreign replacement"
        );
        assert!(!target.exists());
    }

    #[test]
    fn streamed_archive_write_returns_exact_reopen_evidence() {
        let root = unique_temp_dir("streamed-write-evidence");
        let db_path = root.join("index.db");
        let library = b"sqlite streamed evidence";
        fs::write(&db_path, library).expect("write library");
        let archive_path = root.join("evidence.mdp");
        let document = test_document();

        let evidence =
            write_project_archive(&document, &db_path, &archive_path).expect("write archive");

        assert_eq!(
            evidence.library.uncompressed_len,
            u64::try_from(library.len()).expect("fixture length fits u64")
        );
        assert_eq!(
            evidence.library.sha256,
            <[u8; 32]>::from(Sha256::digest(library))
        );
        assert!(evidence.manifest.uncompressed_len > 0);
        assert!(evidence.project.uncompressed_len > 0);
        verify_written_project_archive(&archive_path, &evidence)
            .expect("streamed evidence verifies exact written bytes");
    }

    #[test]
    fn reopen_verifier_rejects_tamper_even_with_a_recomputed_zip_crc() {
        let root = unique_temp_dir("recomputed-crc-tamper");
        let archive_path = root.join("tampered.mdp");
        let document = test_document();
        let expected =
            write_stored_test_archive(&archive_path, &document, b"library revision A", None);

        write_stored_test_archive(&archive_path, &document, b"library revision B", None);
        let error = verify_written_project_archive(&archive_path, &expected)
            .expect_err("content changed with a valid ZIP CRC must fail SHA-256 evidence");

        assert!(
            format!("{error:#}").contains("SHA-256 mismatch"),
            "{error:#}"
        );
    }

    #[test]
    fn reopen_verifier_reads_entries_to_eof_and_rejects_zip_crc_tamper() {
        let root = unique_temp_dir("crc-tamper");
        let archive_path = root.join("tampered.mdp");
        let library = b"MONDRIAN_LIBRARY_CRC_SENTINEL_9DB16E8A4D72";
        let document = test_document();
        let expected = write_stored_test_archive(&archive_path, &document, library, None);
        let mut archive_bytes = fs::read(&archive_path).expect("read stored archive");
        let offsets = archive_bytes
            .windows(library.len())
            .enumerate()
            .filter_map(|(offset, candidate)| (candidate == library).then_some(offset))
            .collect::<Vec<_>>();
        assert_eq!(offsets.len(), 1, "library sentinel must occur exactly once");
        archive_bytes[offsets[0] + library.len() / 2] ^= 0x01;
        fs::write(&archive_path, archive_bytes).expect("tamper stored entry bytes");

        let error = verify_written_project_archive(&archive_path, &expected)
            .expect_err("full entry read must trigger ZIP CRC validation");
        assert!(
            format!("{error:#}").contains("Invalid checksum"),
            "{error:#}"
        );
    }

    #[test]
    fn reopen_verifier_rejects_any_non_contract_archive_entry() {
        let root = unique_temp_dir("extra-entry");
        let archive_path = root.join("extra-entry.mdp");
        let document = test_document();
        let evidence = write_stored_test_archive(
            &archive_path,
            &document,
            b"library",
            Some(("unexpected.bin", b"not part of archive v1")),
        );

        let error = verify_written_project_archive(&archive_path, &evidence)
            .expect_err("archive v1 requires the exact three-entry set");
        assert!(
            format!("{error:#}").contains("unexpected project archive entry"),
            "{error:#}"
        );
    }

    #[test]
    fn create_new_archive_publication_never_replaces_an_existing_entry() {
        let root = unique_temp_dir("create-new");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("project.mdp");
        let document = test_document();

        let evidence = save_project_archive_with_publication(
            &document,
            &db_path,
            &project_path,
            ProjectArchivePublication::CreateNew,
        )
        .expect("first create-only publication");
        assert_eq!(evidence.published_path(), project_path);
        assert_eq!(evidence.publication(), ProjectArchivePublication::CreateNew);
        assert_eq!(evidence.project_id(), document.project_id);
        assert_eq!(evidence.document_revision(), document.document_revision);
        assert_eq!(
            evidence.archive_len(),
            fs::metadata(&project_path).expect("published archive metadata").len()
        );
        let mut published_file = fs::File::open(&project_path).expect("open publication");
        let (published_sha256, published_len) =
            hash_complete_open_file(&mut published_file).expect("hash publication");
        assert_eq!(evidence.archive_sha256(), published_sha256);
        assert_eq!(evidence.archive_len(), published_len);
        assert_eq!(evidence.archive_sha256_hex().len(), 64);
        let published = fs::read(&project_path).expect("read first publication");

        let mut replacement = test_document();
        replacement.meta.name = "Must Not Replace".to_owned();
        let error = save_project_archive_with_publication(
            &replacement,
            &db_path,
            &project_path,
            ProjectArchivePublication::CreateNew,
        )
        .expect_err("second create-only publication must fail atomically");

        assert!(
            format!("{error:#}").contains("atomically publish"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&project_path).expect("read preserved publication"),
            published
        );
        assert!(
            fs::read_dir(&root).expect("read publication directory").all(|entry| !entry
                .expect("publication entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "a rejected create-only publication must remove its completed temporary archive"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepared_sequence_replacement_pairs_the_installation_and_certificate_root() {
        let document = test_document();
        let certificate = document
            .prepare_authoring_validation_certificate()
            .expect("valid Project certificate");
        let mut replacement = document.sequences.sequences[0].clone();
        replacement.revision = replacement.revision.checked_next().expect("next revision");
        replacement.name = "Validated replacement".to_owned();

        let ticket = certificate
            .prepare_sequence_replacement(&document, replacement)
            .expect("prepared replacement");

        assert!(ticket
            .certificate
            .dependency_certificate
            .shares_baseline_root_with(&ticket.document.sequences.sequences));
        assert_eq!(ticket.replacement().name, "Validated replacement");
        let (next_document, next_certificate) = ticket.into_installation();
        next_certificate
            .validate_current_baseline(&next_document)
            .expect("installed pair remains exact");
    }

    #[test]
    fn project_only_replacement_reuses_exact_anchored_sequence_evidence() {
        let document = test_document();
        let certificate = document
            .prepare_authoring_validation_certificate()
            .expect("valid Project certificate");
        let sequence_id = document.sequences.default_sequence_id;
        let retained_sequence_certificate = certificate
            .sequence_certificates
            .get(&sequence_id)
            .expect("default Sequence certificate");
        let mut replacement = document.clone();
        replacement.new_sequence_defaults.resolution.width =
            replacement.new_sequence_defaults.resolution.width.saturating_add(2);

        let next = certificate
            .prepare_project_replacement(&document, &replacement)
            .expect("Project-only replacement");

        assert!(Arc::ptr_eq(
            retained_sequence_certificate,
            next.sequence_certificates
                .get(&sequence_id)
                .expect("reused Sequence certificate")
        ));
        assert_eq!(
            next.dependency_certificate,
            certificate.dependency_certificate
        );
        next.validate_current_baseline(&replacement)
            .expect("reused evidence certifies the replacement Project");
    }

    #[test]
    fn project_color_change_requires_a_full_certificate_rebuild() {
        let document = test_document();
        let certificate = document
            .prepare_authoring_validation_certificate()
            .expect("valid Project certificate");
        let mut color_changed = document.clone();
        color_changed.color_environment =
            ProjectColorEnvironment::new(mondrian_core::ColorEngine::MondrianStandard {
                package: mondrian_core::MondrianStandardPackageIdentity::V2,
            });
        let rebuilt = certificate
            .prepare_project_replacement(&document, &color_changed)
            .expect("valid Project-wide color replacement");
        let mut replacement = color_changed.sequences.sequences[0].clone();
        replacement.revision = replacement.revision.checked_next().expect("next revision");
        replacement.name = "After color change".to_owned();

        assert!(certificate
            .prepare_sequence_replacement(&color_changed, replacement.clone())
            .expect_err("old certificate must not cross color environments")
            .to_string()
            .contains("validation context"));
        rebuilt
            .prepare_sequence_replacement(&color_changed, replacement)
            .expect("rebuilt certificate validates the new color context");
    }

    #[test]
    fn document_validation_rejects_zero_sequence_revision() {
        let mut value = serde_json::to_value(test_document()).expect("serialize document");
        value["sequences"]["sequences"][0]["revision"] = serde_json::json!(0);
        let document = serde_json::from_value::<ProjectDocument>(value)
            .expect("zero revision remains structurally deserializable");

        let error = document.validate().expect_err("zero revision must fail");
        let detail = format!("{error:#}");
        assert!(detail.contains("author revision zero"), "{detail}");
    }

    #[test]
    fn parameter_schema_and_instance_address_round_trip_without_identity_drift() {
        let root = unique_temp_dir("parameter-schema-round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("parameter-schema.mdp");

        let mut document = test_document();
        let parameter_id = ParameterId::new_static("mondrian.effect.builtin.gaussian_blur.radius");
        let mut effect = EffectNode::new(EffectType::GaussianBlur);
        effect.define_property(
            PropertyDescriptor::new(
                "effect.gaussian_blur.radius",
                "Radius",
                PropertyValue::Float(12.0),
            )
            .with_parameter_id(parameter_id.clone()),
        );
        let mut clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(5, 1).expect("duration"),
        )
        .expect("clip");
        let effect_id = clip.add_effect_node(effect);
        document.sequences.active_mut().expect("active sequence").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");

        save_project_archive(&document, &db_path, &project_path).expect("save project");
        let reopened = read_project_document_from_archive(&project_path).expect("reopen project");
        let reopened_effect =
            &reopened.sequences.active().expect("active sequence").video_tracks[0].clips[0].effects
                [0];
        let (address, property) = reopened_effect.properties.iter().next().expect("property");

        assert_eq!(reopened_effect.id, effect_id);
        assert_eq!(property.descriptor.parameter_id(), &parameter_id);
        assert!(address.contains(&effect_id.to_string()));
        assert_eq!(property.descriptor.schema.schema_version, 1);
        assert_eq!(
            property.descriptor.schema.message_id,
            "mondrian.effect.builtin.gaussian_blur.radius.label"
        );
    }

    #[test]
    fn basic_title_closed_author_state_round_trips_with_exact_animation() {
        let root = unique_temp_dir("basic-title-round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("basic-title.mdp");

        let mut document = test_document();
        let mut clip = Clip::new_basic_title(
            "Mondrian 标题",
            mondrian_core::default_basic_title_font_family(),
            TimelineTime::ZERO,
            TimelineTime::new(5, 1).expect("duration"),
        )
        .expect("Basic Title");
        clip.clip_time_in = TimelineTime::new(10, 1).expect("Clip visual author origin");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: mondrian_core::BasicTitle::FONT_SIZE_PATH.to_owned(),
            keyframe: Keyframe::linear(
                TimelineTime::new(10, 1).expect("first key time"),
                PropertyValue::Float(72.0),
            ),
        })
        .expect("first font-size key");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: mondrian_core::BasicTitle::FONT_SIZE_PATH.to_owned(),
            keyframe: Keyframe::linear(
                TimelineTime::new(14, 1).expect("second key time"),
                PropertyValue::Float(144.0),
            ),
        })
        .expect("second font-size key");
        let clip_id = clip.id;
        document.sequences.active_mut().expect("active sequence").video_tracks[0]
            .add_clip(clip)
            .expect("add Basic Title");

        save_project_archive(&document, &db_path, &project_path).expect("save project");
        let reopened = read_project_document_from_archive(&project_path).expect("reopen project");
        let reopened_clip = reopened.sequences.active().expect("active sequence").video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("reopened Clip");
        assert_eq!(
            reopened_clip.clip_time_in,
            TimelineTime::new(10, 1).expect("expected Clip visual author origin")
        );
        let reopened_title = reopened_clip.content.basic_title().expect("reopened Basic Title");

        reopened_title.validate_author_state().expect("valid title state");
        let midpoint = reopened_title
            .evaluate(TimelineTime::new(12, 1).expect("midpoint"))
            .expect("evaluate title");
        assert_eq!(midpoint.text, "Mondrian 标题");
        assert!((midpoint.font_size - 108.0).abs() < 1.0e-5);
    }

    #[test]
    fn mask_scalar_animation_round_trips_without_falling_back_to_defaults() {
        let root = unique_temp_dir("mask-animation-round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("mask-animation.mdp");

        let mut document = test_document();
        let mut clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(5, 1).expect("duration"),
        )
        .expect("clip");
        let mut mask = MaskComponent::new("Subject".to_owned(), MaskEvaluation::default());
        let key_time = TimelineTime::new(2, 1).expect("key time");
        mask.properties
            .write_value(
                mondrian_core::mask_data::MASK_PROP_OPACITY,
                key_time,
                PropertyValue::Float(0.25),
                InterpolationType::Linear,
            )
            .expect("mask opacity key");
        mask.set_shape_animation_enabled(true, TimelineTime::ZERO)
            .expect("enable shape animation");
        mask.write_shape(
            TimelineTime::ZERO,
            mondrian_core::mask_data::MaskShape::default(),
            mondrian_core::mask_data::MaskShapeInterpolation::Linear,
        )
        .expect("set outgoing linear interpolation");
        mask.write_shape(
            key_time,
            mondrian_core::mask_data::MaskShape::Rectangle {
                x: 0.2,
                y: 0.15,
                width: 0.6,
                height: 0.7,
                corner_radius: 0.1,
            },
            mondrian_core::mask_data::MaskShapeInterpolation::Hold,
        )
        .expect("insert stable shape key");
        let mask_id = mask.id;
        let shape_key_ids = mask.shape_keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
        clip.masks.push(mask);
        document.sequences.active_mut().expect("active sequence").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");

        save_project_archive(&document, &db_path, &project_path).expect("save project");
        let reopened = read_project_document_from_archive(&project_path).expect("reopen project");
        let reopened_mask = &reopened.sequences.active().expect("active sequence").video_tracks[0]
            .clips[0]
            .masks[0];

        assert_eq!(reopened_mask.id, mask_id);
        assert_eq!(
            reopened_mask.shape_keyframes.iter().map(|key| key.id).collect::<Vec<_>>(),
            shape_key_ids
        );
        assert_eq!(
            reopened_mask.shape_keyframes[0].interpolation,
            mondrian_core::mask_data::MaskShapeInterpolation::Linear
        );
        assert_eq!(reopened_mask.evaluate_at(key_time).opacity, 0.25);
        reopened_mask.validate_author_state().expect("valid reopened mask");
    }

    #[test]
    fn audio_processor_schema_and_exact_curve_round_trip_without_plugin_resolution() {
        let root = unique_temp_dir("audio-parameter-schema-round-trip");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite placeholder").expect("write db");
        let project_path = root.join("audio-parameter-schema.mdp");

        let mut document = test_document();
        let sequence = document.sequences.active_mut().expect("active sequence");
        let track_id = sequence.audio_tracks[0].id;
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        let mut curve = processor.parameters[&parameter_id].automation.clone();
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, -6.0))
            .expect("gain key");
        processor.set_parameter_automation(curve).expect("schema-compatible curve");
        let mut sample_delay =
            AudioProcessorInstance::built_in(BUILTIN_SAMPLE_DELAY_DEFINITION_ID, 1);
        let delay_parameter_id = ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID);
        sample_delay
            .set_parameter_automation(
                ExactAutomationCurve::new(delay_parameter_id.clone(), 128.0)
                    .expect("exact delay value"),
            )
            .expect("schema-compatible sample delay");
        let lookahead_limiter =
            AudioProcessorInstance::built_in(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, 1);
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(processor);
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(sample_delay);
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(lookahead_limiter);

        save_project_archive(&document, &db_path, &project_path).expect("save project");
        let reopened = read_project_document_from_archive(&project_path).expect("reopen project");
        let sequence = reopened.sequences.active().expect("active reopened sequence");
        let parameter =
            &sequence.audio_program.track_channels[&track_id].strip.pre_fader.processors[0]
                .parameters[&parameter_id];

        assert_eq!(parameter.schema.parameter_id, parameter_id);
        assert_eq!(parameter.schema.default_value, PropertyValue::Double(0.0));
        assert_eq!(parameter.automation.keyframes[0].value, -6.0);
        let delay = &sequence.audio_program.track_channels[&track_id].strip.pre_fader.processors[1]
            .parameters[&delay_parameter_id];
        assert_eq!(delay.schema.default_value, PropertyValue::Int(0));
        assert_eq!(delay.schema.unit, ParameterUnit::Samples);
        assert!(!delay.schema.is_animatable);
        assert_eq!(delay.automation.default_value, 128.0);
        assert!(delay.automation.keyframes.is_empty());
        let lookahead =
            &sequence.audio_program.track_channels[&track_id].strip.pre_fader.processors[2]
                .parameters[&ParameterId::new_static(LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID)];
        assert_eq!(lookahead.schema.unit, ParameterUnit::Milliseconds);
        assert!(!lookahead.schema.is_animatable);
        assert_eq!(lookahead.automation.default_value, 5.0);
    }

    #[test]
    fn project_validation_rejects_audio_processor_values_outside_schema() {
        let mut document = test_document();
        let sequence = document.sequences.active_mut().expect("active sequence");
        let track_id = sequence.audio_tracks[0].id;
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        processor
            .parameters
            .get_mut(&parameter_id)
            .expect("gain parameter")
            .automation
            .default_value = 25.0;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("track channel")
            .strip
            .pre_fader
            .processors
            .push(processor);

        let error = document
            .validate()
            .expect_err("invalid audio parameter must not enter a snapshot");
        assert!(format!("{error:#}").contains("violates its schema"));
    }

    #[test]
    fn project_validation_rejects_deserialized_parameter_state_outside_hard_range() {
        let mut encoded = serde_json::to_value(test_document()).expect("serialize document");
        encoded["sequences"]["sequences"][0]["video_tracks"][0]["opacity"]["static_value"]
            ["Float"] = serde_json::json!(2.0);
        let document: ProjectDocument =
            serde_json::from_value(encoded).expect("structurally decodable document");

        let error = document
            .validate()
            .expect_err("invalid author value must not enter a project snapshot");
        assert!(format!("{error:#}").contains("outside [0, 1]"));
    }

    #[test]
    fn archive_without_manifest_is_rejected() {
        let root = unique_temp_dir("missing-manifest");
        let project_path = root.join("legacy.mdp");
        let file = fs::File::create(&project_path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file(PROJECT_ENTRY, options).expect("start project");
        writer.write_all(b"{}").expect("write project");
        writer.finish().expect("finish archive");

        let err = read_project_document_from_archive(&project_path)
            .expect_err("legacy archive should be rejected");
        assert!(err.to_string().contains("manifest"));
    }

    #[test]
    fn document_fingerprint_is_stable_for_proxy_asset_order() {
        let mut first = test_document();
        let mut second = first.clone();
        let a = AssetId::new();
        let b = AssetId::new();
        first.proxy_mode_assets = [a, b].into_iter().collect();
        second.proxy_mode_assets = [b, a].into_iter().collect();

        assert_eq!(
            project_document_fingerprint(first).expect("first fingerprint"),
            project_document_fingerprint(second).expect("second fingerprint")
        );
    }

    #[test]
    fn document_fingerprint_includes_exact_standard_package_identity() {
        let current = test_document();
        let mut legacy = current.clone();
        legacy.color_environment = mondrian_core::ProjectColorEnvironment::new(
            mondrian_core::ColorEngine::MondrianStandard {
                package: mondrian_core::MondrianStandardPackageIdentity::V2,
            },
        );

        assert_ne!(
            project_document_fingerprint(current).expect("current package fingerprint"),
            project_document_fingerprint(legacy).expect("legacy package fingerprint")
        );
    }

    fn write_current_fixture_archive(path: &Path, library: &[u8]) {
        let file = fs::File::create(path).expect("create fixture archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file(MANIFEST_ENTRY, options).expect("manifest entry");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/manifest.json"))
            .expect("manifest fixture");
        writer.start_file(PROJECT_ENTRY, options).expect("project entry");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/project.json"))
            .expect("project fixture");
        writer.start_file(LIBRARY_ENTRY, options).expect("library entry");
        writer.write_all(library).expect("library fixture");
        writer.finish().expect("finish fixture archive");
    }

    #[test]
    fn current_fixture_open_is_idempotent_and_save_reopen_preserves_semantics() {
        let root = unique_temp_dir("current-fixture");
        let source = root.join("current.mdp");
        write_current_fixture_archive(&source, b"sqlite-current-fixture");

        let first = read_project_document_from_archive(&source).expect("first open");
        let second = read_project_document_from_archive(&source).expect("second open");
        assert_eq!(
            first.color_environment.engine(),
            &mondrian_core::ColorEngine::mondrian_standard()
        );
        let first_context = first
            .sequences
            .active()
            .expect("active current-fixture sequence")
            .settings
            .root_program_color_context(&first.color_environment);
        assert_eq!(
            first_context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert_eq!(
            project_document_fingerprint(first.clone()).expect("first fingerprint"),
            project_document_fingerprint(second).expect("second fingerprint")
        );

        let runtime = root.join("runtime");
        let loaded = load_project_archive(&source, &runtime).expect("load fixture");
        assert_eq!(
            loaded.library_schema_version,
            PROJECT_LIBRARY_SCHEMA_VERSION
        );
        let resaved = root.join("resaved.mdp");
        save_project_archive(&loaded.document, &runtime.join("index.db"), &resaved)
            .expect("resave fixture");
        let reopened = read_project_document_from_archive(&resaved).expect("reopen saved fixture");
        assert_eq!(
            project_document_fingerprint(loaded.document).expect("loaded fingerprint"),
            project_document_fingerprint(reopened).expect("reopened fingerprint")
        );
    }

    #[test]
    fn legacy_standard_v2_archive_reopens_without_visual_identity_drift() {
        let root = unique_temp_dir("legacy-standard-v2");
        let db_path = root.join("index.db");
        fs::write(&db_path, b"sqlite legacy Standard v2").expect("write legacy library");
        let project_path = root.join("legacy-standard-v2.mdp");
        let mut document = test_document();
        let v2_engine = mondrian_core::ColorEngine::MondrianStandard {
            package: mondrian_core::MondrianStandardPackageIdentity::V2,
        };
        document.color_environment = mondrian_core::ProjectColorEnvironment::new(v2_engine.clone());

        save_project_archive(&document, &db_path, &project_path)
            .expect("save legacy Standard v2 archive");
        let reopened = read_project_document_from_archive(&project_path)
            .expect("reopen legacy Standard v2 archive");
        assert_eq!(reopened.color_environment.engine(), &v2_engine);
        let context = reopened
            .sequences
            .active()
            .expect("active reopened legacy sequence")
            .settings
            .root_program_color_context(&reopened.color_environment);
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard_package(
                mondrian_core::MondrianStandardPackageIdentity::V2,
            )
        );
        let (display, view) = context
            .output_transform
            .resolve_display_view(
                mondrian_core::ColorSpace::Rec709,
                reopened.color_environment.engine(),
            )
            .expect("resolve legacy Standard v2 output")
            .expect("legacy Standard v2 display/view");
        assert_eq!(display, "Rec.1886 Rec.709 - Display");
        assert_eq!(view, "Mondrian Standard SDR v1");
    }

    #[test]
    fn older_schemas_are_rejected_without_an_alpha_compatibility_migration() {
        let mut legacy = serde_json::to_value(test_document()).expect("serialize document");
        for version in [5, 6, 10, 14, 17] {
            legacy["schema_version"] = serde_json::json!(version);
            let err = DOCUMENT_MIGRATIONS
                .migrate(legacy.clone())
                .expect_err("older schemas must not migrate implicitly");
            assert!(err.to_string().contains("missing project document migration"));
        }
    }

    #[test]
    fn current_schema_requires_complete_project_color_identity() {
        let value = serde_json::to_value(test_document()).expect("serialize document");

        let mut missing_engine = value.clone();
        missing_engine["color_environment"]
            .as_object_mut()
            .expect("color-environment object")
            .remove("engine");
        assert!(serde_json::from_value::<ProjectDocument>(missing_engine).is_err());

        let mut missing_color_environment = value;
        missing_color_environment
            .as_object_mut()
            .expect("Project document object")
            .remove("color_environment");
        assert!(serde_json::from_value::<ProjectDocument>(missing_color_environment).is_err());

        let mut missing_hdr_view =
            serde_json::to_value(test_document()).expect("serialize document");
        missing_hdr_view["color_environment"]["engine"]["package"]
            .as_object_mut()
            .expect("Standard package identity")
            .remove("hdr_view_transform_id");
        assert!(serde_json::from_value::<ProjectDocument>(missing_hdr_view).is_err());
    }

    #[test]
    fn current_schema_rejects_removed_aces_sequence_workflow() {
        let mut value = serde_json::to_value(test_document()).expect("serialize document");
        value["sequences"]["sequences"][0]["settings"]["color"]["program_output"]["workflow"] =
            serde_json::json!("Aces");

        let error = serde_json::from_value::<ProjectDocument>(value)
            .expect_err("project mode must not be duplicated by an ACES sequence workflow");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn current_schema_rejects_sequence_engine_and_inheritance_fields() {
        for field in ["engine", "inherit_project_engine"] {
            let mut value = serde_json::to_value(test_document()).expect("serialize document");
            value["sequences"]["sequences"][0]["settings"]["color"][field] =
                serde_json::json!("MondrianStandard");

            let error = serde_json::from_value::<ProjectDocument>(value)
                .expect_err("Sequence must not persist a Project-owned engine contract");
            assert!(error.to_string().contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn missing_project_custom_ocio_preserves_author_intent_but_blocks_engine_prepare() {
        let mut document = test_document();
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-project-custom-ocio-{}.ocio",
            std::process::id()
        ));
        document.color_environment =
            mondrian_core::ProjectColorEnvironment::new(missing_custom_engine(missing_path));
        document
            .validate()
            .expect("unavailable external resources do not corrupt the author snapshot");
        let round_tripped: ProjectDocument =
            serde_json::from_value(serde_json::to_value(&document).expect("serialize Project"))
                .expect("preserve unavailable Custom OCIO intent");
        assert_eq!(round_tripped.color_environment, document.color_environment);

        let error = round_tripped
            .color_environment
            .engine()
            .ensure_loaded()
            .expect_err("missing Project Custom OCIO config must block execution prepare");
        assert!(
            format!("{error:#}").contains("OCIO config file not found"),
            "{error:#}"
        );
    }

    #[test]
    fn document_validation_rejects_custom_ocio_working_space_mismatch() {
        let mut document = test_document();
        document.color_environment = mondrian_core::ProjectColorEnvironment::new(
            missing_custom_engine(PathBuf::from("E:/studio/config.ocio")),
        );
        document
            .sequences
            .active_mut()
            .expect("active sequence")
            .settings
            .color
            .working_color_space = mondrian_core::WorkingColorSpace::AcesCg;

        let error = document
            .validate()
            .expect_err("document must reject an unpinned Custom OCIO working space");

        assert!(format!("{error:#}").contains("pins working space 'Linear Rec.2020'"));
    }

    #[test]
    fn document_validation_rejects_standard_working_space_mismatch() {
        let mut document = test_document();
        document
            .sequences
            .active_mut()
            .expect("active sequence")
            .settings
            .color
            .working_color_space = mondrian_core::WorkingColorSpace::LinearP3D65;

        let error = document
            .validate()
            .expect_err("Standard project must reject a non-versioned working space");

        assert!(format!("{error:#}").contains("Mondrian Standard"));
        assert!(format!("{error:#}").contains("Linear Rec.2020"));
    }

    #[test]
    fn failed_archive_open_does_not_modify_source_or_existing_runtime_library() {
        let root = unique_temp_dir("failed-open-preserves-source");
        let source = root.join("invalid.mdp");
        let file = fs::File::create(&source).expect("archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file(MANIFEST_ENTRY, options).expect("manifest");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/manifest.json"))
            .expect("manifest fixture");
        writer.start_file(PROJECT_ENTRY, options).expect("project");
        writer
            .write_all(include_bytes!("../tests/fixtures/current/project.json"))
            .expect("project fixture");
        writer.finish().expect("finish invalid archive");
        let source_before = fs::read(&source).expect("source before");
        let runtime = root.join("runtime");
        fs::create_dir_all(&runtime).expect("runtime");
        fs::write(runtime.join("index.db"), b"existing-runtime").expect("runtime db");

        assert!(load_project_archive(&source, &runtime).is_err());

        assert_eq!(fs::read(&source).expect("source after"), source_before);
        assert_eq!(
            fs::read(runtime.join("index.db")).expect("runtime after"),
            b"existing-runtime"
        );
    }
}
