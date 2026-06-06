//! 字体管理
//!
//! 创建 cosmic-text 0.19 FontSystem，管理字体系列查询。

use cosmic_text::{Attrs, Family, FontSystem, Weight};

pub struct FontManager {
    pub font_system: FontSystem,
}

impl FontManager {
    pub fn new() -> Self {
        let locale = std::env::var("LANG").unwrap_or_else(|_| "en-US".into());
        let db = fontdb::Database::new();
        let font_system = FontSystem::new_with_locale_and_db(locale, db);
        Self { font_system }
    }

    pub fn default_attrs(&self, font_size: f32) -> Attrs<'_> {
        Attrs::new()
            .family(Family::SansSerif)
            .weight(Weight::NORMAL)
    }

    pub fn mono_attrs(&self, font_size: f32) -> Attrs<'_> {
        Attrs::new()
            .family(Family::Monospace)
            .weight(Weight::NORMAL)
    }
}

impl Default for FontManager {
    fn default() -> Self {
        Self::new()
    }
}
