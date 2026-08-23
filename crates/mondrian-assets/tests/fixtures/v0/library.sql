CREATE TABLE assets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    asset_type TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    thumbnail TEXT,
    tags TEXT DEFAULT '[]',
    metadata TEXT DEFAULT '{}',
    usage_count INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
