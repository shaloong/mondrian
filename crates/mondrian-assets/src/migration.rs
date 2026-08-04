use crate::{
    audio_catalog::AssetAudioComponentCatalog,
    schema::{CREATE_FOLDERS_SQL, INIT_SQL},
};
use anyhow::Context;
use mondrian_core::{AudioChannelLayout, MediaFileFingerprint, MediaInfo};
use rusqlite::{Connection, Transaction};
/// Current asset-library SQLite schema version stored in `PRAGMA user_version`.
pub const ASSET_LIBRARY_SCHEMA_VERSION: u32 = 5;

type SqliteMigrationFn = fn(&Transaction<'_>) -> anyhow::Result<()>;

struct SqliteMigrationStep {
    from: u32,
    to: u32,
    migrate: SqliteMigrationFn,
}

const MIGRATIONS: &[SqliteMigrationStep] = &[
    SqliteMigrationStep { from: 0, to: 1, migrate: migrate_zero_to_one },
    SqliteMigrationStep { from: 1, to: 2, migrate: migrate_one_to_two },
    SqliteMigrationStep { from: 2, to: 3, migrate: migrate_two_to_three },
    SqliteMigrationStep { from: 3, to: 4, migrate: migrate_three_to_four },
    SqliteMigrationStep { from: 4, to: 5, migrate: migrate_four_to_five },
];

pub(crate) fn migrate_asset_library(connection: &mut Connection) -> anyhow::Result<()> {
    validate_registry()?;
    let mut version = sqlite_user_version(connection)?;
    if version > ASSET_LIBRARY_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported asset library schema version: {} (current {})",
            version,
            ASSET_LIBRARY_SCHEMA_VERSION
        );
    }
    while version < ASSET_LIBRARY_SCHEMA_VERSION {
        let step = MIGRATIONS.iter().find(|step| step.from == version).with_context(|| {
            format!(
                "missing asset library migration from {version} to {ASSET_LIBRARY_SCHEMA_VERSION}"
            )
        })?;
        let transaction = connection.transaction()?;
        (step.migrate)(&transaction).with_context(|| {
            format!(
                "asset library migration {} -> {} failed",
                step.from, step.to
            )
        })?;
        transaction.pragma_update(None, "user_version", step.to)?;
        transaction.commit()?;
        version = step.to;
    }
    validate_current_schema(connection)
}

fn migrate_zero_to_one(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    if table_exists(transaction, "assets")? {
        transaction.execute_batch(CREATE_FOLDERS_SQL)?;
        add_column_if_missing(transaction, "assets", "folder_id", "TEXT")?;
        add_column_if_missing(
            transaction,
            "assets",
            "interpretation",
            "TEXT DEFAULT '{\"color\":{\"mode\":\"auto\"}}'",
        )?;
    }
    transaction.execute_batch(INIT_SQL)?;
    Ok(())
}

