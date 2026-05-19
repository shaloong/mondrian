//! 素材库主入口

use crate::schema::{INIT_SQL, MIGRATE_FOLDERS_SQL};
use mondrian_core::{types::AssetId, MondrianError, Result};
use mondrian_media::MediaInfo;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Video,
    Audio,
    AdjustmentLayer,
}

impl AssetKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::AdjustmentLayer => "adjustment_layer",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "audio" => Self::Audio,
            "adjustment_layer" => Self::AdjustmentLayer,
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
    #[serde(default)]
    pub folder_id: Option<String>,
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

impl AssetLibrary {
    /// 打开或创建素材库（root 为库根目录）
    pub fn open(root: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&root)?;
        let db_path = root.join("index.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;
        conn.execute_batch(INIT_SQL)
            .map_err(|e| mondrian_core::MondrianError::AssetDbError { reason: e.to_string() })?;

        // Migrate existing databases that lack folder support.
        let _ = conn.execute_batch(MIGRATE_FOLDERS_SQL);

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

    /// 导入单个媒体文件（当前 v0.1 支持：可读视频/音频）
    pub fn import_media_file(&self, path: &Path) -> Result<AssetId> {
        let canonical_path = path.canonicalize().map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

        let info = MediaInfo::probe(&canonical_path)?;
        let kind = detect_asset_kind(&info, &canonical_path)?;

        let name = canonical_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("untitled")
            .to_string();

        let path_str = canonical_path.to_string_lossy().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let db = self.db.lock();

        let existing_id = db
            .query_row(
                "SELECT id FROM assets WHERE path = ?1",
                rusqlite::params![path_str],
                |row| row.get::<_, String>(0),
            )
            .ok();

        let id = match existing_id {
            Some(existing) => {
                let parsed = Uuid::parse_str(&existing).map_err(|e| {
                    MondrianError::AssetDbError { reason: format!("invalid asset id in db: {e}") }
                })?;
                AssetId(parsed)
            }
            None => AssetId::new(),
        };

        let metadata_json = serde_json::to_string(&info)?;
        db.execute(
            "INSERT OR REPLACE INTO assets \
             (id, name, asset_type, path, tags, metadata, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
                COALESCE((SELECT created_at FROM assets WHERE path = ?4), ?7), ?7)",
            rusqlite::params![
                id.0.to_string(),
                name,
                kind.as_str(),
                path_str,
                "[]",
                metadata_json,
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
        let db = self.db.lock();

        db.execute(
            "INSERT INTO assets \
             (id, name, asset_type, path, tags, metadata, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![
                asset_id.0.to_string(),
                asset_name,
                AssetKind::AdjustmentLayer.as_str(),
                synthetic_path.to_string_lossy().to_string(),
                "[]",
                metadata_json,
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
        let now = chrono::Utc::now().to_rfc3339();
        let metadata_json = serde_json::to_string(&info)?;
        let path_str = canonical_path.to_string_lossy().to_string();

        let db = self.db.lock();
        let existing_kind = db
            .query_row(
                "SELECT asset_type FROM assets WHERE id = ?1 LIMIT 1",
                rusqlite::params![asset_id.0.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let Some(existing_kind) = existing_kind else {
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

        let changed = db
            .execute(
                "UPDATE assets SET path = ?1, metadata = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![path_str, metadata_json, now, asset_id.0.to_string()],
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        if changed == 0 {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
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
                     FROM assets WHERE folder_id = ?1 ORDER BY updated_at DESC",
                )
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let rows = stmt
                .query_map(rusqlite::params![fid], parse_asset_row)
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let records: Vec<_> = rows.flatten().collect();
            Ok(records)
        } else {
            let mut stmt = db
                .prepare(
                    "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                     FROM assets ORDER BY updated_at DESC",
                )
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let rows = stmt
                .query_map([], parse_asset_row)
                .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
            let records: Vec<_> = rows.flatten().collect();
            Ok(records)
        }
    }

    pub fn get_asset(&self, asset_id: AssetId) -> Result<Option<AssetRecord>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, name, asset_type, path, folder_id, metadata, created_at, updated_at \
                 FROM assets WHERE id = ?1 LIMIT 1",
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let result = stmt
            .query_row(rusqlite::params![asset_id.0.to_string()], parse_asset_row)
            .optional()
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        Ok(result)
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

    pub fn create_folder(
        &self,
        name: &str,
        parent_id: Option<&str>,
    ) -> Result<String> {
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
            return Err(MondrianError::AssetDbError { reason: "文件夹名不能为空".to_string() });
        }
        let db = self.db.lock();
        db.execute(
            "UPDATE folders SET name = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![trimmed, chrono::Utc::now().to_rfc3339(), folder_id],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        Ok(())
    }

    pub fn delete_folder(&self, folder_id: &str) -> Result<()> {
        let db = self.db.lock();
        // Unlink assets from this folder before deleting.
        db.execute(
            "UPDATE assets SET folder_id = NULL WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        // Child folders cascade via ON DELETE CASCADE.
        db.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![folder_id])
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
            .filter_map(|r| r.ok())
            .collect();
        Ok(records)
    }

    pub fn move_asset_to_folder(
        &self,
        asset_id: AssetId,
        folder_id: Option<&str>,
    ) -> Result<()> {
        let db = self.db.lock();
        db.execute(
            "UPDATE assets SET folder_id = ?1 WHERE id = ?2",
            rusqlite::params![folder_id, asset_id.0.to_string()],
        )
        .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        Ok(())
    }

    /// Find which timeline clips reference this asset (position reverse lookup).
    /// Returns a list of (asset_id, asset_name) for the UI to select and navigate to.
    pub fn get_asset_location(&self, asset_id: AssetId) -> Result<Option<(Option<String>, Option<String>)>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare("SELECT folder_id, name FROM assets WHERE id = ?1")
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;
        let result = stmt
            .query_row(rusqlite::params![asset_id.0.to_string()], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?))
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

    let asset_id = Uuid::parse_str(&id_raw).map(AssetId).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let media_info = serde_json::from_str::<MediaInfo>(&metadata_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(AssetRecord {
        id: asset_id,
        name,
        kind: AssetKind::from_str(&kind_raw),
        path: PathBuf::from(path_raw),
        folder_id,
        media_info,
        created_at,
        updated_at,
    })
}

fn synthetic_adjustment_layer_path(asset_id: AssetId) -> PathBuf {
    PathBuf::from(format!("mondrian://adjustment-layer/{asset_id}"))
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

fn detect_asset_kind(info: &MediaInfo, path: &Path) -> Result<AssetKind> {
    let force_audio =
        info.has_audio && (is_audio_only_extension(path) || !has_meaningful_video_stream(info));
    if force_audio {
        return Ok(AssetKind::Audio);
    }
    if info.has_video {
        return Ok(AssetKind::Video);
    }
    if info.has_audio {
        return Ok(AssetKind::Audio);
    }
    Err(MondrianError::UnsupportedFormat { format: info.container.clone() })
}
