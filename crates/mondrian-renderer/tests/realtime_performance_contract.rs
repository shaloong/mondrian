use mondrian_renderer::{
    RealtimeVisualScenarioId, PROFESSIONAL_REALTIME_VIEWER_MAX_ACTIVE_TEXTURES,
    PROFESSIONAL_REALTIME_VIEWER_MAX_ACTIVE_TEXTURE_BYTES,
    PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_PER_CONTRACT,
    PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_TEXTURE_BYTES, REALTIME_VISUAL_PERFORMANCE_PROFILE,
    REALTIME_VISUAL_PERFORMANCE_REPORT_SCHEMA_VERSION,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("renderer crate is nested under workspace/crates")
        .to_path_buf()
}

fn strings(value: &Value, field: &str) -> BTreeSet<String> {
    value[field]
        .as_array()
        .expect("array field")
        .iter()
        .map(|item| item.as_str().expect("string array item").to_owned())
        .collect()
}

#[test]
fn sealed_matrix_matches_renderer_profile_and_product_resource_grant() {
    let matrix: Value = serde_json::from_slice(
        &std::fs::read(repository_root().join("tests/validation/realtime-performance-matrix.json"))
            .expect("read realtime matrix"),
    )
    .expect("parse realtime matrix");
    assert_eq!(matrix["schema_version"], 1);
    assert_eq!(matrix["execution_policy"], "sealed-required");
    assert_eq!(
        strings(&matrix, "required_dimensions"),
        BTreeSet::from([
            "120-minute-authoring".to_owned(),
            "30-minute-audio-recovery".to_owned(),
            "30-minute-video-playback".to_owned(),
            "4k60-hdr-multilayer-multieffect-scopes".to_owned(),
            "8k30-hdr-multilayer-multieffect-scopes".to_owned(),
            "real-4k60-main10-dual-layer-decode-publish".to_owned(),
        ])
    );

    let visual_gate = matrix["gates"]
        .as_array()
        .expect("gate array")
        .iter()
        .find(|gate| gate["id"] == "renderer-visual")
        .expect("renderer visual gate");
    assert_eq!(
        visual_gate["expected_report_schema"],
        REALTIME_VISUAL_PERFORMANCE_REPORT_SCHEMA_VERSION
    );
    assert_eq!(
        visual_gate["expected_profile"],
        REALTIME_VISUAL_PERFORMANCE_PROFILE
    );
    assert_eq!(
        strings(visual_gate, "expected_scenarios"),
        BTreeSet::from([
            "eight_k30_hdr_multilayer_effects_scopes".to_owned(),
            "uhd4k60_hdr_multilayer_effects_scopes".to_owned(),
        ])
    );

    for scenario in RealtimeVisualScenarioId::ALL {
        let grant = scenario.contract().resource_grant;
        assert_eq!(
            grant.max_idle_per_contract(),
            PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_PER_CONTRACT
        );
        assert_eq!(
            grant.max_idle_bytes(),
            PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_TEXTURE_BYTES
        );
        assert_eq!(
            grant.max_active_texture_bytes(),
            PROFESSIONAL_REALTIME_VIEWER_MAX_ACTIVE_TEXTURE_BYTES
        );
        assert_eq!(
            grant.max_active_textures(),
            PROFESSIONAL_REALTIME_VIEWER_MAX_ACTIVE_TEXTURES
        );
    }
}

#[test]
fn sealed_matrix_names_real_required_producers_and_forbids_skips() {
    let root = repository_root();
    let matrix: Value = serde_json::from_slice(
        &std::fs::read(root.join("tests/validation/realtime-performance-matrix.json"))
            .expect("read realtime matrix"),
    )
    .expect("parse realtime matrix");
    assert_eq!(matrix["acceptance"]["skip_forbidden"], true);
    assert_eq!(matrix["acceptance"]["serial_execution_required"], true);

    let gate_ids = matrix["gates"]
        .as_array()
        .expect("gate array")
        .iter()
        .map(|gate| gate["id"].as_str().expect("gate id").to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        gate_ids,
        BTreeSet::from([
            "long-authoring".to_owned(),
            "playback-reference".to_owned(),
            "real-4k60-dual-layer".to_owned(),
            "renderer-visual".to_owned(),
        ])
    );

    let app_source =
        std::fs::read_to_string(root.join("crates/mondrian-app/src/app/perf_tests.rs"))
            .expect("read App perf producers");
    assert!(app_source.contains("fn preview_media_realtime_4k60_dual_video_gate()"));
    let authoring_source =
        std::fs::read_to_string(root.join("crates/mondrian-app/src/app/authoring_perf.rs"))
            .expect("read authoring perf producer");
    assert!(authoring_source.contains("fn large_project_authoring_perf_matrix()"));

    let supervisor = std::fs::read_to_string(
        root.join("scripts/validation/invoke-realtime-performance-matrix.ps1"),
    )
    .expect("read realtime supervisor");
    assert!(supervisor.contains("Invoke-BoundedPlaybackGateProcess"));
    assert!(supervisor.contains("Local\\MondrianRealtimePerformanceMatrixV1"));
    assert!(supervisor.contains("\"unqualified\""));
}