fn migrate_one_to_two(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    add_column_if_missing(
        transaction,
        "assets",
        "audio_components",
        "TEXT NOT NULL DEFAULT '{\"components\":[]}'",
    )?;

    let rows = {
        let mut statement = transaction.prepare("SELECT id, path, metadata FROM assets")?;
        let mapped = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (asset_id, _path, metadata) in rows {
        let info = serde_json::from_str::<MediaInfo>(&metadata)
            .with_context(|| format!("asset {asset_id} has invalid media metadata"))?;
        let catalog =
            AssetAudioComponentCatalog::from_media_info(&info, MediaFileFingerprint::default());
        let catalog_json = serde_json::to_string(&catalog)
            .with_context(|| format!("serialize audio Component catalog for asset {asset_id}"))?;
        transaction.execute(
            "UPDATE assets SET audio_components = ?1 WHERE id = ?2",
            rusqlite::params![catalog_json, asset_id],
        )?;
    }
    Ok(())
}

fn migrate_two_to_three(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    add_column_if_missing(transaction, "assets", "source_fingerprint", "TEXT")?;

    // Older schemas never stored the fingerprint that authorized `metadata`.
    // A current filesystem observation must not be paired with those stale
    // probe facts. Preserve logical Component IDs, but revoke every physical
    // binding until the media Adapter supplies one coherent fresh candidate.
    let rows = {
        let mut statement = transaction.prepare("SELECT id, audio_components FROM assets")?;
        let mapped = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (asset_id, catalog_json) in rows {
        let mut catalog = serde_json::from_str::<AssetAudioComponentCatalog>(&catalog_json)
            .with_context(|| format!("asset {asset_id} has invalid audio Component catalog"))?;
        catalog.source_fingerprint = MediaFileFingerprint::default();
        transaction.execute(
            "UPDATE assets SET source_fingerprint = NULL, audio_components = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&catalog)?, asset_id],
        )?;
    }
    Ok(())
}

fn migrate_three_to_four(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    // Asset identity is Project-contained author state. Product removal hides a
    // record from ordinary Library membership; it never deletes the record or
    // its recoverable source/provider binding.
    add_column_if_missing(transaction, "assets", "retired_at", "TEXT")?;
    Ok(())
}

fn migrate_four_to_five(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    // v5 removes the parallel standard-layout enum representation. Rewrite
    // only typed layout fields; arbitrary asset metadata strings are never
    // interpreted as schema values.
    let rows = {
        let mut statement =
            transaction.prepare("SELECT id, metadata, audio_components FROM assets")?;
        let mapped = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (asset_id, metadata, audio_components) in rows {
        let metadata = rewrite_legacy_channel_layout_json(&metadata)
            .with_context(|| format!("asset {asset_id} has invalid media metadata"))?;
        let audio_components = rewrite_legacy_channel_layout_json(&audio_components)
            .with_context(|| format!("asset {asset_id} has invalid audio Component catalog"))?;
        transaction.execute(
            "UPDATE assets SET metadata = ?1, audio_components = ?2 WHERE id = ?3",
            rusqlite::params![metadata, audio_components, asset_id],
        )?;
    }
    Ok(())
}

fn rewrite_legacy_channel_layout_json(encoded: &str) -> anyhow::Result<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(encoded)?;
    rewrite_legacy_channel_layout_fields(&mut value)?;
    Ok(serde_json::to_string(&value)?)
}

fn rewrite_legacy_channel_layout_fields(value: &mut serde_json::Value) -> anyhow::Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, field) in fields {
                if matches!(name.as_str(), "channel_layout" | "source_layout") {
                    rewrite_one_legacy_channel_layout(field)?;
                } else {
                    rewrite_legacy_channel_layout_fields(field)?;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                rewrite_legacy_channel_layout_fields(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn rewrite_one_legacy_channel_layout(value: &mut serde_json::Value) -> anyhow::Result<()> {
    let exact = match value.as_str() {
        Some("Mono") => Some(AudioChannelLayout::Mono),
        Some("Stereo") => Some(AudioChannelLayout::Stereo),
        Some("Surround51Side") => Some(AudioChannelLayout::Surround51Side),
        Some("Surround51Back") => Some(AudioChannelLayout::Surround51Back),
        Some("Surround71") => Some(AudioChannelLayout::Surround71),
        _ => None,
    };
    if let Some(layout) = exact {
        *value = serde_json::json!({"Exact": serde_json::to_value(layout)?});
        return Ok(());
    }
    if let Some(fields) = value.as_object_mut() {
        if let Some(channels) = fields.remove("Other") {
            fields.insert("Unsupported".to_owned(), channels);
        }
    }
    Ok(())
}

fn add_column_if_missing(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    declaration: &str,
) -> anyhow::Result<()> {
    if column_exists(transaction, table, column)? {
        return Ok(());
    }
    transaction.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
    ))?;
    Ok(())
}

fn validate_current_schema(connection: &Connection) -> anyhow::Result<()> {
    for table in [
        "folders",
        "assets",
        "asset_project_links",
        "ai_generation_log",
    ] {
        if !table_exists(connection, table)? {
            anyhow::bail!(
                "asset library schema v{ASSET_LIBRARY_SCHEMA_VERSION} is missing table `{table}`"
            );
        }
    }
    for column in [
        "folder_id",
        "interpretation",
        "audio_components",
        "source_fingerprint",
        "retired_at",
    ] {
        if !column_exists(connection, "assets", column)? {
            anyhow::bail!(
                "asset library schema v{ASSET_LIBRARY_SCHEMA_VERSION} is missing assets.{column}"
            );
        }
    }
    Ok(())
}

fn sqlite_user_version(connection: &Connection) -> anyhow::Result<u32> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    u32::try_from(version).context("asset library user_version is outside u32 range")
}

fn table_exists(connection: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_registry() -> anyhow::Result<()> {
    let mut expected = 0;
    for step in MIGRATIONS {
        if step.from != expected || step.to != step.from.saturating_add(1) {
            anyhow::bail!("asset library migration registry is not contiguous");
        }
        expected = step.to;
    }
    if expected != ASSET_LIBRARY_SCHEMA_VERSION {
        anyhow::bail!(
            "asset library migration registry ends at {expected}, current is {ASSET_LIBRARY_SCHEMA_VERSION}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unversioned_legacy_database_migrates_once_in_order() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection
            .execute_batch(include_str!("../tests/fixtures/v0/library.sql"))
            .expect("legacy schema");

        migrate_asset_library(&mut connection).expect("migrate");
        migrate_asset_library(&mut connection).expect("idempotent reopen");

        assert_eq!(
            sqlite_user_version(&connection).expect("version"),
            ASSET_LIBRARY_SCHEMA_VERSION
        );
        assert!(column_exists(&connection, "assets", "folder_id").expect("folder column"));
        assert!(
            column_exists(&connection, "assets", "interpretation").expect("interpretation column")
        );
        assert!(column_exists(&connection, "assets", "audio_components").expect("audio catalog"));
        assert!(
            column_exists(&connection, "assets", "source_fingerprint").expect("source fingerprint")
        );
        assert!(column_exists(&connection, "assets", "retired_at").expect("retirement column"));
    }

    #[test]
    fn failed_migration_rolls_back_schema_and_version() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection
            .execute_batch("CREATE TABLE assets (id TEXT PRIMARY KEY);")
            .expect("malformed legacy schema");

        assert!(migrate_asset_library(&mut connection).is_err());

        assert_eq!(sqlite_user_version(&connection).expect("version"), 0);
        assert!(!column_exists(&connection, "assets", "folder_id").expect("rolled back column"));
        assert!(!table_exists(&connection, "folders").expect("rolled back table"));
    }

    #[test]
    fn invalid_v1_media_metadata_rolls_back_audio_catalog_migration() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection
            .execute_batch(include_str!("../tests/fixtures/v0/library.sql"))
            .expect("legacy schema");
        {
            let transaction = connection.transaction().expect("transaction");
            migrate_zero_to_one(&transaction).expect("migrate to v1");
            transaction.pragma_update(None, "user_version", 1).expect("version");
            transaction.commit().expect("commit v1");
        }
        connection
            .execute(
                "INSERT INTO assets \
                 (id, name, asset_type, path, metadata, created_at, updated_at) \
                 VALUES ('asset-1', 'broken', 'audio', 'broken.wav', '{}', 'now', 'now')",
                [],
            )
            .expect("insert malformed metadata");

        assert!(migrate_asset_library(&mut connection).is_err());

        assert_eq!(sqlite_user_version(&connection).expect("version"), 1);
        assert!(!column_exists(&connection, "assets", "audio_components").expect("rolled back"));
    }

    #[test]
    fn v2_probe_bindings_are_revoked_instead_of_paired_with_current_filesystem_state() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection.execute_batch(INIT_SQL).expect("current tables");
        let catalog = AssetAudioComponentCatalog {
            source_fingerprint: MediaFileFingerprint {
                len: Some(42),
                modified_secs: Some(7),
                modified_nanos: Some(9),
                ..MediaFileFingerprint::default()
            },
            components: Vec::new(),
        };
        connection
            .execute(
                "INSERT INTO assets \
                 (id, name, asset_type, path, metadata, audio_components, created_at, updated_at) \
                 VALUES ('asset-1', 'legacy', 'audio', 'legacy.wav', '{}', ?1, 'now', 'now')",
                [serde_json::to_string(&catalog).expect("catalog")],
            )
            .expect("legacy row");
        connection.pragma_update(None, "user_version", 2).expect("v2");

        migrate_asset_library(&mut connection).expect("migrate");

        let (fingerprint, catalog_json): (Option<String>, String) = connection
            .query_row(
                "SELECT source_fingerprint, audio_components FROM assets WHERE id = 'asset-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("migrated row");
        let migrated: AssetAudioComponentCatalog =
            serde_json::from_str(&catalog_json).expect("migrated catalog");
        assert!(fingerprint.is_none());
        assert_eq!(migrated.source_fingerprint, MediaFileFingerprint::default());
        assert!(!migrated.source_fingerprint.authorizes_reuse());
    }

    #[test]
    fn v3_assets_remain_visible_when_retirement_membership_is_added() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection.execute_batch(INIT_SQL).expect("current tables");
        connection
            .execute(
                "INSERT INTO assets \
                 (id, name, asset_type, path, metadata, audio_components, created_at, updated_at) \
                 VALUES ('asset-1', 'legacy', 'solid_color', 'generated:solid-color:asset-1', \
                 'null', '{\"components\":[]}', 'now', 'now')",
                [],
            )
            .expect("legacy row");
        connection
            .execute_batch("ALTER TABLE assets DROP COLUMN retired_at")
            .expect("model v3 schema");
        connection.pragma_update(None, "user_version", 3).expect("v3");

        migrate_asset_library(&mut connection).expect("migrate");

        let retired_at: Option<String> = connection
            .query_row(
                "SELECT retired_at FROM assets WHERE id = 'asset-1'",
                [],
                |row| row.get(0),
            )
            .expect("migrated row");
        assert!(retired_at.is_none());
    }

    #[test]
    fn v4_layout_evidence_is_rewritten_transactionally_without_touching_labels() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection.execute_batch(INIT_SQL).expect("current tables");
        connection
            .execute(
                "INSERT INTO assets \
                 (id, name, asset_type, path, metadata, audio_components, created_at, updated_at) \
                 VALUES ('asset-1', 'Stereo', 'audio', 'legacy.wav', \
                 '{\"audio_streams\":[{\"channel_layout\":\"Stereo\",\"title\":\"Stereo\"}]}', \
                 '{\"components\":[{\"source_layout\":{\"Other\":6}}]}', 'now', 'now')",
                [],
            )
            .expect("legacy row");
        connection.pragma_update(None, "user_version", 4).expect("v4");

        migrate_asset_library(&mut connection).expect("migrate");

        let (metadata, components): (String, String) = connection
            .query_row(
                "SELECT metadata, audio_components FROM assets WHERE id = 'asset-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("migrated row");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata");
        let components: serde_json::Value = serde_json::from_str(&components).expect("components");
        assert_eq!(metadata["audio_streams"][0]["title"], "Stereo");
        assert_eq!(
            metadata["audio_streams"][0]["channel_layout"],
            serde_json::json!({"Exact": {"Speakers": ["FrontLeft", "FrontRight"]}})
        );
        assert_eq!(
            components["components"][0]["source_layout"],
            serde_json::json!({"Unsupported": 6})
        );
        assert_eq!(
            sqlite_user_version(&connection).expect("version"),
            ASSET_LIBRARY_SCHEMA_VERSION
        );
    }
}
