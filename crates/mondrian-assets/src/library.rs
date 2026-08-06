//! 素材库主入口

use crate::{
    audio_catalog::AssetAudioComponentCatalog,
    migration::migrate_asset_library,
    native_path::{
        ordinary_absolute_path, ordinary_canonical_path, ordinary_sibling_anchor,
        persisted_file_path, persisted_path_text, sqlite_open_path,
    },
};
use mondrian_core::{
    is_picture_file_extension,
    timeline_data::AssetMediaInterpretation,
    types::{AssetId, AssetSource},
    AudioSourceComponentId, AudioSourceSelection, MediaFileFingerprint, MediaInfo, MondrianError,
    Result,
};
use mondrian_storage::OwnedPublicationFile;
use parking_lot::Mutex;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

/// Persisted Asset Library classification used by product routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    /// Time-varying file-backed picture, optionally with linked audio.
    Video,
    /// File-backed single-picture source with placement-owned hold duration.
    StillImage,
    /// File-backed audio without meaningful picture.
    Audio,
    /// Sequence-local generated adjustment-layer source.
    AdjustmentLayer,
    /// Sequence-local generated solid-color source.
    SolidColor,
}

/// Project-Library membership of one strong Asset record.
///
/// Retirement changes only ordinary Library visibility. The record identity,
/// expected media/component contract, and recoverable provider binding remain
/// available to Timeline execution, persistence, and Undo/Redo snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetLibraryMembership {
    /// The record appears in ordinary Asset Library listings.
    Visible,
    /// The record is hidden from ordinary listings but remains resolvable.
    Retired {
        /// UTC timestamp of the committed membership change.
        at: String,
    },
}

impl AssetLibraryMembership {
    /// Whether the record is hidden from ordinary Asset Library listings.
    pub const fn is_retired(&self) -> bool {
        matches!(self, Self::Retired { .. })
    }
}

impl AssetKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::StillImage => "still_image",
            Self::Audio => "audio",
            Self::AdjustmentLayer => "adjustment_layer",
            Self::SolidColor => "solid_color",
        }
    }

    fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "video" => Some(Self::Video),
            "still_image" => Some(Self::StillImage),
            "audio" => Some(Self::Audio),
            "adjustment_layer" => Some(Self::AdjustmentLayer),
            "solid_color" => Some(Self::SolidColor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: AssetId,
    pub name: String,
    pub kind: AssetKind,
    /// Closed source identity. Generated Assets never carry a file path.
    pub source: AssetSource,
    /// File revision whose probe facts are stored, or `None` when the Asset is
    /// generated or requires a fresh probe.
    #[serde(default)]
    source_fingerprint: Option<MediaFileFingerprint>,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub interpretation: AssetMediaInterpretation,
    /// Stable logical audio Components and their conservative stream bindings.
    #[serde(default)]
    pub audio_components: AssetAudioComponentCatalog,
    /// Persisted file probe facts. Generated Assets carry `None`.
    media_info: Option<MediaInfo>,
    pub created_at: String,
    pub updated_at: String,
    /// Library visibility, independent from the record's strong identity.
    pub membership: AssetLibraryMembership,
}

/// Result of one atomic Asset Library removal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetLibraryRemovalOutcome {
    /// Visible Asset records changed to retired membership.
    pub retired_assets: usize,
    /// Folder records removed after their Assets were unlinked.
    pub deleted_folders: usize,
}

/// Result of one atomic Asset Library organization transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetLibraryMoveOutcome {
    /// Distinct visible Asset records whose folder membership changed.
    pub moved_assets: usize,
    /// Distinct folder records whose parent changed.
    pub moved_folders: usize,
}

impl AssetRecord {
    /// Canonical path for a file-backed Asset.
    pub fn file_path(&self) -> Option<&Path> {
        match &self.source {
            AssetSource::File(path) => Some(path),
            AssetSource::Generated(_) | AssetSource::Remote(_) => None,
        }
    }

    /// Probe facts only when a file revision has been admitted.
    pub fn media_probe(&self) -> Option<&MediaInfo> {
        if self.needs_reprobe() {
            None
        } else {
            self.media_info.as_ref()
        }
    }

    /// Complete admitted bounded revision evidence for a file-backed Asset.
    pub fn source_fingerprint(&self) -> Option<MediaFileFingerprint> {
        if self.needs_reprobe() {
            None
        } else {
            self.source_fingerprint
        }
    }

    /// Resolve one stable logical audio Component to its admitted physical
    /// stream and exact source revision.
    pub fn admitted_audio_source_selection(
        &self,
        component_id: AudioSourceComponentId,
    ) -> Option<AudioSourceSelection> {
        let fingerprint = self.source_fingerprint()?;
        let probe = self.media_probe()?;
        let stream =
            self.audio_components.resolve_current(component_id, probe, fingerprint).ok()?;
        Some(AudioSourceSelection::from_stream(stream, fingerprint))
    }

    /// Whether this file Asset requires a fresh probe before execution.
    pub fn needs_reprobe(&self) -> bool {
        match (
            &self.source,
            self.media_info.as_ref(),
            self.source_fingerprint,
        ) {
            (AssetSource::Generated(_), None, None) => false,
            (AssetSource::File(_), Some(probe), Some(fingerprint)) => {
                !fingerprint.authorizes_reuse()
                    || fingerprint.len != Some(probe.file_size)
                    || self.audio_components.source_fingerprint != fingerprint
            }
            (AssetSource::File(_), _, _) | (AssetSource::Remote(_), _, _) => true,
            (AssetSource::Generated(_), _, _) => true,
        }
    }
}

/// Immutable, already-probed file candidate admitted at the Asset Library Seam.
///
/// Construction validates path/fingerprint/probe agreement against the current
/// file. The commit Interface rechecks the physical file fingerprint
/// immediately before the SQLite transaction, so a delayed or superseded media
/// task cannot publish stale probe facts.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetMediaProbeCandidate {
    path: PathBuf,
    path_text: String,
    source_fingerprint: MediaFileFingerprint,
    media_info: MediaInfo,
}

impl AssetMediaProbeCandidate {
    /// Canonicalize one ordinary file path and validate its complete revision
    /// token and probe snapshot.
    pub fn new(
        path: PathBuf,
        source_fingerprint: MediaFileFingerprint,
        media_info: MediaInfo,
    ) -> Result<Self> {
        if !path.is_absolute() {
            return Err(MondrianError::AssetDbError {
                reason: "media probe candidate path is not absolute".to_owned(),
            });
        }
        let canonical_path =
            ordinary_canonical_path(&path).map_err(|error| MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        let path_text =
            persisted_path_text(&canonical_path).map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "media probe candidate path cannot be persisted losslessly: {error}"
                ),
            })?;
        if !source_fingerprint.authorizes_reuse() {
            return Err(MondrianError::AssetDbError {
                reason: "media probe candidate has no complete source-revision evidence".to_owned(),
            });
        }
        if source_fingerprint.len != Some(media_info.file_size) {
            return Err(MondrianError::AssetDbError {
                reason: "media probe candidate file size disagrees with source fingerprint"
                    .to_owned(),
            });
        }
        if MediaFileFingerprint::capture(&canonical_path) != source_fingerprint {
            return Err(MondrianError::AssetDbError {
                reason: "media source changed before probe candidate preparation".to_owned(),
            });
        }
        Ok(Self {
            path: canonical_path,
            path_text,
            source_fingerprint,
            media_info,
        })
    }

    /// Canonical file path whose revision was probed.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Exact file revision whose probe facts are carried.
    pub const fn source_fingerprint(&self) -> MediaFileFingerprint {
        self.source_fingerprint
    }

    /// Stable immutable probe snapshot.
    pub const fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }
}

/// A folder / bin in the asset library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderRecord {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
}

pub struct AssetLibrary {
    root: PathBuf,
    db: Arc<Mutex<Connection>>,
}

/// Identity-bound online SQLite backup owned by one persistence request.
///
/// The path is an opaque sibling selected by `mondrian-storage`, not the
/// caller's sibling anchor. Dropping this guard cleans a path only while
/// identity evidence still says it names the file object SQLite wrote.
pub struct AssetLibrarySnapshot {
    file: OwnedPublicationFile,
}

impl fmt::Debug for AssetLibrarySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssetLibrarySnapshot")
            .field("path", &self.path())
            .finish_non_exhaustive()
    }
}

impl AssetLibrarySnapshot {
    /// Absolute opaque path of the complete SQLite backup.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Clone the retained exact-object handle for a downstream streaming
    /// consumer. The consumer must not reopen [`Self::path`], because a
    /// pathname is not object-identity evidence.
    pub fn try_clone_reader(&self) -> Result<std::fs::File> {
        self.file
            .file()
            .and_then(|file| file.try_clone().map_err(anyhow::Error::from))
            .map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "clone retained SQLite snapshot {} failed: {error:#}",
                    self.path().display()
                ),
            })
    }
}

fn connection_revision(connection: &Connection) -> Result<u64> {
    let revision = connection
        .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
        .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
    u64::try_from(revision).map_err(|_| MondrianError::AssetDbError {
        reason: format!("SQLite returned a negative total_changes value: {revision}"),
    })
}

fn validate_folder_reparent(
    folders: &[(String, Option<String>)],
    folder_id: &str,
    parent_folder_id: Option<&str>,
) -> Result<()> {
    if !folders.iter().any(|(id, _)| id == folder_id) {
        return Err(MondrianError::AssetDbError {
            reason: format!("文件夹不存在：{folder_id}")
        });
    }
    let Some(parent_id) = parent_folder_id else {
        return Ok(());
    };
    if parent_id == folder_id {
        return Err(MondrianError::AssetDbError {
            reason: "不能将文件夹移动到自身".to_owned()
        });
    }

    let mut descendants = vec![folder_id.to_owned()];
    let mut index = 0usize;
    while index < descendants.len() {
        let current = descendants[index].clone();
        for (id, parent) in folders {
            if parent.as_deref() == Some(current.as_str()) && !descendants.contains(id) {
                descendants.push(id.clone());
            }
        }
        index += 1;
    }
    if descendants.iter().any(|id| id == parent_id) {
        return Err(MondrianError::AssetDbError {
            reason: "不能将文件夹移动到自身的子文件夹".to_owned(),
        });
    }
    Ok(())
}

