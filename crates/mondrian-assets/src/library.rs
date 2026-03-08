//! 素材库主入口

use crate::schema::INIT_SQL;
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
}

impl AssetKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "audio" => Self::Audio,
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
    pub media_info: MediaInfo,
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
        let force_audio = info.has_audio
            && (is_audio_only_extension(&canonical_path) || !has_meaningful_video_stream(&info));
        let kind = if force_audio {
            AssetKind::Audio
        } else if info.has_video {
            AssetKind::Video
        } else if info.has_audio {
            AssetKind::Audio
        } else {
            return Err(MondrianError::UnsupportedFormat { format: info.container.clone() });
        };

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

    pub fn list_assets(&self) -> Result<Vec<AssetRecord>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, name, asset_type, path, metadata, created_at, updated_at \
                 FROM assets ORDER BY updated_at DESC",
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let records = stmt
            .query_map([], |row| {
                let id_raw: String = row.get(0)?;
                let name: String = row.get(1)?;
                let kind_raw: String = row.get(2)?;
                let path_raw: String = row.get(3)?;
                let metadata_raw: String = row.get(4)?;
                let created_at: String = row.get(5)?;
                let updated_at: String = row.get(6)?;

                let asset_id = Uuid::parse_str(&id_raw).map(AssetId).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                let media_info = serde_json::from_str::<MediaInfo>(&metadata_raw).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                Ok(AssetRecord {
                    id: asset_id,
                    name,
                    kind: AssetKind::from_str(&kind_raw),
                    path: PathBuf::from(path_raw),
                    media_info,
                    created_at,
                    updated_at,
                })
            })
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?
            .filter_map(std::result::Result::ok)
            .collect();

        Ok(records)
    }

    pub fn get_asset(&self, asset_id: AssetId) -> Result<Option<AssetRecord>> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, name, asset_type, path, metadata, created_at, updated_at \
                 FROM assets WHERE id = ?1 LIMIT 1",
            )
            .map_err(|e| MondrianError::AssetDbError { reason: e.to_string() })?;

        let result = stmt
            .query_row(rusqlite::params![asset_id.0.to_string()], |row| {
                let id_raw: String = row.get(0)?;
                let name: String = row.get(1)?;
                let kind_raw: String = row.get(2)?;
                let path_raw: String = row.get(3)?;
                let metadata_raw: String = row.get(4)?;
                let created_at: String = row.get(5)?;
                let updated_at: String = row.get(6)?;

                let parsed_id = Uuid::parse_str(&id_raw).map(AssetId).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                let media_info = serde_json::from_str::<MediaInfo>(&metadata_raw).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                Ok(AssetRecord {
                    id: parsed_id,
                    name,
                    kind: AssetKind::from_str(&kind_raw),
                    path: PathBuf::from(path_raw),
                    media_info,
                    created_at,
                    updated_at,
                })
            })
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

        let now = chrono::Utc::now().to_rfc3339();
        let db = self.db.lock();
        let changed = db
            .execute(
                "UPDATE assets SET name = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![trimmed, now, asset_id.0.to_string()],
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
