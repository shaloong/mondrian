use mondrian_core::{ensure_mondrian_default_ocio_loaded, ColorEngine, ColorSpace, GpuLanguage};
use mondrian_renderer::{OcioGpuShaderCache, OcioGpuShaderRequest};
use std::sync::Arc;

#[test]
fn preview_and_historical_export_share_plain_ocio_gpu_artifacts() {
    ensure_mondrian_default_ocio_loaded().expect("Mondrian Standard OCIO config");
    let request = OcioGpuShaderRequest::ColorSpace {
        engine: ColorEngine::mondrian_standard(),
        src: ColorSpace::ArriLogC4WideGamut4.into(),
        dst: ColorSpace::Rec709.into(),
        language: GpuLanguage::Glsl4_0,
    };

    let mut preview_owner = OcioGpuShaderCache::default();
    let preview_plan = preview_owner.get_or_extract(request.clone()).expect("Preview OCIO plan");

    let mut historical_export_owner = OcioGpuShaderCache::default();
    let export_plan = historical_export_owner
        .get_or_extract(request.clone())
        .expect("historical Export OCIO plan");
    let export_warm = historical_export_owner
        .get_or_extract(request)
        .expect("historical Export warm plan");

    assert!(Arc::ptr_eq(&preview_plan, &export_plan));
    assert!(Arc::ptr_eq(&export_plan, &export_warm));
    let diagnostics = historical_export_owner.diagnostics();
    assert_eq!(diagnostics.entries, 1);
    assert_eq!(diagnostics.misses, 1);
    assert_eq!(diagnostics.hits, 1);
    assert_eq!(diagnostics.shared_hits, 1);
    assert_eq!(diagnostics.shared_misses, 0);
    assert_eq!(diagnostics.extraction_failures, 0);
}

#[test]
fn different_engines_never_share_plain_ocio_gpu_artifacts() {
    let mut owner = OcioGpuShaderCache::default();
    let standard = OcioGpuShaderRequest::ColorSpace {
        engine: ColorEngine::mondrian_standard(),
        src: ColorSpace::AcesCg.into(),
        dst: ColorSpace::Aces2065_1.into(),
        language: GpuLanguage::Glsl4_0,
    };
    let aces = OcioGpuShaderRequest::ColorSpace {
        engine: ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        },
        src: ColorSpace::AcesCg.into(),
        dst: ColorSpace::Aces2065_1.into(),
        language: GpuLanguage::Glsl4_0,
    };

    let standard_plan = owner.get_or_extract(standard).expect("Standard OCIO plan");
    let aces_plan = owner.get_or_extract(aces).expect("ACES OCIO plan");

    assert!(!Arc::ptr_eq(&standard_plan, &aces_plan));
    assert_ne!(standard_plan.cache_key, aces_plan.cache_key);
}