impl AssetLibrary {
    /// 打开或创建素材库（root 为库根目录）
    pub fn open(root: PathBuf) -> Result<Arc<Self>> {
        let root = ordinary_absolute_path(&root).map_err(|error| MondrianError::AssetDbError {
            reason: format!("invalid Asset Library root path: {error}"),
        })?;
        std::fs::create_dir_all(&root)?;
        let root = ordinary_canonical_path(&root).map_err(|error| MondrianError::AssetDbError {
            reason: format!("freeze Asset Library root path failed: {error}"),
        })?;
        let db_path = root.join("index.db");
        let sqlite_path = sqlite_open_path(&db_path)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;
        let mut conn = Connection::open(&sqlite_path)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;
        migrate_asset_library(&mut conn)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: format!("{e:#}") })?;

        info!("Asset library opened at {:?}", root);
        Ok(Arc::new(Self { root, db: Arc::new(Mutex::new(conn)) }))
    }

    /// Path of the live project-library database.
    ///
    /// Persistence callers must use [`Self::snapshot_database`] rather than
    /// copying this file while the connection is active.
    pub fn database_path(&self) -> PathBuf {
        self.root.join("index.db")
    }

    /// Return the connection-local SQLite change revision used to bind an
    /// author snapshot to the exact asset-library state it observed.
    pub fn database_revision(&self) -> Result<u64> {
        connection_revision(&self.db.lock())
    }

    /// Create a transactionally consistent standalone SQLite snapshot.
    ///
    /// The live database may use WAL and remain open while persistence runs.
    /// Copying `index.db` directly is therefore forbidden: the SQLite online
    /// backup API is the sole archive snapshot boundary. `sibling_anchor`
    /// selects an already-existing parent directory only; its leaf has no
    /// identity semantics. The returned guard owns a unique opaque sibling and
    /// never creates, replaces, or deletes the anchor itself.
    pub fn snapshot_database(
        &self,
        expected_revision: u64,
        sibling_anchor: &Path,
    ) -> Result<AssetLibrarySnapshot> {
        let sibling_anchor = ordinary_sibling_anchor(sibling_anchor).map_err(|error| {
            MondrianError::AssetDbError {
                reason: format!("invalid SQLite snapshot sibling anchor: {error}"),
            }
        })?;
        if sibling_anchor == self.database_path() {
            return Err(MondrianError::AssetDbError {
                reason: "database snapshot sibling anchor aliases the live database".to_owned(),
            });
        }
        let source = self.db.lock();
        let actual_revision = connection_revision(&source)?;
        if actual_revision != expected_revision {
            return Err(MondrianError::AssetDbError {
                reason: format!(
                    "asset library changed after snapshot capture: expected revision {expected_revision}, current {actual_revision}"
                ),
            });
        }
        let staging =
            OwnedPublicationFile::create_sibling(&sibling_anchor, "asset-library-snapshot")
                .map_err(|error| MondrianError::AssetDbError {
                    reason: format!(
                        "allocate identity-bound SQLite snapshot beside {} failed: {error:#}",
                        sibling_anchor.display()
                    ),
                })?;
        let reservation = staging.release_for_external_writer();
        let staging_path = reservation.path().to_path_buf();
        let sqlite_destination =
            sqlite_open_path(&staging_path).map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "adapt SQLite snapshot path {} failed: {error}",
                    staging_path.display()
                ),
            })?;
        let mut target =
            Connection::open(sqlite_destination).map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "open identity-bound SQLite snapshot {} failed: {error}",
                    staging_path.display()
                ),
            })?;
        target
            .pragma_update(None, "journal_mode", "OFF")
            .and_then(|_| target.pragma_update(None, "synchronous", "OFF"))
            .map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "configure private SQLite snapshot {} failed: {error}",
                    staging_path.display()
                ),
            })?;
        let backup = rusqlite::backup::Backup::new(&source, &mut target).map_err(|error| {
            MondrianError::AssetDbError {
                reason: format!(
                    "initialize SQLite online backup from {} to {} failed: {error}",
                    self.database_path().display(),
                    staging_path.display()
                ),
            }
        })?;
        backup
            .run_to_completion(128, std::time::Duration::from_millis(1), None)
            .map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "copy SQLite online backup from {} to {} failed: {error}",
                    self.database_path().display(),
                    staging_path.display()
                ),
            })?;
        drop(backup);
        let snapshot_journal_mode = target
            .query_row("PRAGMA journal_mode = DELETE", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "normalize standalone SQLite snapshot {} journal mode failed: {error}",
                    staging_path.display()
                ),
            })?;
        if !snapshot_journal_mode.eq_ignore_ascii_case("delete") {
            return Err(MondrianError::AssetDbError {
                reason: format!(
                    "standalone SQLite snapshot {} retained unexpected journal mode `{snapshot_journal_mode}`",
                    staging_path.display()
                ),
            });
        }
        target.close().map_err(|(_, error)| MondrianError::AssetDbError {
            reason: format!(
                "close completed SQLite snapshot {} failed: {error}",
                staging_path.display()
            ),
        })?;
        let mut file = reservation.reclaim().map_err(|error| MondrianError::AssetDbError {
            reason: format!(
                "reclaim identity-bound SQLite snapshot {} failed: {error:#}",
                staging_path.display()
            ),
        })?;
        file.file_mut()
            .and_then(|file| file.sync_all().map_err(anyhow::Error::from))
            .map_err(|error| MondrianError::AssetDbError {
                reason: format!(
                    "flush SQLite snapshot {} failed: {error:#}",
                    staging_path.display()
                ),
            })?;
        Ok(AssetLibrarySnapshot { file })
    }

    /// Atomically insert or update one already-probed file Asset and its folder.
    ///
    /// The media Adapter performs canonicalization and FFmpeg probing before
    /// crossing this Seam. This transaction publishes source facts, stable
    /// audio bindings, and folder placement together.
    pub fn commit_media_probe(
        &self,
        candidate: AssetMediaProbeCandidate,
        folder_id: Option<&str>,
    ) -> Result<AssetId> {
        let AssetMediaProbeCandidate {
            path: canonical_path,
            path_text,
            source_fingerprint,
            media_info: info,
        } = candidate;
        let kind = detect_asset_kind(&info, &canonical_path)?;

        let name = canonical_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("untitled")
            .to_string();

        let now = chrono::Utc::now().to_rfc3339();
        let mut db = self.db.lock();
        let transaction = db
            .transaction()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        if let Some(folder_id) = folder_id {
            let folder_exists = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1)",
                    [folder_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            if !folder_exists {
                return Err(MondrianError::AssetDbError {
                    reason: format!("target Asset folder does not exist: {folder_id}"),
                });
            }
        }

        let existing = transaction
            .query_row(
                "SELECT id, audio_components FROM assets WHERE path = ?1",
                rusqlite::params![&path_text],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let (id, audio_components) = match existing {
            Some((existing_id, existing_catalog)) => {
                let parsed = Uuid::parse_str(&existing_id).map_err(|e| {
                    MondrianError::AssetDbError { reason: format!("invalid asset id in db: {e}") }
                })?;
                let catalog = serde_json::from_str::<AssetAudioComponentCatalog>(&existing_catalog)
                    .map_err(|e| MondrianError::AssetDbError {
                        reason: format!("invalid audio Component catalog in db: {e}"),
                    })?
                    .reconcile(&info, source_fingerprint)
                    .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
                (AssetId(parsed), catalog)
            }
            None => (
                AssetId::new(),
                AssetAudioComponentCatalog::from_media_info(&info, source_fingerprint),
            ),
        };

        let metadata_json = serde_json::to_string(&info)?;
        let source_fingerprint_json = serde_json::to_string(&source_fingerprint)?;
        let audio_components_json = serde_json::to_string(&audio_components)?;
        let interpretation_json = serde_json::to_string(&AssetMediaInterpretation::default())?;
        if MediaFileFingerprint::capture(&canonical_path) != source_fingerprint {
            return Err(MondrianError::AssetDbError {
                reason: "media source changed before Asset probe commit".to_owned(),
            });
        }
        transaction
            .execute(
                "INSERT INTO assets \
             (id, name, asset_type, path, folder_id, tags, metadata, source_fingerprint, interpretation, audio_components, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11) \
             ON CONFLICT(path) DO UPDATE SET \
                name = excluded.name, \
                asset_type = excluded.asset_type, \
                folder_id = excluded.folder_id, \
                metadata = excluded.metadata, \
                source_fingerprint = excluded.source_fingerprint, \
                audio_components = excluded.audio_components, \
                retired_at = NULL, \
                updated_at = excluded.updated_at",
                rusqlite::params![
                    id.0.to_string(),
                    name,
                    kind.as_str(),
                    path_text,
                    folder_id,
                    "[]",
                    metadata_json,
                    source_fingerprint_json,
                    interpretation_json,
                    audio_components_json,
                    now
                ],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        transaction
            .commit()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;

        Ok(id)
    }

    pub fn create_adjustment_layer_asset(&self, name: Option<&str>) -> Result<AssetId> {
        let now = chrono::Utc::now().to_rfc3339();
        let asset_id = AssetId::new();
        let asset_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.next_adjustment_layer_name());
        // The asset is a reusable palette/template entry. Any adjustment parameter state
        // belongs to the timeline instance created from it, not this asset record.
        let storage_key = generated_adjustment_storage_key(asset_id);
        let metadata_json = "null";
        let interpretation_json = serde_json::to_string(&AssetMediaInterpretation::default())?;
        let audio_components_json = serde_json::to_string(&AssetAudioComponentCatalog::default())?;
        let db = self.db.lock();

        db.execute(
            "INSERT INTO assets \
             (id, name, asset_type, path, tags, metadata, interpretation, audio_components, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            rusqlite::params![
                asset_id.0.to_string(),
                asset_name,
                AssetKind::AdjustmentLayer.as_str(),
                storage_key,
                "[]",
                metadata_json,
                interpretation_json,
                audio_components_json,
                now
            ],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        Ok(asset_id)
    }

    pub fn create_solid_color_asset(&self, name: Option<&str>) -> Result<AssetId> {
        let now = chrono::Utc::now().to_rfc3339();
        let asset_id = AssetId::new();
        let asset_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.next_solid_color_name());
        let storage_key = generated_solid_storage_key(asset_id);
        let metadata_json = "null";
        let interpretation_json = serde_json::to_string(&AssetMediaInterpretation::default())?;
        let audio_components_json = serde_json::to_string(&AssetAudioComponentCatalog::default())?;
        let db = self.db.lock();

        db.execute(
            "INSERT INTO assets \
             (id, name, asset_type, path, tags, metadata, interpretation, audio_components, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            rusqlite::params![
                asset_id.0.to_string(),
                asset_name,
                AssetKind::SolidColor.as_str(),
                storage_key,
                "[]",
                metadata_json,
                interpretation_json,
                audio_components_json,
                now
            ],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        Ok(asset_id)
    }

    /// Atomically relink one file Asset to an already-probed candidate.
    pub fn commit_relink_probe(
        &self,
        asset_id: AssetId,
        candidate: AssetMediaProbeCandidate,
    ) -> Result<()> {
        let AssetMediaProbeCandidate {
            path: canonical_path,
            path_text,
            source_fingerprint,
            media_info: info,
        } = candidate;
        let kind = detect_asset_kind(&info, &canonical_path)?;
        let now = chrono::Utc::now().to_rfc3339();
        let metadata_json = serde_json::to_string(&info)?;
        let source_fingerprint_json = serde_json::to_string(&source_fingerprint)?;

        let mut db = self.db.lock();
        let transaction = db
            .transaction()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        let existing = transaction
            .query_row(
                "SELECT asset_type, audio_components FROM assets WHERE id = ?1 LIMIT 1",
                rusqlite::params![asset_id.0.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let Some((existing_kind, existing_catalog)) = existing else {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        };

        let existing_kind = AssetKind::from_db_str(existing_kind.as_str()).ok_or_else(|| {
            MondrianError::AssetDbError {
                reason: format!("Asset {asset_id} has unsupported stored type `{existing_kind}`"),
            }
        })?;
        if existing_kind != kind {
            return Err(MondrianError::AssetDbError {
                reason: format!(
                    "重连类型不匹配：资产类型为 {:?}，新文件识别为 {:?}",
                    existing_kind, kind
                ),
            });
        }

        let audio_components =
            serde_json::from_str::<AssetAudioComponentCatalog>(&existing_catalog)
                .map_err(|e| MondrianError::AssetDbError {
                    reason: format!("invalid audio Component catalog in db: {e}"),
                })?
                .reconcile(&info, source_fingerprint)
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        let audio_components_json = serde_json::to_string(&audio_components)?;

        if MediaFileFingerprint::capture(&canonical_path) != source_fingerprint {
            return Err(MondrianError::AssetDbError {
                reason: "media source changed before relink commit".to_owned(),
            });
        }
        let changed = transaction
            .execute(
                "UPDATE assets SET path = ?1, metadata = ?2, source_fingerprint = ?3, \
                 audio_components = ?4, updated_at = ?5 WHERE id = ?6",
                rusqlite::params![
                    path_text,
                    metadata_json,
                    source_fingerprint_json,
                    audio_components_json,
                    now,
                    asset_id.0.to_string()
                ],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }
        transaction
            .commit()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;

        Ok(())
    }

    /// Commit refreshed probe facts and optionally rebind one stable audio
    /// Component in the same transaction.
    ///
    /// Passing `None` preserves every existing binding and only discovers
    /// unclaimed streams. Passing `Some` is explicit author intent to retarget
    /// one logical Component.
    pub fn commit_audio_component_probe(
        &self,
        asset_id: AssetId,
        candidate: AssetMediaProbeCandidate,
        rebind: Option<(AudioSourceComponentId, u32)>,
    ) -> Result<()> {
        let AssetMediaProbeCandidate {
            path: expected_path,
            path_text,
            source_fingerprint,
            media_info: info,
        } = candidate;
        let kind = detect_asset_kind(&info, &expected_path)?;
        let metadata_json = serde_json::to_string(&info)?;
        let source_fingerprint_json = serde_json::to_string(&source_fingerprint)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut db = self.db.lock();
        let transaction = db
            .transaction()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        let existing = transaction
            .query_row(
                "SELECT path, asset_type, audio_components FROM assets WHERE id = ?1 LIMIT 1",
                rusqlite::params![asset_id.0.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        let Some((stored_path, stored_kind, existing_catalog)) = existing else {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        };
        if stored_path != path_text {
            return Err(MondrianError::AssetDbError {
                reason: "Asset path changed before audio Component catalog commit".to_owned(),
            });
        }
        let stored_kind =
            AssetKind::from_db_str(&stored_kind).ok_or_else(|| MondrianError::AssetDbError {
                reason: format!("Asset {asset_id} has unsupported stored type `{stored_kind}`"),
            })?;
        if stored_kind != kind {
            return Err(MondrianError::AssetDbError {
                reason: "Asset type changed before audio Component catalog commit".to_owned(),
            });
        }

        let reconciled = serde_json::from_str::<AssetAudioComponentCatalog>(&existing_catalog)
            .map_err(|error| MondrianError::AssetDbError {
                reason: format!("invalid audio Component catalog in db: {error}"),
            })?
            .reconcile(&info, source_fingerprint)
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        let audio_components = if let Some((component_id, stream_index)) = rebind {
            reconciled
                .rebind(component_id, stream_index, &info, source_fingerprint)
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?
        } else {
            reconciled
        };
        let audio_components_json = serde_json::to_string(&audio_components)?;
        let current_fingerprint = MediaFileFingerprint::capture(&expected_path);
        if current_fingerprint != source_fingerprint {
            return Err(MondrianError::AssetDbError {
                reason: "audio source changed before Component catalog commit".to_owned(),
            });
        }
        let changed = transaction
            .execute(
                "UPDATE assets SET metadata = ?1, source_fingerprint = ?2, \
                 audio_components = ?3, updated_at = ?4 WHERE id = ?5 AND path = ?6",
                rusqlite::params![
                    metadata_json,
                    source_fingerprint_json,
                    audio_components_json,
                    now,
                    asset_id.0.to_string(),
                    stored_path
                ],
            )
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        if changed != 1 {
            return Err(MondrianError::AssetDbError {
                reason: "Asset changed before audio Component catalog commit".to_owned(),
            });
        }
        transaction
            .commit()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        Ok(())
    }

    pub fn list_assets(&self) -> Result<Vec<AssetRecord>> {
        self.list_assets_in_folder(None)
    }

    /// List only records that remain ordinary Asset Library members.
    ///
    /// Retired records intentionally remain accessible through
    /// [`Self::get_asset`] so Project-contained references never turn into
    /// fabricated offline media.
    pub fn list_assets_in_folder(&self, folder_id: Option<&str>) -> Result<Vec<AssetRecord>> {
        let db = self.db.lock();
        if let Some(fid) = folder_id {
            let mut stmt = db
                .prepare(
                    "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                     , interpretation, audio_components, source_fingerprint, retired_at \
                     FROM assets WHERE folder_id = ?1 AND retired_at IS NULL \
                     ORDER BY updated_at DESC",
                )
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let rows = stmt
                .query_map(rusqlite::params![fid], parse_asset_row)
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })
        } else {
            let mut stmt = db
                .prepare(
                    "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                     , interpretation, audio_components, source_fingerprint, retired_at \
                     FROM assets WHERE retired_at IS NULL ORDER BY updated_at DESC",
                )
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let rows = stmt
                .query_map([], parse_asset_row)
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })
        }
    }

    /// Resolve a Project-contained Asset identity, including retired records.
    ///
    /// `None` means the strong record is genuinely missing/corrupt; callers
    /// must not reinterpret that outcome as an offline recoverable binding.
    pub fn get_asset(&self, asset_id: AssetId) -> Result<Option<AssetRecord>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                 , interpretation, audio_components, source_fingerprint, retired_at \
                 FROM assets WHERE id = ?1 LIMIT 1",
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let result = stmt
            .query_row(rusqlite::params![asset_id.0.to_string()], parse_asset_row)
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        Ok(result)
    }

    /// Persist the user-selected interpretation for an asset.
    ///
    /// This stores only user intent. Automatic detection results remain in
    /// `MediaInfo`/runtime diagnostics and may change as detectors improve.
    pub fn set_asset_interpretation(
        &self,
        asset_id: AssetId,
        interpretation: AssetMediaInterpretation,
    ) -> Result<()> {
        let interpretation_json = serde_json::to_string(&interpretation)?;
        let now = chrono::Utc::now().to_rfc3339();
        let db = self.db.lock();
        let changed = db
            .execute(
                "UPDATE assets SET interpretation = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![interpretation_json, now, asset_id.0.to_string()],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }

        Ok(())
    }

    /// Reset the asset interpretation to automatic detection.
    pub fn reset_asset_interpretation(&self, asset_id: AssetId) -> Result<()> {
        self.set_asset_interpretation(asset_id, AssetMediaInterpretation::default())
    }

    pub fn rename_asset(&self, asset_id: AssetId, new_name: &str) -> Result<()> {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return Err(MondrianError::AssetDbError {
                reason: "素材名不能为空".to_string()
            });
        }

        let db = self.db.lock();
        let changed = db
            .execute(
                "UPDATE assets SET name = ?1 WHERE id = ?2",
                rusqlite::params![trimmed, asset_id.0.to_string()],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }

        Ok(())
    }

    /// Retire visible records as one all-or-nothing Library transaction.
    ///
    /// Retirement hides ordinary Library membership while preserving each
    /// strong `AssetId`, source/provider binding, and expected media/component
    /// contract. Every requested record is preflighted before any update, so a
    /// missing or already-retired member cannot partially retire a batch.
    pub fn retire_assets(&self, asset_ids: &[AssetId]) -> Result<usize> {
        self.retire_assets_and_delete_folders(asset_ids, &[])
            .map(|outcome| outcome.retired_assets)
    }

    /// Atomically retire Assets and remove folder/bin records.
    ///
    /// Folder deletion only removes organization membership: records inside a
    /// removed folder are moved to the Library root unless they are themselves
    /// in `asset_ids`. Asset records are never physically purged by this
    /// Interface.
    pub fn retire_assets_and_delete_folders(
        &self,
        asset_ids: &[AssetId],
        folder_ids: &[String],
    ) -> Result<AssetLibraryRemovalOutcome> {
        let asset_ids = asset_ids.iter().copied().collect::<BTreeSet<_>>();
        let requested_folders = folder_ids.iter().cloned().collect::<BTreeSet<_>>();
        if asset_ids.is_empty() && requested_folders.is_empty() {
            return Ok(AssetLibraryRemovalOutcome { retired_assets: 0, deleted_folders: 0 });
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut db = self.db.lock();
        let transaction = db
            .transaction()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;

        for asset_id in &asset_ids {
            let retired_at = transaction
                .query_row(
                    "SELECT retired_at FROM assets WHERE id = ?1 LIMIT 1",
                    rusqlite::params![asset_id.0.to_string()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            if retired_at.is_none() || retired_at.flatten().is_some() {
                return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
            }
        }

        let folders = {
            let mut statement = transaction
                .prepare("SELECT id, parent_id FROM folders")
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?
        };
        for folder_id in &requested_folders {
            if !folders.iter().any(|(id, _)| id == folder_id) {
                return Err(MondrianError::AssetDbError {
                    reason: format!("文件夹不存在：{folder_id}"),
                });
            }
        }

        let mut removed_folders = requested_folders;
        let mut changed = true;
        while changed {
            changed = false;
            for (folder_id, parent_id) in &folders {
                if parent_id.as_ref().is_some_and(|parent_id| removed_folders.contains(parent_id))
                    && removed_folders.insert(folder_id.clone())
                {
                    changed = true;
                }
            }
        }

        let retired_assets = if asset_ids.is_empty() {
            0
        } else {
            let ids = asset_ids.iter().map(|id| id.0.to_string()).collect::<Vec<_>>();
            let placeholders = std::iter::repeat_n("?", ids.len()).collect::<Vec<_>>().join(",");
            let mut parameters = Vec::with_capacity(ids.len() + 2);
            parameters.push(rusqlite::types::Value::Text(now.clone()));
            parameters.push(rusqlite::types::Value::Text(now.clone()));
            parameters.extend(ids.into_iter().map(rusqlite::types::Value::Text));
            transaction
                .execute(
                    &format!(
                        "UPDATE assets SET retired_at = ?1, updated_at = ?2 \
                         WHERE id IN ({placeholders}) AND retired_at IS NULL"
                    ),
                    params_from_iter(parameters),
                )
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?
        };
        if retired_assets != asset_ids.len() {
            return Err(MondrianError::AssetDbError {
                reason: "Asset retirement changed after batch preflight".to_owned(),
            });
        }

        let expected_deleted_folders = removed_folders.len();
        let deleted_folders = if removed_folders.is_empty() {
            0
        } else {
            let folder_ids = removed_folders.into_iter().collect::<Vec<_>>();
            let placeholders =
                std::iter::repeat_n("?", folder_ids.len()).collect::<Vec<_>>().join(",");
            transaction
                .execute(
                    &format!(
                        "UPDATE assets SET folder_id = NULL WHERE folder_id IN ({placeholders})"
                    ),
                    params_from_iter(folder_ids.iter()),
                )
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            let _changed_rows = transaction
                .execute(
                    &format!("DELETE FROM folders WHERE id IN ({placeholders})"),
                    params_from_iter(folder_ids.iter()),
                )
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            // SQLite's affected-row count excludes rows removed by FK cascade,
            // so verify the exact preflight closure instead of trusting it.
            for folder_id in &folder_ids {
                let still_exists = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1)",
                        [folder_id],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
                if still_exists {
                    return Err(MondrianError::AssetDbError {
                        reason: "Asset folder removal changed after batch preflight".to_owned(),
                    });
                }
            }
            expected_deleted_folders
        };

        transaction
            .commit()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        Ok(AssetLibraryRemovalOutcome { retired_assets, deleted_folders })
    }

    pub fn library_root(&self) -> &Path {
        &self.root
    }

    // ── Folder / Bin CRUD ──────────────────────────────────────────────

    pub fn create_folder(&self, name: &str, parent_id: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let db = self.db.lock();
        db.execute(
            "INSERT INTO folders (id, name, parent_id, sort_order, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 0, ?4, ?4)",
            rusqlite::params![id, name.trim(), parent_id, now],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        Ok(id)
    }

    pub fn rename_folder(&self, folder_id: &str, new_name: &str) -> Result<()> {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return Err(MondrianError::AssetDbError {
                reason: "文件夹名不能为空".to_string()
            });
        }
        let db = self.db.lock();
        let changed = db
            .execute(
                "UPDATE folders SET name = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![trimmed, chrono::Utc::now().to_rfc3339(), folder_id],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        if changed == 0 {
            return Err(MondrianError::AssetDbError {
                reason: format!("文件夹不存在：{folder_id}"),
            });
        }
        Ok(())
    }

    pub fn delete_folder(&self, folder_id: &str) -> Result<()> {
        self.retire_assets_and_delete_folders(&[], &[folder_id.to_owned()])?;
        Ok(())
    }

    pub fn list_folders(&self) -> Result<Vec<FolderRecord>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, name, parent_id, sort_order, created_at, updated_at \
                 FROM folders ORDER BY sort_order, name",
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        let records: Vec<_> = stmt
            .query_map([], |row| {
                Ok(FolderRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    parent_id: row.get(2)?,
                    sort_order: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        Ok(records)
    }

    /// Return whether a folder id exists in the library.
    pub fn folder_exists(&self, folder_id: &str) -> Result<bool> {
        let db = self.db.lock();
        let exists = db
            .query_row(
                "SELECT 1 FROM folders WHERE id = ?1 LIMIT 1",
                rusqlite::params![folder_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?
            .is_some();
        Ok(exists)
    }

    pub fn move_asset_to_folder(&self, asset_id: AssetId, folder_id: Option<&str>) -> Result<()> {
        self.move_assets_and_folders(&[asset_id], &[], folder_id)?;
        Ok(())
    }

    pub fn move_folder(&self, folder_id: &str, parent_folder_id: Option<&str>) -> Result<()> {
        self.move_assets_and_folders(&[], &[folder_id.to_owned()], parent_folder_id)?;
        Ok(())
    }

    /// Atomically move visible Asset records and folders to one Library bin.
    ///
    /// The complete request is deduplicated and validated against one SQLite
    /// transaction before any row changes. Missing/retired Assets, missing
    /// folders, self-parenting, descendant cycles, or a storage failure roll
    /// back the entire request. A record already at the destination is a
    /// successful no-op and is excluded from the returned counts.
    pub fn move_assets_and_folders(
        &self,
        asset_ids: &[AssetId],
        folder_ids: &[String],
        target_folder_id: Option<&str>,
    ) -> Result<AssetLibraryMoveOutcome> {
        let asset_ids = asset_ids.iter().copied().collect::<BTreeSet<_>>();
        let folder_ids = folder_ids.iter().cloned().collect::<BTreeSet<_>>();
        if asset_ids.is_empty() && folder_ids.is_empty() {
            return Ok(AssetLibraryMoveOutcome { moved_assets: 0, moved_folders: 0 });
        }

        let mut db = self.db.lock();
        let transaction = db
            .transaction()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        let folders = {
            let mut statement = transaction
                .prepare("SELECT id, parent_id FROM folders")
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?
        };
        if let Some(target_folder_id) = target_folder_id
            && !folders.iter().any(|(id, _)| id == target_folder_id)
        {
            return Err(MondrianError::AssetDbError {
                reason: format!("目标文件夹不存在：{target_folder_id}"),
            });
        }

        let mut assets_to_move = Vec::with_capacity(asset_ids.len());
        for asset_id in asset_ids {
            let record = transaction
                .query_row(
                    "SELECT folder_id, retired_at FROM assets WHERE id = ?1 LIMIT 1",
                    rusqlite::params![asset_id.0.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            let Some((current_folder_id, retired_at)) = record else {
                return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
            };
            if retired_at.is_some() {
                return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
            }
            if current_folder_id.as_deref() != target_folder_id {
                assets_to_move.push(asset_id);
            }
        }

        let mut folders_to_move = Vec::with_capacity(folder_ids.len());
        for folder_id in folder_ids {
            validate_folder_reparent(&folders, &folder_id, target_folder_id)?;
            let current_parent = folders
                .iter()
                .find(|(id, _)| id == &folder_id)
                .and_then(|(_, parent)| parent.as_deref());
            if current_parent != target_folder_id {
                folders_to_move.push(folder_id);
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        for asset_id in &assets_to_move {
            let changed = transaction
                .execute(
                    "UPDATE assets SET folder_id = ?1, updated_at = ?2 \
                     WHERE id = ?3 AND retired_at IS NULL",
                    rusqlite::params![target_folder_id, now, asset_id.0.to_string()],
                )
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            if changed != 1 {
                return Err(MondrianError::AssetDbError {
                    reason: "Asset folder membership changed after batch preflight".to_owned(),
                });
            }
        }
        for folder_id in &folders_to_move {
            let changed = transaction
                .execute(
                    "UPDATE folders SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![target_folder_id, now, folder_id],
                )
                .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
            if changed != 1 {
                return Err(MondrianError::AssetDbError {
                    reason: "Asset folder parent changed after batch preflight".to_owned(),
                });
            }
        }

        transaction
            .commit()
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        Ok(AssetLibraryMoveOutcome {
            moved_assets: assets_to_move.len(),
            moved_folders: folders_to_move.len(),
        })
    }

    /// Find which timeline clips reference this asset (position reverse lookup).
    /// Returns a list of (asset_id, asset_name) for the UI to select and navigate to.
    pub fn get_asset_location(
        &self,
        asset_id: AssetId,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare("SELECT folder_id, name FROM assets WHERE id = ?1")
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        let result = stmt
            .query_row(rusqlite::params![asset_id.0.to_string()], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        Ok(result)
    }

    fn next_adjustment_layer_name(&self) -> String {
        let db = self.db.lock();
        let mut stmt = match db
            .prepare("SELECT name FROM assets WHERE asset_type = ?1 ORDER BY created_at ASC")
        {
            Ok(stmt) => stmt,
            Err(_) => return "调整图层 1".to_string(),
        };

        let rows = match stmt.query_map(
            rusqlite::params![AssetKind::AdjustmentLayer.as_str()],
            |row| row.get::<_, String>(0),
        ) {
            Ok(rows) => rows,
            Err(_) => return "调整图层 1".to_string(),
        };

        let mut next_index = 1usize;
        for name in rows.filter_map(std::result::Result::ok) {
            let Some(suffix) = name.strip_prefix("调整图层 ") else {
                continue;
            };
            let Ok(index) = suffix.trim().parse::<usize>() else {
                continue;
            };
            next_index = next_index.max(index + 1);
        }

        format!("调整图层 {next_index}")
    }

    fn next_solid_color_name(&self) -> String {
        let db = self.db.lock();
        let mut stmt = match db
            .prepare("SELECT name FROM assets WHERE asset_type = ?1 ORDER BY created_at ASC")
        {
            Ok(stmt) => stmt,
            Err(_) => return "纯色层 1".to_string(),
        };

        let rows = match stmt.query_map(rusqlite::params![AssetKind::SolidColor.as_str()], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(rows) => rows,
            Err(_) => return "纯色层 1".to_string(),
        };

        let mut next_index = 1usize;
        for name in rows.filter_map(std::result::Result::ok) {
            let Some(suffix) = name.strip_prefix("纯色层 ") else {
                continue;
            };
            let Ok(index) = suffix.trim().parse::<usize>() else {
                continue;
            };
            next_index = next_index.max(index + 1);
        }

        format!("纯色层 {next_index}")
    }
}

fn parse_asset_row(row: &rusqlite::Row) -> rusqlite::Result<AssetRecord> {
    let id_raw: String = row.get(0)?;
    let name: String = row.get(1)?;
    let kind_raw: String = row.get(2)?;
    let path_raw: String = row.get(3)?;
    let folder_id: Option<String> = row.get(4)?;
    let metadata_raw: String = row.get(5)?;
    let created_at: String = row.get(6)?;
    let updated_at: String = row.get(7)?;
    let interpretation_raw: String = row.get(8)?;
    let audio_components_raw: String = row.get(9)?;
    let source_fingerprint_raw: Option<String> = row.get(10)?;
    let retired_at: Option<String> = row.get(11)?;

    let asset_id = Uuid::parse_str(&id_raw).map(AssetId).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let media_info = serde_json::from_str::<Option<MediaInfo>>(&metadata_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let source_fingerprint = source_fingerprint_raw
        .map(|raw| {
            serde_json::from_str::<MediaFileFingerprint>(&raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    let interpretation = serde_json::from_str::<AssetMediaInterpretation>(&interpretation_raw)
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
        })?;
    let audio_components =
        serde_json::from_str::<AssetAudioComponentCatalog>(&audio_components_raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
        })?;
    audio_components.validate().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let kind = AssetKind::from_db_str(&kind_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported Asset type `{kind_raw}`"),
            )),
        )
    })?;
    let source = match kind {
        AssetKind::AdjustmentLayer => {
            AssetSource::Generated(mondrian_core::GeneratedAssetKind::AdjustmentLayer)
        }
        AssetKind::SolidColor => {
            AssetSource::Generated(mondrian_core::GeneratedAssetKind::SolidColor)
        }
        AssetKind::Video | AssetKind::StillImage | AssetKind::Audio => {
            let path = persisted_file_path(&path_raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            AssetSource::File(path)
        }
    };

    Ok(AssetRecord {
        id: asset_id,
        name,
        kind,
        source,
        source_fingerprint,
        folder_id,
        interpretation,
        audio_components,
        media_info,
        created_at,
        updated_at,
        membership: retired_at.map_or(AssetLibraryMembership::Visible, |at| {
            AssetLibraryMembership::Retired { at }
        }),
    })
}

fn generated_adjustment_storage_key(asset_id: AssetId) -> String {
    format!("generated:adjustment-layer:{asset_id}")
}

fn generated_solid_storage_key(asset_id: AssetId) -> String {
    format!("generated:solid-color:{asset_id}")
}

fn has_meaningful_video_stream(info: &MediaInfo) -> bool {
    info.video_streams.iter().any(|stream| {
        let fps = stream.frame_rate.to_f64();
        let moving_fps = fps >= 1.0;
        let moving_frames = stream.total_frames.unwrap_or(0) > 1;
        moving_fps || moving_frames
    })
}

fn is_audio_only_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|v| v.to_str()) else {
        return false;
    };

    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aiff" | "aif" | "alac"
    )
}

fn is_proven_single_picture(info: &MediaInfo) -> bool {
    info.primary_video()
        .and_then(|video| video.total_frames)
        .is_some_and(|frames| frames == 1)
}

fn detect_asset_kind(info: &MediaInfo, path: &Path) -> Result<AssetKind> {
    let force_audio =
        info.has_audio && (is_audio_only_extension(path) || !has_meaningful_video_stream(info));
    if force_audio {
        return Ok(AssetKind::Audio);
    }
    if info.has_video && is_picture_file_extension(path) && is_proven_single_picture(info) {
        return Ok(AssetKind::StillImage);
    }
    if info.has_video {
        return Ok(AssetKind::Video);
    }
    if info.has_audio {
        return Ok(AssetKind::Audio);
    }
    Err(MondrianError::UnsupportedFormat { format: info.container.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        timeline_data::{AssetMediaInterpretation, MediaColorInterpretation},
        AudioSourceComponentId, ColorSpace,
    };
    use mondrian_core::{
        AudioCodec, AudioStreamInfo, ChannelLayout, DecodedVideoRange, DetectedColorInterpretation,
        PixelFormat, VideoCodec, VideoCodecProfile, VideoColorDetectionMethod,
        VideoColorInterpretationConfidence, VideoColorInterpretationWarning, VideoColorSpaceSource,
        VideoStreamInfo,
    };
    use std::time::Duration;

    fn open_test_library() -> Arc<AssetLibrary> {
        let dir = tempfile::tempdir().expect("tempdir");
        AssetLibrary::open(dir.keep()).expect("open library")
    }

    fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    }

    #[test]
    fn unknown_persisted_asset_kind_fails_closed() {
        let library = open_test_library();
        let asset_id =
            library.create_solid_color_asset(Some("Corrupt kind")).expect("create Asset");
        library
            .db
            .lock()
            .execute(
                "UPDATE assets SET asset_type = 'future-unknown-kind' WHERE id = ?1",
                rusqlite::params![asset_id.0.to_string()],
            )
            .expect("corrupt stored kind");

        assert!(
            library.get_asset(asset_id).is_err(),
            "unknown persisted kinds must not be reinterpreted as file-backed video"
        );
    }

    #[test]
    fn non_absolute_persisted_file_identity_fails_closed() {
        let library = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("corrupt-path.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let asset_id = commit_info(&library, &media_path, lightweight_video_info(&media_path));
        library
            .db
            .lock()
            .execute(
                "UPDATE assets SET path = 'relative/media.mp4' WHERE id = ?1",
                rusqlite::params![asset_id.0.to_string()],
            )
            .expect("corrupt stored path");

        assert!(
            library.get_asset(asset_id).is_err(),
            "a relative persisted media identity must not escape the library boundary"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_device_namespace_persisted_file_identity_fails_closed() {
        let library = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("corrupt-device.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let asset_id = commit_info(&library, &media_path, lightweight_video_info(&media_path));
        library
            .db
            .lock()
            .execute(
                r"UPDATE assets SET path = '\\.\PhysicalDrive0' WHERE id = ?1",
                rusqlite::params![asset_id.0.to_string()],
            )
            .expect("corrupt stored path");

        assert!(
            library.get_asset(asset_id).is_err(),
            "a persisted device namespace must not be interpreted as media"
        );
    }

    fn probe_candidate(path: &Path, mut info: MediaInfo) -> AssetMediaProbeCandidate {
        let path = ordinary_canonical_path(path).expect("canonical fixture path");
        info.file_size = std::fs::metadata(&path).expect("fixture metadata").len();
        AssetMediaProbeCandidate::new(path.clone(), MediaFileFingerprint::capture(&path), info)
            .expect("valid probe candidate")
    }

    fn commit_info(lib: &AssetLibrary, path: &Path, info: MediaInfo) -> AssetId {
        lib.commit_media_probe(probe_candidate(path, info), None).expect("commit probe")
    }

    fn lightweight_video_info(_path: &Path) -> MediaInfo {
        MediaInfo {
            duration: Duration::from_secs(1),
            file_size: 1,
            container: "mp4".to_string(),
            video_streams: vec![VideoStreamInfo {
                index: 0,
                codec: VideoCodec::H264,
                duration: Some(Duration::from_secs(1)),
                codec_profile: VideoCodecProfile::Unknown,
                width: 1920,
                height: 1080,
                frame_rate: mondrian_core::types::Rational::FPS_30,
                frame_rate_proven: true,
                pixel_format: PixelFormat::Yuv420p,
                pixel_format_proven: true,
                color_range: DecodedVideoRange::Unknown,
                color_interpretation: DetectedColorInterpretation {
                    candidate_color_space: None,
                    confidence: VideoColorInterpretationConfidence::None,
                    source: VideoColorSpaceSource::MissingMetadata,
                    method: VideoColorDetectionMethod::MissingMetadata,
                    evidence: Vec::new(),
                    warnings: vec![VideoColorInterpretationWarning::MissingCicpTags],
                    user_overridable: true,
                },
                color_metadata: None,
                color_metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
                bit_depth: 8,
                has_alpha: false,
                avg_bitrate: 0,
                total_frames: Some(30),
            }],
            audio_streams: Vec::new(),
            has_video: true,
            has_audio: false,
        }
    }

    fn lightweight_audio_info(_path: &Path) -> MediaInfo {
        let stream = |index, stream_id, language: &str, is_default| AudioStreamInfo {
            index,
            stream_id: Some(stream_id),
            language: Some(language.to_owned()),
            title: None,
            is_default,
            codec: AudioCodec::Aac,
            duration: Some(Duration::from_secs(1)),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: ChannelLayout::Stereo,
            bit_depth: 24,
            avg_bitrate: 256_000,
        };
        MediaInfo {
            duration: Duration::from_secs(1),
            file_size: 1,
            container: "mov".to_string(),
            video_streams: Vec::new(),
            audio_streams: vec![stream(1, 10, "eng", false), stream(3, 30, "jpn", true)],
            has_video: false,
            has_audio: true,
        }
    }

    #[test]
    fn online_snapshot_captures_committed_wal_state() {
        let lib = open_test_library();
        lib.db.lock().execute_batch("PRAGMA journal_mode = WAL;").expect("enable WAL");
        let asset_id = lib.create_solid_color_asset(Some("Snapshot red")).expect("create asset");
        let revision = lib.database_revision().expect("database revision");
        let snapshot_dir = tempfile::tempdir().expect("snapshot tempdir");
        let snapshot_path = snapshot_dir.path().join("library.db");

        let snapshot_guard =
            lib.snapshot_database(revision, &snapshot_path).expect("online snapshot");
        assert!(
            !snapshot_path.exists(),
            "the caller-provided sibling anchor must not be claimed"
        );

        let sqlite_snapshot =
            sqlite_open_path(snapshot_guard.path()).expect("adapt snapshot SQLite path");
        let snapshot = Connection::open_with_flags(
            sqlite_snapshot,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open snapshot");
        let stored_name: String = snapshot
            .query_row(
                "SELECT name FROM assets WHERE id = ?1",
                [asset_id.to_string()],
                |row| row.get(0),
            )
            .expect("snapshotted asset");
        assert_eq!(stored_name, "Snapshot red");
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(
                !sqlite_sidecar_path(snapshot_guard.path(), suffix).exists(),
                "private snapshot must not leak a SQLite sidecar"
            );
        }
    }

    #[test]
    fn online_snapshot_fails_closed_when_asset_revision_advanced() {
        let lib = open_test_library();
        let captured = lib.database_revision().expect("captured revision");
        lib.create_adjustment_layer_asset(Some("Changed after capture"))
            .expect("create asset");
        let snapshot_dir = tempfile::tempdir().expect("snapshot tempdir");
        let snapshot_path = snapshot_dir.path().join("library.db");

        let error = lib
            .snapshot_database(captured, &snapshot_path)
            .expect_err("stale snapshot must fail");

        assert!(error.to_string().contains("changed after snapshot capture"));
        assert!(!snapshot_path.exists());
    }

    #[test]
    fn online_snapshot_uses_an_opaque_sibling_without_touching_the_namespace_hint() {
        let lib = open_test_library();
        let revision = lib.database_revision().expect("database revision");
        let snapshot_dir = tempfile::tempdir().expect("snapshot tempdir");
        let snapshot_path = snapshot_dir.path().join("library.db");
        let marker = b"belongs to another snapshot request";
        std::fs::write(&snapshot_path, marker).expect("write destination marker");

        let snapshot_guard = lib
            .snapshot_database(revision, &snapshot_path)
            .expect("a namespace collision must not affect opaque snapshot allocation");
        let preserved = std::fs::read(&snapshot_path).expect("read preserved destination marker");

        assert_eq!(preserved.as_slice(), marker);
        assert_ne!(snapshot_guard.path(), snapshot_path);
        assert!(snapshot_guard.path().is_file());
        let owned_path = snapshot_guard.path().to_path_buf();
        drop(snapshot_guard);
        assert!(
            !owned_path.exists(),
            "owned snapshot must be cleaned on drop"
        );
        assert_eq!(
            std::fs::read(&snapshot_path).expect("read marker after cleanup"),
            marker
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_cleanup_preserves_an_observed_replacement_path() {
        let lib = open_test_library();
        let revision = lib.database_revision().expect("database revision");
        let snapshot_dir = tempfile::tempdir().expect("snapshot tempdir");
        let namespace_hint = snapshot_dir.path().join("library.db");
        let snapshot_guard =
            lib.snapshot_database(revision, &namespace_hint).expect("online snapshot");
        let owned_path = snapshot_guard.path().to_path_buf();
        std::fs::remove_file(&owned_path).expect("detach owned snapshot name");
        let replacement = b"foreign replacement";
        std::fs::write(&owned_path, replacement).expect("install replacement path");

        drop(snapshot_guard);

        assert_eq!(
            std::fs::read(&owned_path).expect("replacement must survive cleanup"),
            replacement
        );
    }

    #[test]
    fn open_and_list_empty() {
        let lib = open_test_library();
        let assets = lib.list_assets().expect("list_assets");
        assert!(assets.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn open_supports_sqlite_sidecars_beyond_legacy_windows_max_path() {
        use std::os::windows::ffi::OsStrExt;

        let parent = tempfile::tempdir().expect("long-path parent");
        let root = parent.path().join("a".repeat(80)).join("b".repeat(80)).join("c".repeat(80));
        let journal_path = root.join("index.db-journal");
        assert!(
            journal_path.as_os_str().encode_wide().count() >= 260,
            "fixture must cross the legacy Windows MAX_PATH boundary"
        );

        let library = AssetLibrary::open(root.clone()).expect("open long-path library");
        library
            .db
            .lock()
            .execute_batch("PRAGMA journal_mode = WAL;")
            .expect("enable long-path WAL");
        let asset_id = library.create_solid_color_asset(Some("Long path")).expect("create Asset");
        let revision = library.database_revision().expect("database revision");
        let snapshot_hint = root.join("snapshot.db");
        let snapshot_guard =
            library.snapshot_database(revision, &snapshot_hint).expect("long-path snapshot");
        let snapshot_path =
            sqlite_open_path(snapshot_guard.path()).expect("adapt long snapshot path");
        let snapshot =
            Connection::open_with_flags(snapshot_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("open long-path snapshot");
        let stored_name: String = snapshot
            .query_row(
                "SELECT name FROM assets WHERE id = ?1",
                [asset_id.to_string()],
                |row| row.get(0),
            )
            .expect("snapshotted long-path asset");

        assert_eq!(stored_name, "Long path");
        assert_eq!(library.database_path(), root.join("index.db"));
        assert!(!snapshot_hint.exists());
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(
                !sqlite_sidecar_path(snapshot_guard.path(), suffix).exists(),
                "private long-path snapshot must not leak a SQLite sidecar"
            );
        }
    }

    #[test]
    fn atomic_probe_commit_registers_preprobed_video() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("preprobed.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");

        let asset_id = commit_info(&lib, &media_path, lightweight_video_info(&media_path));

        let record = lib.get_asset(asset_id).expect("get").expect("asset exists");
        assert_eq!(record.kind, AssetKind::Video);
        assert_eq!(record.name, "preprobed.mp4");
        let expected_path = ordinary_canonical_path(&media_path).expect("ordinary canonical path");
        assert_eq!(record.file_path(), Some(expected_path.as_path()));
        assert!(!record.needs_reprobe());
    }

    #[test]
    fn probe_commit_is_atomic_with_folder_validation() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("atomic.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let candidate = probe_candidate(&media_path, lightweight_video_info(&media_path));

        assert!(lib.commit_media_probe(candidate, Some("missing-folder")).is_err());
        assert!(lib.list_assets().expect("list").is_empty());
    }

    #[test]
    fn probe_candidate_normalizes_one_stable_source_revision() {
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("candidate.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let canonical_path = ordinary_canonical_path(&media_path).expect("canonical fixture path");
        let fingerprint = MediaFileFingerprint::capture(&canonical_path);
        let mut mismatched_info = lightweight_video_info(&canonical_path);
        mismatched_info.file_size = 2;

        assert!(AssetMediaProbeCandidate::new(
            canonical_path.clone(),
            fingerprint,
            mismatched_info
        )
        .is_err());
        let normalized = AssetMediaProbeCandidate::new(
            media_dir.path().join(".").join("candidate.mp4"),
            fingerprint,
            lightweight_video_info(&canonical_path),
        )
        .expect("lexically non-canonical absolute path must normalize");
        assert_eq!(normalized.path(), canonical_path);
    }

    #[cfg(unix)]
    #[test]
    fn probe_candidate_rejects_a_path_the_utf8_schema_cannot_represent() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join(OsString::from_vec(vec![
            b'n', b'o', b'n', b'-', b'u', b't', b'f', b'8', 0xff,
        ]));
        std::fs::write(&media_path, [0u8]).expect("media file");
        let canonical_path = ordinary_canonical_path(&media_path).expect("canonical fixture path");
        let fingerprint = MediaFileFingerprint::capture(&canonical_path);

        let error = AssetMediaProbeCandidate::new(
            canonical_path.clone(),
            fingerprint,
            lightweight_video_info(&canonical_path),
        )
        .expect_err("non-UTF-8 path must fail closed");

        assert!(error.to_string().contains("persisted losslessly"));
    }

    #[test]
    fn failed_folder_assignment_cannot_partially_update_an_existing_asset() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("existing.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let asset_id = commit_info(&lib, &media_path, lightweight_video_info(&media_path));
        let before = lib.get_asset(asset_id).expect("get").expect("asset");

        assert!(lib
            .commit_media_probe(
                probe_candidate(&media_path, lightweight_video_info(&media_path)),
                Some("missing-folder"),
            )
            .is_err());

        let after = lib.get_asset(asset_id).expect("get").expect("asset");
        assert_eq!(after.id, before.id);
        assert_eq!(after.folder_id, before.folder_id);
        assert_eq!(after.updated_at, before.updated_at);
        assert_eq!(lib.list_assets().expect("list").len(), 1);
    }

    #[test]
    fn stale_probe_candidate_cannot_publish_after_source_replacement() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("stale.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let candidate = probe_candidate(&media_path, lightweight_video_info(&media_path));
        std::fs::write(&media_path, [1u8, 2u8]).expect("replace source");

        assert!(lib.commit_media_probe(candidate, None).is_err());
        assert!(lib.list_assets().expect("list").is_empty());
    }

    #[test]
    fn still_image_extension_has_first_class_asset_identity() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("reference.png");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let mut info = lightweight_video_info(&media_path);
        info.container = "png_pipe".to_owned();
        info.video_streams[0].codec = VideoCodec::Other("png".to_owned());
        info.video_streams[0].total_frames = Some(1);

        let asset_id = commit_info(&lib, &media_path, info);

        let record = lib.get_asset(asset_id).expect("get").expect("asset exists");
        assert_eq!(record.kind, AssetKind::StillImage);
        assert!(record.media_probe().expect("file probe").has_video);
        assert!(!record.media_probe().expect("file probe").has_audio);
    }

    #[test]
    fn known_multiframe_image_remains_time_varying_video() {
        let media_path = Path::new("animated.gif");
        let mut info = lightweight_video_info(media_path);
        info.video_streams[0].total_frames = Some(12);

        assert_eq!(
            detect_asset_kind(&info, media_path).expect("classify animated image"),
            AssetKind::Video
        );
    }

    #[test]
    fn unproven_image_frame_count_fails_closed_as_time_varying_video() {
        let media_path = Path::new("unproven.webp");
        let mut info = lightweight_video_info(media_path);
        info.video_streams[0].total_frames = None;

        assert_eq!(
            detect_asset_kind(&info, media_path).expect("classify unproven image"),
            AssetKind::Video
        );
    }

    #[test]
    fn import_persists_default_stream_as_stable_primary_component() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("dual-audio.mov");
        std::fs::write(&media_path, [0u8]).expect("media file");

        let asset_id = commit_info(&lib, &media_path, lightweight_audio_info(&media_path));
        let record = lib.get_asset(asset_id).expect("get").expect("asset exists");
        let primary = record
            .audio_components
            .resolve(
                AudioSourceComponentId::primary(),
                record.media_probe().expect("probe"),
            )
            .expect("primary binding");

        assert_eq!(primary.index, 3);
        assert_eq!(record.audio_components.components.len(), 2);
    }

    #[test]
    fn media_upsert_preserves_asset_side_state_and_component_identities() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("dual-audio.mov");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let original_id = commit_info(&lib, &media_path, lightweight_audio_info(&media_path));
        let folder_id = lib.create_folder("Dialogue", None).expect("folder");
        lib.move_asset_to_folder(original_id, Some(&folder_id)).expect("move");
        let interpretation = AssetMediaInterpretation {
            color: MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
            ..AssetMediaInterpretation::default()
        };
        lib.set_asset_interpretation(original_id, interpretation)
            .expect("interpretation");
        let original = lib.get_asset(original_id).expect("get").expect("original");
        let original_component_ids = original
            .audio_components
            .components
            .iter()
            .map(|component| component.id)
            .collect::<Vec<_>>();

        let repeated_id = lib
            .commit_media_probe(
                probe_candidate(&media_path, lightweight_audio_info(&media_path)),
                Some(&folder_id),
            )
            .expect("repeat upsert");
        let repeated = lib.get_asset(repeated_id).expect("get").expect("repeated");

        assert_eq!(repeated_id, original_id);
        assert_eq!(repeated.folder_id.as_deref(), Some(folder_id.as_str()));
        assert_eq!(repeated.interpretation, interpretation);
        assert_eq!(
            repeated
                .audio_components
                .components
                .iter()
                .map(|component| component.id)
                .collect::<Vec<_>>(),
            original_component_ids
        );
    }

    #[test]
    fn importing_a_retired_path_restores_the_same_strong_asset_identity() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("restored.mov");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let original_id = commit_info(&lib, &media_path, lightweight_audio_info(&media_path));
        lib.retire_assets(&[original_id]).expect("retire");
        assert!(lib.list_assets().expect("hidden list").is_empty());

        let restored_id = lib
            .commit_media_probe(
                probe_candidate(&media_path, lightweight_audio_info(&media_path)),
                None,
            )
            .expect("restore by import");

        assert_eq!(restored_id, original_id);
        assert_eq!(
            lib.get_asset(restored_id).expect("query").expect("record").membership,
            AssetLibraryMembership::Visible
        );
        assert_eq!(lib.list_assets().expect("visible list").len(), 1);
    }

    #[test]
    fn component_refresh_discovers_new_stream_without_retargeting_missing_identity() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("refresh-audio.mov");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let asset_id = commit_info(&lib, &media_path, lightweight_audio_info(&media_path));
        let original = lib.get_asset(asset_id).expect("get").expect("asset");
        let missing_component = original
            .audio_components
            .components
            .iter()
            .find(|component| component.binding.stream_index == 1)
            .expect("original stream one")
            .id;
        let original_path = original.file_path().expect("file path");
        let mut replacement = lightweight_audio_info(original_path);
        replacement.audio_streams[0].index = 7;
        replacement.audio_streams[0].stream_id = Some(70);
        let fingerprint = MediaFileFingerprint::capture(original_path);

        lib.commit_audio_component_probe(
            asset_id,
            AssetMediaProbeCandidate::new(original_path.to_path_buf(), fingerprint, replacement)
                .expect("replacement candidate"),
            None,
        )
        .expect("refresh Components");

        let refreshed = lib.get_asset(asset_id).expect("get").expect("asset");
        assert!(matches!(
            refreshed
                .audio_components
                .resolve(missing_component, refreshed.media_probe().expect("probe")),
            Err(crate::AudioComponentCatalogError::MissingStream { stream_index: 1, .. })
        ));
        assert!(
            refreshed.audio_components.components.iter().any(|component| component
                .binding
                .stream_index
                == 7
                && component.id != missing_component)
        );
        assert_eq!(refreshed.audio_components.source_fingerprint, fingerprint);
    }

    #[test]
    fn explicit_component_rebind_is_committed_with_current_probe_evidence() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("rebind-audio.mov");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let asset_id = commit_info(&lib, &media_path, lightweight_audio_info(&media_path));
        let original = lib.get_asset(asset_id).expect("get").expect("asset");
        let original_path = original.file_path().expect("file path");
        let mut replacement = lightweight_audio_info(original_path);
        replacement.audio_streams[0].index = 7;
        replacement.audio_streams[0].stream_id = Some(70);
        replacement.audio_streams[1].stream_id = Some(31);
        let fingerprint = MediaFileFingerprint::capture(original_path);

        lib.commit_audio_component_probe(
            asset_id,
            AssetMediaProbeCandidate::new(original_path.to_path_buf(), fingerprint, replacement)
                .expect("replacement candidate"),
            Some((AudioSourceComponentId::primary(), 7)),
        )
        .expect("rebind Component");

        let rebound = lib.get_asset(asset_id).expect("get").expect("asset");
        assert_eq!(
            rebound
                .audio_components
                .resolve(
                    AudioSourceComponentId::primary(),
                    rebound.media_probe().expect("probe")
                )
                .expect("primary binding")
                .index,
            7
        );
        assert_eq!(rebound.audio_components.source_fingerprint, fingerprint);
    }

    #[test]
    fn create_adjustment_layer() {
        let lib = open_test_library();
        let id = lib
            .create_adjustment_layer_asset(Some("Test Adjustment"))
            .expect("create adjustment");
        let record = lib.get_asset(id).expect("get").expect("exists");
        assert_eq!(record.kind, AssetKind::AdjustmentLayer);
        assert_eq!(record.name, "Test Adjustment");
        assert_eq!(record.interpretation, AssetMediaInterpretation::default());
        assert_eq!(
            record.source,
            AssetSource::Generated(mondrian_core::GeneratedAssetKind::AdjustmentLayer)
        );
        assert!(record.media_probe().is_none());
        assert!(record.source_fingerprint().is_none());
    }

    #[test]
    fn create_adjustment_layer_auto_name() {
        let lib = open_test_library();
        let id = lib.create_adjustment_layer_asset(None).expect("create");
        let record = lib.get_asset(id).expect("get").expect("exists");
        assert!(record.name.contains("调整图层"));
        assert_eq!(record.kind, AssetKind::AdjustmentLayer);
    }

    #[test]
    fn create_multiple_adjustment_layers_increment_names() {
        let lib = open_test_library();
        let id1 = lib.create_adjustment_layer_asset(None).expect("create 1");
        let id2 = lib.create_adjustment_layer_asset(None).expect("create 2");
        let r1 = lib.get_asset(id1).expect("get1").expect("exists1");
        let r2 = lib.get_asset(id2).expect("get2").expect("exists2");
        assert_ne!(r1.name, r2.name);
    }

    #[test]
    fn create_solid_color() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Red Background")).expect("create");
        let record = lib.get_asset(id).expect("get").expect("exists");
        assert_eq!(record.kind, AssetKind::SolidColor);
        assert_eq!(record.name, "Red Background");
        assert_eq!(record.interpretation, AssetMediaInterpretation::default());
        assert_eq!(
            record.source,
            AssetSource::Generated(mondrian_core::GeneratedAssetKind::SolidColor)
        );
        assert!(record.media_probe().is_none());
        assert!(record.source_fingerprint().is_none());
    }

    #[test]
    fn set_asset_interpretation_persists_user_override() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Plate")).expect("create");
        let interpretation = AssetMediaInterpretation {
            color: MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
            range: mondrian_core::timeline_data::MediaRangeInterpretation::Override {
                range: mondrian_core::timeline_data::MediaSignalRange::Full,
            },
            ..AssetMediaInterpretation::default()
        };

        lib.set_asset_interpretation(id, interpretation).expect("set interpretation");

        let record = lib.get_asset(id).expect("get").expect("exists");
        assert_eq!(record.interpretation, interpretation);
        assert_eq!(
            record.interpretation.color.override_color_space(),
            Some(ColorSpace::Rec2100Pq)
        );
        assert_eq!(
            record.interpretation.range.override_range(),
            Some(mondrian_core::timeline_data::MediaSignalRange::Full)
        );
    }

    #[test]
    fn reset_asset_interpretation_returns_to_auto() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Plate")).expect("create");
        lib.set_asset_interpretation(
            id,
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override {
                    color_space: ColorSpace::SonySLog3SGamut3Cine,
                },
                ..AssetMediaInterpretation::default()
            },
        )
        .expect("set interpretation");

        lib.reset_asset_interpretation(id).expect("reset interpretation");

        let record = lib.get_asset(id).expect("get").expect("exists");
        assert_eq!(record.interpretation.color, MediaColorInterpretation::Auto);
    }

    #[test]
    fn set_asset_interpretation_rejects_missing_asset() {
        let lib = open_test_library();
        let err = lib
            .set_asset_interpretation(AssetId::new(), AssetMediaInterpretation::default())
            .unwrap_err();

        assert!(matches!(err, MondrianError::AssetNotFound { .. }));
    }

    #[test]
    fn create_solid_color_auto_name() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(None).expect("create");
        let record = lib.get_asset(id).expect("get").expect("exists");
        assert!(record.name.contains("纯色层"));
        assert_eq!(record.kind, AssetKind::SolidColor);
    }

    #[test]
    fn list_assets_returns_all() {
        let lib = open_test_library();
        lib.create_adjustment_layer_asset(Some("Adj1")).expect("create 1");
        lib.create_solid_color_asset(Some("Solid1")).expect("create 2");
        let assets = lib.list_assets().expect("list");
        assert_eq!(assets.len(), 2);
    }

    #[test]
    fn get_nonexistent_asset() {
        let lib = open_test_library();
        let result = lib.get_asset(AssetId::new()).expect("get");
        assert!(result.is_none());
    }

    #[test]
    fn rename_asset() {
        let lib = open_test_library();
        let id = lib.create_adjustment_layer_asset(Some("Original")).expect("create");
        lib.rename_asset(id, "Renamed").expect("rename");
        let record = lib.get_asset(id).expect("get").expect("exists");
        assert_eq!(record.name, "Renamed");
    }

    #[test]
    fn rename_asset_empty_name_fails() {
        let lib = open_test_library();
        let id = lib.create_adjustment_layer_asset(Some("X")).expect("create");
        let err = lib.rename_asset(id, "   ").unwrap_err();
        assert!(err.to_string().contains("不能为空"));
    }

    #[test]
    fn rename_nonexistent_asset_fails() {
        let lib = open_test_library();
        let err = lib.rename_asset(AssetId::new(), "X").unwrap_err();
        assert!(matches!(err, MondrianError::AssetNotFound { .. }));
    }

    #[test]
    fn move_asset_to_folder_updates_folder_id() {
        let lib = open_test_library();
        let folder_id = lib.create_folder("Bin", None).expect("create folder");
        let id = lib.create_solid_color_asset(Some("Plate")).expect("create asset");

        lib.move_asset_to_folder(id, Some(&folder_id)).expect("move into folder");
        assert_eq!(
            lib.get_asset(id).expect("get").expect("asset").folder_id.as_deref(),
            Some(folder_id.as_str())
        );
        lib.move_asset_to_folder(id, None).expect("move to root");
        assert_eq!(
            lib.get_asset(id).expect("get").expect("asset").folder_id,
            None
        );
    }

    #[test]
    fn move_asset_rejects_missing_asset_or_folder() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Plate")).expect("create asset");

        assert!(matches!(
            lib.move_asset_to_folder(AssetId::new(), None).unwrap_err(),
            MondrianError::AssetNotFound { .. }
        ));
        assert!(matches!(
            lib.move_asset_to_folder(id, Some("missing-folder")).unwrap_err(),
            MondrianError::AssetDbError { .. }
        ));
    }

    #[test]
    fn batch_move_deduplicates_and_commits_assets_and_folders_together() {
        let lib = open_test_library();
        let target = lib.create_folder("Target", None).expect("target");
        let first_folder = lib.create_folder("First", None).expect("first folder");
        let second_folder = lib.create_folder("Second", None).expect("second folder");
        let first_asset = lib.create_solid_color_asset(Some("First")).expect("first asset");
        let second_asset = lib.create_solid_color_asset(Some("Second")).expect("second asset");

        let outcome = lib
            .move_assets_and_folders(
                &[first_asset, second_asset, first_asset],
                &[
                    first_folder.clone(),
                    second_folder.clone(),
                    first_folder.clone(),
                ],
                Some(&target),
            )
            .expect("atomic batch move");

        assert_eq!(
            outcome,
            AssetLibraryMoveOutcome { moved_assets: 2, moved_folders: 2 }
        );
        assert_eq!(
            lib.get_asset(first_asset).expect("first").expect("record").folder_id.as_deref(),
            Some(target.as_str())
        );
        assert_eq!(
            lib.get_asset(second_asset)
                .expect("second")
                .expect("record")
                .folder_id
                .as_deref(),
            Some(target.as_str())
        );
        let folders = lib.list_folders().expect("folders");
        for folder_id in [first_folder, second_folder] {
            assert_eq!(
                folders
                    .iter()
                    .find(|folder| folder.id == folder_id)
                    .expect("moved folder")
                    .parent_id
                    .as_deref(),
                Some(target.as_str())
            );
        }
    }

    #[test]
    fn batch_move_rolls_back_every_row_when_sqlite_aborts_mid_transaction() {
        let lib = open_test_library();
        let target = lib.create_folder("Target", None).expect("target");
        let first = lib.create_solid_color_asset(Some("First")).expect("first");
        let second = lib.create_solid_color_asset(Some("Second")).expect("second");
        lib.db
            .lock()
            .execute_batch(&format!(
                "CREATE TRIGGER force_batch_move_abort \
                 BEFORE UPDATE OF folder_id ON assets \
                 WHEN OLD.id = '{}' \
                 BEGIN SELECT RAISE(ABORT, 'forced batch move failure'); END;",
                second.0
            ))
            .expect("install failure trigger");

        let error = lib
            .move_assets_and_folders(&[first, second], &[], Some(&target))
            .expect_err("trigger must abort the transaction");

        assert!(matches!(error, MondrianError::AssetDbError { .. }));
        assert_eq!(
            lib.get_asset(first).expect("first").expect("record").folder_id,
            None
        );
        assert_eq!(
            lib.get_asset(second).expect("second").expect("record").folder_id,
            None
        );
    }

    #[test]
    fn batch_move_rejects_cycles_before_changing_asset_membership() {
        let lib = open_test_library();
        let parent = lib.create_folder("Parent", None).expect("parent");
        let child = lib.create_folder("Child", Some(&parent)).expect("child");
        let asset = lib.create_solid_color_asset(Some("Plate")).expect("asset");

        let error = lib
            .move_assets_and_folders(&[asset], &[parent], Some(&child))
            .expect_err("folder cycle must fail the complete request");

        assert!(matches!(error, MondrianError::AssetDbError { .. }));
        assert_eq!(
            lib.get_asset(asset).expect("asset").expect("record").folder_id,
            None
        );
    }

    #[test]
    fn retiring_asset_hides_membership_but_preserves_the_strong_record() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Retire Me")).expect("create");

        assert_eq!(lib.retire_assets(&[id]).expect("retire"), 1);

        assert!(lib.list_assets().expect("visible list").is_empty());
        let record = lib.get_asset(id).expect("resolve").expect("strong record");
        assert!(record.membership.is_retired());
        assert_eq!(record.name, "Retire Me");
    }

    #[test]
    fn retiring_missing_or_already_retired_asset_fails() {
        let lib = open_test_library();
        let err = lib.retire_assets(&[AssetId::new()]).unwrap_err();
        assert!(matches!(err, MondrianError::AssetNotFound { .. }));

        let id = lib.create_solid_color_asset(Some("Once")).expect("create");
        lib.retire_assets(&[id]).expect("first retirement");
        let err = lib.retire_assets(&[id]).unwrap_err();
        assert!(matches!(err, MondrianError::AssetNotFound { .. }));
    }

    #[test]
    fn retirement_batch_preflight_prevents_partial_membership_change() {
        let lib = open_test_library();
        let first = lib.create_solid_color_asset(Some("First")).expect("first");
        let second = lib.create_solid_color_asset(Some("Second")).expect("second");

        let error = lib.retire_assets(&[first, AssetId::new(), second]).unwrap_err();

        assert!(matches!(error, MondrianError::AssetNotFound { .. }));
        assert_eq!(lib.list_assets().expect("all remain visible").len(), 2);
        assert_eq!(
            lib.get_asset(first).expect("first").expect("record").membership,
            AssetLibraryMembership::Visible
        );
        assert_eq!(
            lib.get_asset(second).expect("second").expect("record").membership,
            AssetLibraryMembership::Visible
        );
    }

    #[test]
    fn removal_batch_preflights_folders_before_retiring_any_asset() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Plate")).expect("asset");

        let error = lib
            .retire_assets_and_delete_folders(&[id], &["missing-folder".to_owned()])
            .unwrap_err();

        assert!(matches!(error, MondrianError::AssetDbError { .. }));
        assert_eq!(
            lib.get_asset(id).expect("asset").expect("record").membership,
            AssetLibraryMembership::Visible
        );
        assert_eq!(lib.list_assets().expect("visible").len(), 1);
    }

    #[test]
    fn create_and_list_folder() {
        let lib = open_test_library();
        let folder_id = lib.create_folder("My Folder", None).expect("create folder");
        assert!(!folder_id.is_empty());
        let folders = lib.list_folders().expect("list folders");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "My Folder");
    }

    #[test]
    fn create_nested_folder() {
        let lib = open_test_library();
        let parent = lib.create_folder("Parent", None).expect("create parent");
        let child = lib.create_folder("Child", Some(&parent)).expect("create child");
        let folders = lib.list_folders().expect("list");
        assert_eq!(folders.len(), 2);
        assert_eq!(
            folders.iter().find(|f| f.id == child).unwrap().parent_id.as_deref(),
            Some(parent.as_str())
        );
    }

    #[test]
    fn folder_exists_reports_real_folder_ids_only() {
        let lib = open_test_library();
        let folder_id = lib.create_folder("Bin", None).expect("create folder");

        assert!(lib.folder_exists(&folder_id).expect("existing folder"));
        assert!(!lib.folder_exists("missing-folder").expect("missing folder"));
    }

    #[test]
    fn rename_folder() {
        let lib = open_test_library();
        let id = lib.create_folder("Old", None).expect("create");
        lib.rename_folder(&id, "New").expect("rename");
        let folders = lib.list_folders().expect("list");
        assert_eq!(folders[0].name, "New");
    }

    #[test]
    fn rename_nonexistent_folder_fails() {
        let lib = open_test_library();
        let err = lib.rename_folder("missing-folder", "New").unwrap_err();
        assert!(matches!(err, MondrianError::AssetDbError { .. }));
    }

    #[test]
    fn move_folder_reparents_and_moves_back_to_root() {
        let lib = open_test_library();
        let parent = lib.create_folder("Parent", None).expect("create parent");
        let child = lib.create_folder("Child", None).expect("create child");

        lib.move_folder(&child, Some(&parent)).expect("move under parent");
        let folders = lib.list_folders().expect("list");
        assert_eq!(
            folders.iter().find(|folder| folder.id == child).unwrap().parent_id.as_deref(),
            Some(parent.as_str())
        );

        lib.move_folder(&child, None).expect("move to root");
        let folders = lib.list_folders().expect("list");
        assert_eq!(
            folders.iter().find(|folder| folder.id == child).unwrap().parent_id,
            None
        );
    }

    #[test]
    fn move_folder_rejects_missing_or_descendant_targets() {
        let lib = open_test_library();
        let parent = lib.create_folder("Parent", None).expect("create parent");
        let child = lib.create_folder("Child", Some(&parent)).expect("create child");

        assert!(matches!(
            lib.move_folder("missing-folder", None).unwrap_err(),
            MondrianError::AssetDbError { .. }
        ));
        assert!(matches!(
            lib.move_folder(&parent, Some("missing-folder")).unwrap_err(),
            MondrianError::AssetDbError { .. }
        ));
        assert!(matches!(
            lib.move_folder(&parent, Some(&child)).unwrap_err(),
            MondrianError::AssetDbError { .. }
        ));
    }

    #[test]
    fn delete_folder_unlinks_assets() {
        let lib = open_test_library();
        let folder_id = lib.create_folder("Bin", None).expect("create folder");
        let child_id = lib.create_folder("Child", Some(&folder_id)).expect("create child");
        let id = lib.create_adjustment_layer_asset(Some("In Bin")).expect("create asset");
        lib.move_asset_to_folder(id, Some(&child_id)).expect("move asset");

        lib.delete_folder(&folder_id).expect("delete folder");

        let asset = lib.get_asset(id).expect("get").expect("asset kept");
        assert_eq!(asset.folder_id, None);
        assert!(!lib.folder_exists(&folder_id).expect("parent deleted"));
        assert!(!lib.folder_exists(&child_id).expect("child deleted"));
    }

    #[test]
    fn delete_nonexistent_folder_fails() {
        let lib = open_test_library();
        let err = lib.delete_folder("missing-folder").unwrap_err();

        assert!(matches!(err, MondrianError::AssetDbError { .. }));
    }

    #[test]
    fn asset_kind_serde_roundtrip() {
        for kind in [
            AssetKind::Video,
            AssetKind::StillImage,
            AssetKind::Audio,
            AssetKind::AdjustmentLayer,
            AssetKind::SolidColor,
        ] {
            let json = serde_json::to_string(&kind).expect("serialize");
            let back: AssetKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn list_assets_in_nonexistent_folder_returns_empty() {
        let lib = open_test_library();
        let result = lib.list_assets_in_folder(Some("nonexistent")).expect("list");
        assert!(result.is_empty());
    }
}
