use crate::schema::{CREATE_FOLDERS_SQL, INIT_SQL};
use anyhow::Context;
use rusqlite::{Connection, Transaction};

/// Current asset-library SQLite schema version stored in `PRAGMA user_version`.
pub const ASSET_LIBRARY_SCHEMA_VERSION: u32 = 1;

type SqliteMigrationFn = fn(&Transaction<'_>) -> anyhow::Result<()>;

struct SqliteMigrationStep {
    from: u32,
    to: u32,
    migrate: SqliteMigrationFn,
}

const MIGRATIONS: &[SqliteMigrationStep] =
    &[SqliteMigrationStep { from: 0, to: 1, migrate: migrate_zero_to_one }];

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
            anyhow::bail!("asset library schema v1 is missing table `{table}`");
        }
    }
    for column in ["folder_id", "interpretation"] {
        if !column_exists(connection, "assets", column)? {
            anyhow::bail!("asset library schema v1 is missing assets.{column}");
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

        assert_eq!(sqlite_user_version(&connection).expect("version"), 1);
        assert!(column_exists(&connection, "assets", "folder_id").expect("folder column"));
        assert!(
            column_exists(&connection, "assets", "interpretation").expect("interpretation column")
        );
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
}
