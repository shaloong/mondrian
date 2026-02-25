# 素材资产系统设计

## 1. 设计定位

> **这是 Mondrian 的核心差异化竞争力**

对标 Unity 引擎的资源管理系统，实现：

- 跨项目素材复用（角色/场景/提示词/预设）
- AI 人物形象库（LoRA + 参考图管理）
- 模板系统（效果预设包）
- 本地 SQLite 索引 + 文件系统存储

---

## 2. 目录结构

```text
~/.mondrian/library/           本地素材库根目录
│
├── media/                     用户导入的媒体素材
│   ├── video/
│   ├── audio/
│   └── image/
│
├── characters/                角色设定库（核心创新）
│   ├── {character_id}/
│   │   ├── manifest.json      角色元数据
│   │   ├── avatar.png         角色头像
│   │   ├── references/        参考图（多张）
│   │   ├── lora/              LoRA 权重（可选，本地模型）
│   │   └── prompts/
│   │       ├── base.txt       基础外观提示词
│   │       ├── action/        动作提示词模板
│   │       └── scene/         场景提示词模板
│   └── ...
│
├── scenes/                    场景设定库
│   ├── {scene_id}/
│   │   ├── manifest.json
│   │   ├── thumbnail.jpg
│   │   ├── references/
│   │   └── prompts/
│   └── ...
│
├── music/                     音乐素材库
│   ├── {track_id}.mp3
│   └── {track_id}.json        曲目元数据（BPM/情绪/时长）
│
├── templates/                 效果预设包
│   ├── color/                 调色预设（LUT + 参数）
│   ├── transitions/           转场预设
│   ├── titles/                文字动画预设
│   └── workflows/             AI 工作流预设
│
├── ai_generated/              AI 生成素材（自动归档）
│   ├── images/
│   ├── videos/
│   └── prompts/
│
└── index.db                   SQLite 全局索引
```

---

## 3. 核心数据结构

```rust
/// 素材资产统一表示
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub name: String,
    pub asset_type: AssetType,
    pub path: PathBuf,           // 绝对路径
    pub thumbnail_path: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub metadata: AssetMetadata, // 类型特定元数据
    pub usage_count: u32,        // 被多少项目引用
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssetType {
    Video,
    Audio,
    Image,
    Character,
    Scene,
    MusicTrack,
    ColorPreset,
    TransitionPreset,
    TitlePreset,
    Workflow,
    AiGenerated { generator: String, prompt: String },
}

/// 角色设定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterManifest {
    pub id: CharacterId,
    pub name: String,
    pub description: String,
    pub avatar: PathBuf,
    pub references: Vec<PathBuf>,
    pub base_prompt: String,            // 外观描述提示词
    pub negative_prompt: String,
    pub lora_config: Option<LoraConfig>,
    pub action_prompts: HashMap<String, String>,   // "running" → "..."
    pub scene_prompts: HashMap<String, String>,    // "outdoor" → "..."
    pub voice_config: Option<VoiceConfig>,         // TTS 配置（未来）
    pub created_at: DateTime<Utc>,
}

/// 场景设定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneManifest {
    pub id: SceneId,
    pub name: String,
    pub description: String,
    pub thumbnail: PathBuf,
    pub references: Vec<PathBuf>,
    pub base_prompt: String,
    pub time_of_day_variants: HashMap<String, String>, // "dawn", "noon", "dusk"
    pub weather_variants: HashMap<String, String>,     // "sunny", "rainy", "foggy"
    pub created_at: DateTime<Utc>,
}
```text

---

## 4. AssetLibrary 接口

