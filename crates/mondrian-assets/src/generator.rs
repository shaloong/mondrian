//! Procedural asset generation framework.
//!
//! Provides a registry for procedural content generators. When an asset
//! with `AssetSource::Generated(_)` is encountered, the framework dispatches
//! to the registered generator for that kind.

use mondrian_core::types::{AssetSource, GeneratedAssetKind};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A procedural content generator — produces asset data from parameters.
///
/// Implementations register themselves via `register_procedural_generator`.
/// The engine calls `generate` when a `Generated` asset source is first
/// accessed or when the cache is invalidated.
pub trait ProceduralGenerator: Send + Sync {
    /// Unique key for this generator — should match a `GeneratedAssetKind`.
    fn kind(&self) -> GeneratedAssetKind;

    /// Human-readable label for debugging and UI.
    fn label(&self) -> &str;

    /// Generate RGBA8 pixel data for the given dimensions.
    /// Returns `None` if the combination of parameters is unsupported.
    fn generate_rgba8(
        &self,
        params: &serde_json::Value,
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>>;
}

/// Registry of procedural content generators.
///
/// Each `GeneratedAssetKind` variant has at most one registered generator.
/// Generators are registered at application startup and never removed.
#[derive(Default)]
pub struct GeneratorRegistry {
    generators: HashMap<GeneratedAssetKind, Arc<dyn ProceduralGenerator>>,
}

impl GeneratorRegistry {
    pub fn register(&mut self, generator: Arc<dyn ProceduralGenerator>) {
        let kind = generator.kind();
        self.generators.insert(kind, generator);
    }

    pub fn get(&self, kind: &GeneratedAssetKind) -> Option<&Arc<dyn ProceduralGenerator>> {
        self.generators.get(kind)
    }

    /// Generate asset data for a Generated asset source.
    pub fn generate(
        &self,
        kind: &GeneratedAssetKind,
        params: &serde_json::Value,
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        self.get(kind)?.generate_rgba8(params, width, height)
    }
}

/// Global generator registry.
fn global_registry() -> &'static Mutex<GeneratorRegistry> {
    static REGISTRY: std::sync::OnceLock<Mutex<GeneratorRegistry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(GeneratorRegistry::default()))
}

/// Register a procedural generator. Call at application startup.
pub fn register_procedural_generator(generator: Arc<dyn ProceduralGenerator>) {
    global_registry().lock().unwrap().register(generator);
}

/// Generate content for an asset source.
///
/// Returns `None` if the source is not `Generated` or has no registered generator.
pub fn generate_asset_content(
    source: &AssetSource,
    params: &serde_json::Value,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    match source {
        AssetSource::Generated(kind) => {
            global_registry().lock().unwrap().generate(kind, params, width, height)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestGenerator;
    impl ProceduralGenerator for TestGenerator {
        fn kind(&self) -> GeneratedAssetKind {
            GeneratedAssetKind::SolidColor
        }
        fn label(&self) -> &str {
            "TestSolid"
        }
        fn generate_rgba8(&self, _params: &serde_json::Value, w: u32, h: u32) -> Option<Vec<u8>> {
            Some([255, 0, 0, 255].repeat(w as usize * h as usize))
        }
    }

    #[test]
    fn registry_lookup_by_kind() {
        let mut reg = GeneratorRegistry::default();
        reg.register(Arc::new(TestGenerator));
        assert!(reg.get(&GeneratedAssetKind::SolidColor).is_some());
        assert!(reg.get(&GeneratedAssetKind::AdjustmentLayer).is_none());
    }

    #[test]
    fn generate_via_registry() {
        let mut reg = GeneratorRegistry::default();
        reg.register(Arc::new(TestGenerator));
        let data = reg.generate(
            &GeneratedAssetKind::SolidColor,
            &serde_json::json!({"color": "#ff0000"}),
            16,
            16,
        );
        assert!(data.is_some());
        assert_eq!(data.unwrap().len(), 16 * 16 * 4);
    }

    #[test]
    fn generate_asset_content_dispatches() {
        register_procedural_generator(Arc::new(TestGenerator));
        let source = AssetSource::Generated(GeneratedAssetKind::SolidColor);
        let data = generate_asset_content(&source, &serde_json::json!({}), 8, 8);
        assert!(data.is_some());
    }

    #[test]
    fn file_source_returns_none() {
        let source = AssetSource::File(std::path::PathBuf::from("test.mp4"));
        assert!(generate_asset_content(&source, &serde_json::json!({}), 8, 8).is_none());
    }
}
