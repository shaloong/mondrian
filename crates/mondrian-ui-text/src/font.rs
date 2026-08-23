//! 字体管理
//!
//! 创建 cosmic-text 0.19 FontSystem，管理字体系列查询。

use cosmic_text::{Attrs, Family, FontSystem, Weight};

pub struct FontManager {
    pub font_system: FontSystem,
}

impl FontManager {
    pub fn new() -> Self {
        let font_system = FontSystem::new();
        Self { font_system }
    }

    pub fn default_attrs(&self) -> Attrs<'_> {
        Attrs::new().family(Family::SansSerif).weight(Weight::NORMAL)
    }

    pub fn mono_attrs(&self) -> Attrs<'_> {
        Attrs::new().family(Family::Monospace).weight(Weight::NORMAL)
    }
}

impl Default for FontManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_manager_creates_default_attrs() {
        let mgr = FontManager::new();
        let attrs = mgr.default_attrs();
        assert_eq!(attrs.family, Family::SansSerif);
    }

    #[test]
    fn font_manager_creates_mono_attrs() {
        let mgr = FontManager::new();
        let attrs = mgr.mono_attrs();
        assert_eq!(attrs.family, Family::Monospace);
    }

    #[test]
    fn font_manager_default_constructs() {
        let _mgr = FontManager::default();
    }
}
