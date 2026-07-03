//! SQLite 数据库 Schema 初始化

pub const INIT_SQL: &str = r#"
-- 文件夹 / Bin 层级
CREATE TABLE IF NOT EXISTS folders (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    parent_id   TEXT,
    sort_order  INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES folders(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);

-- 全局资产索引
CREATE TABLE IF NOT EXISTS assets (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    asset_type  TEXT NOT NULL,
    path        TEXT NOT NULL UNIQUE,
    folder_id   TEXT,
    thumbnail   TEXT,
    tags        TEXT DEFAULT '[]',
    metadata    TEXT DEFAULT '{}',
    interpretation TEXT DEFAULT '{"color":{"mode":"auto"}}',
    usage_count INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_assets_type ON assets(asset_type);
CREATE INDEX IF NOT EXISTS idx_assets_name ON assets(name);
CREATE INDEX IF NOT EXISTS idx_assets_folder ON assets(folder_id);

-- 跨项目引用
CREATE TABLE IF NOT EXISTS asset_project_links (
    asset_id   TEXT NOT NULL,
    project_id TEXT NOT NULL,
    linked_at  TEXT NOT NULL,
    PRIMARY KEY (asset_id, project_id)
);

-- AI 生成历史
CREATE TABLE IF NOT EXISTS ai_generation_log (
    id           TEXT PRIMARY KEY,
    asset_id     TEXT REFERENCES assets(id),
    provider     TEXT NOT NULL,
    prompt       TEXT NOT NULL,
    params       TEXT DEFAULT '{}',
    created_at   TEXT NOT NULL
);
"#;

/// Migration SQL for existing databases that don't have the folder columns.
pub const MIGRATE_FOLDERS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS folders (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    parent_id   TEXT,
    sort_order  INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES folders(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);

ALTER TABLE assets ADD COLUMN folder_id TEXT;
CREATE INDEX IF NOT EXISTS idx_assets_folder ON assets(folder_id);
"#;

/// Migration SQL for existing databases that don't have asset interpretation.
pub const MIGRATE_INTERPRETATION_SQL: &str = r#"
ALTER TABLE assets ADD COLUMN interpretation TEXT DEFAULT '{"color":{"mode":"auto"}}';
"#;