```rust
pub struct AssetLibrary {
    root: PathBuf,
    db: Arc<Mutex<rusqlite::Connection>>,
    watcher: notify::RecommendedWatcher,  // 文件系统变更监听
}

impl AssetLibrary {
    // ── 通用资产操作 ───────────────────────────────────
    pub fn import(&self, path: &Path, options: ImportOptions) -> Result<Asset>;
    pub fn get(&self, id: AssetId) -> Option<Asset>;
    pub fn delete(&self, id: AssetId) -> Result<()>;
    pub fn search(&self, query: AssetQuery) -> Vec<Asset>;

    // ── 缩略图 ─────────────────────────────────────────
    pub async fn generate_thumbnail(&self, id: AssetId) -> Result<PathBuf>;

    // ── 角色库 ─────────────────────────────────────────
    pub fn create_character(&self, manifest: CharacterManifest) -> Result<CharacterId>;
    pub fn get_character(&self, id: CharacterId) -> Option<CharacterManifest>;
    pub fn list_characters(&self) -> Vec<CharacterManifest>;
    pub fn add_character_reference(&self, id: CharacterId, image: &Path) -> Result<()>;

    /// 根据角色生成综合提示词
    pub fn build_character_prompt(
        &self,
        id: CharacterId,
        action: Option<&str>,
        scene: Option<SceneId>,
        extra_prompt: Option<&str>,
    ) -> Result<String>;

    // ── 场景库 ─────────────────────────────────────────
    pub fn create_scene(&self, manifest: SceneManifest) -> Result<SceneId>;
    pub fn list_scenes(&self) -> Vec<SceneManifest>;

    // ── 跨项目引用 ─────────────────────────────────────
    pub fn link_asset_to_project(
        &self,
        asset_id: AssetId,
        project_id: ProjectId,
    ) -> Result<AssetLink>;
}
```

---

## 5. 角色提示词合成逻辑

```rust
impl AssetLibrary {
    pub fn build_character_prompt(
        &self,
        char_id: CharacterId,
        action: Option<&str>,
        scene_id: Option<SceneId>,
        extra: Option<&str>,
    ) -> Result<String> {
        let character = self.get_character(char_id)
            .ok_or(AssetError::NotFound)?;

        let mut parts = vec![character.base_prompt.clone()];

        // 动作提示词
        if let Some(action) = action {
            if let Some(action_prompt) = character.action_prompts.get(action) {
                parts.push(action_prompt.clone());
            }
        }

        // 场景提示词
        if let Some(scene_id) = scene_id {
            if let Some(scene) = self.get_scene(scene_id) {
                parts.push(scene.base_prompt.clone());
                // 将角色的场景特化提示词合并
                if let Some(char_scene_prompt) = character.scene_prompts
                    .get(&scene.name) {
                    parts.push(char_scene_prompt.clone());
                }
            }
        }

        if let Some(extra) = extra {
            parts.push(extra.to_string());
        }

        // 添加通用质量提示词
        parts.push("high quality, 8K, cinematic lighting".to_string());

        Ok(parts.join(", "))
    }
}
```text

---

## 6. SQLite 索引 Schema

```sql
-- 全局资产索引
CREATE TABLE assets (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    asset_type  TEXT NOT NULL,
    path        TEXT NOT NULL UNIQUE,
    thumbnail   TEXT,
    tags        TEXT,            -- JSON array
    metadata    TEXT,            -- JSON object
    usage_count INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_assets_type    ON assets(asset_type);
CREATE INDEX idx_assets_name    ON assets(name);
CREATE VIRTUAL TABLE assets_fts USING fts5(name, tags, metadata);

-- 跨项目引用
CREATE TABLE asset_project_links (
    asset_id   TEXT NOT NULL,
    project_id TEXT NOT NULL,
    linked_at  TEXT NOT NULL,
    PRIMARY KEY (asset_id, project_id)
);

-- AI 生成历史
CREATE TABLE ai_generation_log (
    id           TEXT PRIMARY KEY,
    asset_id     TEXT REFERENCES assets(id),
    provider     TEXT NOT NULL,
    prompt       TEXT NOT NULL,
    params       TEXT,           -- JSON
    cost_tokens  INTEGER,
    created_at   TEXT NOT NULL
);
```

---

## 7. 文件变更监听

当用户在文件系统中手动修改/删除素材时，库自动同步：

```rust
// 使用 notify crate 监听文件系统变更
let watcher = notify::recommended_watcher(|event| {
    match event.kind {
        EventKind::Create(_) => library.on_file_created(event.paths),
        EventKind::Remove(_) => library.on_file_removed(event.paths),
        EventKind::Modify(_) => library.on_file_modified(event.paths),
        _ => {}
    }
})?;
watcher.watch(&library_root, RecursiveMode::Recursive)?;
```text
