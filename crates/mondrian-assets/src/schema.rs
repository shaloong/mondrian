//! SQLite 数据库 Schema 初始化

pub const INIT_SQL: &str = r#"
-- 全局资产索引
CREATE TABLE IF NOT EXISTS assets (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    asset_type  TEXT NOT NULL,
    path        TEXT NOT NULL UNIQUE,
    thumbnail   TEXT,
    tags        TEXT DEFAULT '[]',
    metadata    TEXT DEFAULT '{}',
    usage_count INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assets_type ON assets(asset_type);
CREATE INDEX IF NOT EXISTS idx_assets_name ON assets(name);

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
