//! 素材库主入口

use crate::{audio_catalog::AssetAudioComponentCatalog, migration::migrate_asset_library};
use mondrian_core::{
    timeline_data::AssetMediaInterpretation,
    types::{AssetId, AssetSource},
    AudioSourceComponentId, MondrianError, Result,
};
use mondrian_media::{MediaFileFingerprint, MediaInfo};
use parking_lot::Mutex;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
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

    fn from_str(value: &str) -> Self {
        match value {
            "still_image" => Self::StillImage,
            "audio" => Self::Audio,
            "adjustment_layer" => Self::AdjustmentLayer,
            "solid_color" => Self::SolidColor,
            _ => Self::Video,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: AssetId,
    pub name: String,
    pub kind: AssetKind,
    pub path: PathBuf,
    /// Structured asset source. `None` in legacy project files — use `path`.
    #[serde(default)]
    pub source: Option<AssetSource>,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub interpretation: AssetMediaInterpretation,
    /// Stable logical audio Components and their conservative stream bindings.
    #[serde(default)]
    pub audio_components: AssetAudioComponentCatalog,
    pub media_info: MediaInfo,
    pub created_at: String,
    pub updated_at: String,
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

fn connection_revision(connection: &Connection) -> Result<u64> {
    let revision = connection
        .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
        .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
    u64::try_from(revision).map_err(|_| MondrianError::AssetDbError {
        reason: format!("SQLite returned a negative total_changes value: {revision}"),
    })
}

impl AssetLibrary {
    /// 打开或创建素材库（root 为库根目录）
    pub fn open(root: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&root)?;
        let db_path = root.join("index.db");
        let mut conn = Connection::open(&db_path)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;
        migrate_asset_library(&mut conn)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;

        info!("Asset library opened at {:?}", root);
        Ok(Arc::new(Self { root, db: Arc::new(Mutex::new(conn)) }))
    }

    /// 默认库路径：~/.mondrian/library
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".mondrian")
            .join("library")
    }

    /// Path of the live project-library database.
    ///
    /// Persistence callers must use [`Self::snapshot_database`] rather than
    /// copying this file while the connection is active.
    pub fn database_path(&self) -> PathBuf {
        self.root.join("index.db")
    }

    /// Create a transactionally consistent standalone SQLite snapshot.
    ///
    /// The live database may use WAL and remain open while persistence runs.
    /// Copying `index.db` directly is therefore forbidden: the SQLite online
    /// backup API is the sole archive snapshot boundary.
    /// Return the connection-local SQLite change revision used to bind an
    /// author snapshot to the exact asset-library state it observed.
    pub fn database_revision(&self) -> Result<u64> {
        connection_revision(&self.db.lock())
    }

    pub fn snapshot_database(&self, expected_revision: u64, destination: &Path) -> Result<()> {
        if destination == self.database_path() {
            return Err(MondrianError::AssetDbError {
                reason: "database snapshot destination aliases the live database".to_owned(),
            });
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if destination.exists() {
            std::fs::remove_file(destination)?;
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
        let mut target = Connection::open(destination)
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        let backup = rusqlite::backup::Backup::new(&source, &mut target)
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        backup
            .run_to_completion(128, std::time::Duration::from_millis(1), None)
            .map_err(|error| MondrianError::AssetDbError { reason: error.to_string() })?;
        drop(backup);
        drop(target);
        std::fs::OpenOptions::new().write(true).open(destination)?.sync_all()?;
        Ok(())
    }

    /// 导入单个媒体文件（当前 v0.1 支持：可读视频/音频）
    pub fn import_media_file(&self, path: &Path) -> Result<AssetId> {
        let canonical_path = path.canonicalize().map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

        let info = MediaInfo::probe(&canonical_path)?;
        self.upsert_media_file_with_info(&canonical_path, info)
    }

    /// Insert or update a media asset using metadata already produced by the caller.
    ///
    /// This keeps synchronous metadata probing out of callers that already have a
    /// bounded probe result, such as background import workers or performance
    /// harnesses that need to isolate decode/access-mode latency from metadata
    /// analysis latency.
    pub fn upsert_media_file_with_info(&self, path: &Path, mut info: MediaInfo) -> Result<AssetId> {
        let canonical_path = path.canonicalize().map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        info.path = canonical_path.clone();
        let kind = detect_asset_kind(&info, &canonical_path)?;
        let source_fingerprint = MediaFileFingerprint::capture(&canonical_path);

        let name = canonical_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("untitled")
            .to_string();

        let path_str = canonical_path.to_string_lossy().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let db = self.db.lock();

        let existing = db
            .query_row(
                "SELECT id, audio_components FROM assets WHERE path = ?1",
                rusqlite::params![&path_str],
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
        let audio_components_json = serde_json::to_string(&audio_components)?;
        let interpretation_json = serde_json::to_string(&AssetMediaInterpretation::default())?;
        db.execute(
            "INSERT INTO assets \
             (id, name, asset_type, path, tags, metadata, interpretation, audio_components, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) \
             ON CONFLICT(path) DO UPDATE SET \
                name = excluded.name, \
                asset_type = excluded.asset_type, \
                metadata = excluded.metadata, \
                audio_components = excluded.audio_components, \
                updated_at = excluded.updated_at",
            rusqlite::params![
                id.0.to_string(),
                name,
                kind.as_str(),
                path_str,
                "[]",
                metadata_json,
                interpretation_json,
                audio_components_json,
                now
            ],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

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
        let synthetic_path = synthetic_adjustment_layer_path(asset_id);
        let metadata_json = serde_json::to_string(&MediaInfo::synthetic_adjustment_layer())?;
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
                synthetic_path.to_string_lossy().to_string(),
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
        let synthetic_path = synthetic_solid_color_path(asset_id);
        let metadata_json = serde_json::to_string(&MediaInfo::synthetic_solid_color())?;
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
                synthetic_path.to_string_lossy().to_string(),
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

    pub fn relink_asset(&self, asset_id: AssetId, path: &Path) -> Result<()> {
        let canonical_path = path.canonicalize().map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

        let info = MediaInfo::probe(&canonical_path)?;
        let kind = detect_asset_kind(&info, &canonical_path)?;
        let source_fingerprint = MediaFileFingerprint::capture(&canonical_path);
        let now = chrono::Utc::now().to_rfc3339();
        let metadata_json = serde_json::to_string(&info)?;
        let path_str = canonical_path.to_string_lossy().to_string();

        let db = self.db.lock();
        let existing = db
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

        let existing_kind = AssetKind::from_str(existing_kind.as_str());
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

        let changed = db
            .execute(
                "UPDATE assets SET path = ?1, metadata = ?2, audio_components = ?3, \
                 updated_at = ?4 WHERE id = ?5",
                rusqlite::params![
                    path_str,
                    metadata_json,
                    audio_components_json,
                    now,
                    asset_id.0.to_string()
                ],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }

        Ok(())
    }

    /// Explicitly bind one stable Asset audio Component to a physical stream
    /// from the Asset's current file revision.
    pub fn rebind_audio_component(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        stream_index: u32,
    ) -> Result<()> {
        let asset = self
            .get_asset(asset_id)?
            .ok_or_else(|| MondrianError::AssetNotFound { asset_id: asset_id.to_string() })?;
        let fingerprint_before = MediaFileFingerprint::capture(&asset.path);
        let info = MediaInfo::probe(&asset.path)?;
        let fingerprint_after = MediaFileFingerprint::capture(&asset.path);
        if fingerprint_before != fingerprint_after {
            return Err(MondrianError::AssetDbError {
                reason: "audio source changed while probing Component rebind candidates".to_owned(),
            });
        }
        self.rebind_audio_component_with_info(
            asset_id,
            component_id,
            stream_index,
            &asset.path,
            info,
            fingerprint_after,
        )
    }

    /// Re-probe one Asset and conservatively refresh its audio Component catalog.
    ///
    /// Existing logical IDs and bindings are never retargeted. Newly discovered,
    /// unclaimed physical streams receive new IDs so a later explicit rebind can
    /// select them.
    pub fn refresh_audio_components(&self, asset_id: AssetId) -> Result<()> {
        let asset = self
            .get_asset(asset_id)?
            .ok_or_else(|| MondrianError::AssetNotFound { asset_id: asset_id.to_string() })?;
        let fingerprint_before = MediaFileFingerprint::capture(&asset.path);
        let info = MediaInfo::probe(&asset.path)?;
        let fingerprint_after = MediaFileFingerprint::capture(&asset.path);
        if fingerprint_before != fingerprint_after {
            return Err(MondrianError::AssetDbError {
                reason: "audio source changed while refreshing Component candidates".to_owned(),
            });
        }
        self.refresh_audio_components_with_info(asset_id, &asset.path, info, fingerprint_after)
    }

    fn refresh_audio_components_with_info(
        &self,
        asset_id: AssetId,
        expected_path: &Path,
        info: MediaInfo,
        source_fingerprint: MediaFileFingerprint,
    ) -> Result<()> {
        self.commit_audio_component_probe(asset_id, expected_path, info, source_fingerprint, None)
    }

    fn rebind_audio_component_with_info(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        stream_index: u32,
        expected_path: &Path,
        info: MediaInfo,
        source_fingerprint: MediaFileFingerprint,
    ) -> Result<()> {
        self.commit_audio_component_probe(
            asset_id,
            expected_path,
            info,
            source_fingerprint,
            Some((component_id, stream_index)),
        )
    }

    fn commit_audio_component_probe(
        &self,
        asset_id: AssetId,
        expected_path: &Path,
        mut info: MediaInfo,
        source_fingerprint: MediaFileFingerprint,
        rebind: Option<(AudioSourceComponentId, u32)>,
    ) -> Result<()> {
        let current_fingerprint = MediaFileFingerprint::capture(expected_path);
        if current_fingerprint != source_fingerprint {
            return Err(MondrianError::AssetDbError {
                reason: "audio source changed before Component catalog commit".to_owned(),
            });
        }
        info.path = expected_path.to_path_buf();
        let kind = detect_asset_kind(&info, expected_path)?;
        let metadata_json = serde_json::to_string(&info)?;
        let now = chrono::Utc::now().to_rfc3339();
        let db = self.db.lock();
        let existing = db
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
        if Path::new(&stored_path) != expected_path {
            return Err(MondrianError::AssetDbError {
                reason: "Asset path changed before audio Component catalog commit".to_owned(),
            });
        }
        if AssetKind::from_str(&stored_kind) != kind {
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
        let changed = db
            .execute(
                "UPDATE assets SET metadata = ?1, audio_components = ?2, updated_at = ?3 \
                 WHERE id = ?4 AND path = ?5",
                rusqlite::params![
                    metadata_json,
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
        Ok(())
    }

    pub fn list_assets(&self) -> Result<Vec<AssetRecord>> {
        self.list_assets_in_folder(None)
    }

    pub fn list_assets_in_folder(&self, folder_id: Option<&str>) -> Result<Vec<AssetRecord>> {
        let db = self.db.lock();
        if let Some(fid) = folder_id {
            let mut stmt = db
                .prepare(
                    "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                     , interpretation, audio_components \
                     FROM assets WHERE folder_id = ?1 ORDER BY updated_at DESC",
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
                     , interpretation, audio_components \
                     FROM assets ORDER BY updated_at DESC",
                )
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let rows = stmt
                .query_map([], parse_asset_row)
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })
        }
    }

    pub fn get_asset(&self, asset_id: AssetId) -> Result<Option<AssetRecord>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                 , interpretation, audio_components \
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

    pub fn delete_asset(&self, asset_id: AssetId) -> Result<()> {
        let db = self.db.lock();
        let changed = db
            .execute(
                "DELETE FROM assets WHERE id = ?1",
                rusqlite::params![asset_id.0.to_string()],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }

        Ok(())
    }

    pub fn clear_assets(&self) -> Result<()> {
        let db = self.db.lock();
        db.execute("DELETE FROM assets", [])
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        Ok(())
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
        let db = self.db.lock();
        let mut stmt = db
            .prepare("SELECT id, parent_id FROM folders")
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        let folders: Vec<(String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        drop(stmt);
        if !folders.iter().any(|(id, _)| id == folder_id) {
            return Err(MondrianError::AssetDbError {
                reason: format!("文件夹不存在：{folder_id}"),
            });
        }

        let mut folder_ids = vec![folder_id.to_string()];
        let mut index = 0usize;
        while index < folder_ids.len() {
            let parent_id = folder_ids[index].clone();
            for (id, parent) in &folders {
                if parent.as_deref() == Some(parent_id.as_str()) && !folder_ids.contains(id) {
                    folder_ids.push(id.clone());
                }
            }
            index += 1;
        }

        let placeholders = std::iter::repeat_n("?", folder_ids.len()).collect::<Vec<_>>().join(",");
        db.execute(
            &format!("UPDATE assets SET folder_id = NULL WHERE folder_id IN ({placeholders})"),
            params_from_iter(folder_ids.iter()),
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        db.execute(
            &format!("DELETE FROM folders WHERE id IN ({placeholders})"),
            params_from_iter(folder_ids.iter()),
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
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
        let db = self.db.lock();
        if let Some(folder_id) = folder_id {
            let exists = db
                .query_row(
                    "SELECT 1 FROM folders WHERE id = ?1 LIMIT 1",
                    rusqlite::params![folder_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?
                .is_some();
            if !exists {
                return Err(MondrianError::AssetDbError {
                    reason: format!("目标文件夹不存在：{folder_id}"),
                });
            }
        }
        let changed = db
            .execute(
                "UPDATE assets SET folder_id = ?1 WHERE id = ?2",
                rusqlite::params![folder_id, asset_id.0.to_string()],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }
        Ok(())
    }

    pub fn move_folder(&self, folder_id: &str, parent_folder_id: Option<&str>) -> Result<()> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare("SELECT id, parent_id FROM folders")
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        let folders: Vec<(String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        drop(stmt);

        if !folders.iter().any(|(id, _)| id == folder_id) {
            return Err(MondrianError::AssetDbError {
                reason: format!("文件夹不存在：{folder_id}"),
            });
        }
        if let Some(parent_id) = parent_folder_id {
            if parent_id == folder_id {
                return Err(MondrianError::AssetDbError {
                    reason: "不能将文件夹移动到自身".to_string(),
                });
            }
            if !folders.iter().any(|(id, _)| id == parent_id) {
                return Err(MondrianError::AssetDbError {
                    reason: format!("目标文件夹不存在：{parent_id}"),
                });
            }

            let mut descendants = vec![folder_id.to_string()];
            let mut index = 0usize;
            while index < descendants.len() {
                let current = descendants[index].clone();
                for (id, parent) in &folders {
                    if parent.as_deref() == Some(current.as_str()) && !descendants.contains(id) {
                        descendants.push(id.clone());
                    }
                }
                index += 1;
            }
            if descendants.iter().any(|id| id == parent_id) {
                return Err(MondrianError::AssetDbError {
                    reason: "不能将文件夹移动到自身的子文件夹".to_string(),
                });
            }
        }

        let changed = db
            .execute(
                "UPDATE folders SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![parent_folder_id, chrono::Utc::now().to_rfc3339(), folder_id],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        if changed == 0 {
            return Err(MondrianError::AssetDbError {
                reason: format!("文件夹不存在：{folder_id}"),
            });
        }
        Ok(())
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

    let asset_id = Uuid::parse_str(&id_raw).map(AssetId).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let media_info = serde_json::from_str::<MediaInfo>(&metadata_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
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

    Ok(AssetRecord {
        id: asset_id,
        name,
        kind: AssetKind::from_str(&kind_raw),
        path: PathBuf::from(path_raw),
        source: None, // legacy DB records; set for new assets only
        folder_id,
        interpretation,
        audio_components,
        media_info,
        created_at,
        updated_at,
    })
}

fn synthetic_adjustment_layer_path(asset_id: AssetId) -> PathBuf {
    PathBuf::from(format!("mondrian://adjustment-layer/{asset_id}"))
}

fn synthetic_solid_color_path(asset_id: AssetId) -> PathBuf {
    PathBuf::from(format!("mondrian://solid-color/{asset_id}"))
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
    if info.has_video
        && mondrian_media::is_picture_file_extension(path)
        && is_proven_single_picture(info)
    {
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
    use mondrian_media::{
        info::{AudioCodec, ChannelLayout, PixelFormat, VideoCodec},
        AudioStreamInfo, DetectedColorInterpretation, VideoColorDetectionMethod,
        VideoColorInterpretationConfidence, VideoColorInterpretationWarning, VideoColorSpaceSource,
        VideoStreamInfo,
    };
    use std::time::Duration;

    fn open_test_library() -> Arc<AssetLibrary> {
        let dir = tempfile::tempdir().expect("tempdir");
        AssetLibrary::open(dir.keep()).expect("open library")
    }

    fn lightweight_video_info(path: &Path) -> MediaInfo {
        MediaInfo {
            path: path.to_path_buf(),
            duration: Duration::from_secs(1),
            file_size: 1,
            container: "mp4".to_string(),
            video_streams: vec![VideoStreamInfo {
                index: 0,
                codec: VideoCodec::H264,
                duration: Some(Duration::from_secs(1)),
                codec_profile: mondrian_media::VideoCodecProfile::Unknown,
                width: 1920,
                height: 1080,
                frame_rate: mondrian_core::types::Rational::FPS_30,
                frame_rate_proven: true,
                pixel_format: PixelFormat::Yuv420p,
                pixel_format_proven: true,
                color_range: mondrian_media::DecodedVideoRange::Unknown,
                detected_color_space: None,
                color_interpretation: DetectedColorInterpretation {
                    color_space: None,
                    confidence: VideoColorInterpretationConfidence::None,
                    source: VideoColorSpaceSource::MissingMetadata,
                    method: VideoColorDetectionMethod::MissingMetadata,
                    evidence: Vec::new(),
                    warnings: vec![VideoColorInterpretationWarning::MissingCicpTags],
                    user_overridable: true,
                },
                color_space_source: VideoColorSpaceSource::MissingMetadata,
                color_detection_method: VideoColorDetectionMethod::MissingMetadata,
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

    fn lightweight_audio_info(path: &Path) -> MediaInfo {
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
            path: path.to_path_buf(),
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

        lib.snapshot_database(revision, &snapshot_path).expect("online snapshot");

        let snapshot = Connection::open(snapshot_path).expect("open snapshot");
        let stored_name: String = snapshot
            .query_row(
                "SELECT name FROM assets WHERE id = ?1",
                [asset_id.to_string()],
                |row| row.get(0),
            )
            .expect("snapshotted asset");
        assert_eq!(stored_name, "Snapshot red");
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
    fn open_and_list_empty() {
        let lib = open_test_library();
        let assets = lib.list_assets().expect("list_assets");
        assert!(assets.is_empty());
    }

    #[test]
    fn upsert_media_file_with_info_registers_preprobed_video() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("preprobed.mp4");
        std::fs::write(&media_path, [0u8]).expect("media file");

        let asset_id = lib
            .upsert_media_file_with_info(&media_path, lightweight_video_info(&media_path))
            .expect("upsert preprobed media");

        let record = lib.get_asset(asset_id).expect("get").expect("asset exists");
        assert_eq!(record.kind, AssetKind::Video);
        assert_eq!(record.name, "preprobed.mp4");
        assert_eq!(
            record.path,
            media_path.canonicalize().expect("canonical path")
        );
        assert_eq!(record.media_info.path, record.path);
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

        let asset_id = lib
            .upsert_media_file_with_info(&media_path, info)
            .expect("upsert preprobed still");

        let record = lib.get_asset(asset_id).expect("get").expect("asset exists");
        assert_eq!(record.kind, AssetKind::StillImage);
        assert!(record.media_info.has_video);
        assert!(!record.media_info.has_audio);
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

        let asset_id = lib
            .upsert_media_file_with_info(&media_path, lightweight_audio_info(&media_path))
            .expect("upsert preprobed media");
        let record = lib.get_asset(asset_id).expect("get").expect("asset exists");
        let primary = record
            .audio_components
            .resolve(AudioSourceComponentId::primary(), &record.media_info)
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
        let original_id = lib
            .upsert_media_file_with_info(&media_path, lightweight_audio_info(&media_path))
            .expect("initial upsert");
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
            .upsert_media_file_with_info(&media_path, lightweight_audio_info(&media_path))
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
    fn component_refresh_discovers_new_stream_without_retargeting_missing_identity() {
        let lib = open_test_library();
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().join("refresh-audio.mov");
        std::fs::write(&media_path, [0u8]).expect("media file");
        let asset_id = lib
            .upsert_media_file_with_info(&media_path, lightweight_audio_info(&media_path))
            .expect("initial upsert");
        let original = lib.get_asset(asset_id).expect("get").expect("asset");
        let missing_component = original
            .audio_components
            .components
            .iter()
            .find(|component| component.binding.stream_index == 1)
            .expect("original stream one")
            .id;
        let mut replacement = lightweight_audio_info(&original.path);
        replacement.audio_streams[0].index = 7;
        replacement.audio_streams[0].stream_id = Some(70);
        let fingerprint = MediaFileFingerprint::capture(&original.path);

        lib.refresh_audio_components_with_info(asset_id, &original.path, replacement, fingerprint)
            .expect("refresh Components");

        let refreshed = lib.get_asset(asset_id).expect("get").expect("asset");
        assert!(matches!(
            refreshed.audio_components.resolve(missing_component, &refreshed.media_info),
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
        let asset_id = lib
            .upsert_media_file_with_info(&media_path, lightweight_audio_info(&media_path))
            .expect("initial upsert");
        let original = lib.get_asset(asset_id).expect("get").expect("asset");
        let mut replacement = lightweight_audio_info(&original.path);
        replacement.audio_streams[0].index = 7;
        replacement.audio_streams[0].stream_id = Some(70);
        replacement.audio_streams[1].stream_id = Some(31);
        let fingerprint = MediaFileFingerprint::capture(&original.path);

        lib.rebind_audio_component_with_info(
            asset_id,
            AudioSourceComponentId::primary(),
            7,
            &original.path,
            replacement,
            fingerprint,
        )
        .expect("rebind Component");

        let rebound = lib.get_asset(asset_id).expect("get").expect("asset");
        assert_eq!(
            rebound
                .audio_components
                .resolve(AudioSourceComponentId::primary(), &rebound.media_info)
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
        assert!(record.path.to_string_lossy().starts_with("mondrian://adjustment-layer/"));
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
        assert!(record.path.to_string_lossy().starts_with("mondrian://solid-color/"));
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
    fn delete_asset() {
        let lib = open_test_library();
        let id = lib.create_solid_color_asset(Some("Delete Me")).expect("create");
        lib.delete_asset(id).expect("delete");
        assert!(lib.get_asset(id).expect("get").is_none());
    }

    #[test]
    fn delete_nonexistent_asset_fails() {
        let lib = open_test_library();
        let err = lib.delete_asset(AssetId::new()).unwrap_err();
        assert!(matches!(err, MondrianError::AssetNotFound { .. }));
    }

    #[test]
    fn clear_assets_removes_all() {
        let lib = open_test_library();
        lib.create_adjustment_layer_asset(None).expect("create 1");
        lib.create_solid_color_asset(None).expect("create 2");
        lib.clear_assets().expect("clear");
        assert!(lib.list_assets().expect("list").is_empty());
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
