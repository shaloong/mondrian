//! Product-level Camera RAW interpretation transaction contract.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mondrian_app::app::product_action::{
    AssetProductAction, AssetSetInterpretationPayload, ProductAction,
};
use mondrian_app::app::AppState;
use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::{
    CameraRawDebayerQuality, CameraRawInterpretation, CameraRawWhiteBalance, Rational,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "mondrian-camera-raw-authoring-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create Camera RAW fixture root");
        Self(path)
    }

    fn project_file(&self) -> PathBuf {
        self.0.join("camera-raw-authoring.mdp")
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn camera_raw_interpretation_round_trips_and_commits_atomically() {
    let fixture = FixtureRoot::new();
    let mut state = AppState::new();
    state
        .create_new_project_at(
            fixture.project_file(),
            "Camera RAW Authoring",
            1920,
            1080,
            Rational::new(24, 1),
        )
        .expect("create project");
    let asset_id = state
        .asset_library()
        .expect("asset library")
        .create_solid_color_asset(Some("RAW authoring fixture"))
        .expect("create asset");
    let interpretation = AssetMediaInterpretation {
        camera_raw: CameraRawInterpretation {
            exposure_millistops: 1_000,
            white_balance: CameraRawWhiteBalance::TemperatureTint {
                temperature_kelvin: 5_600,
                tint_milli: 25,
            },
            debayer_quality: CameraRawDebayerQuality::Bilinear,
        },
        ..AssetMediaInterpretation::default()
    };
    let product = ProductAction::Asset(AssetProductAction::SetInterpretation(
        AssetSetInterpretationPayload { asset_id, interpretation },
    ));
    let external = product.clone().into_external_action();
    assert_eq!(
        ProductAction::decode_external(&external)
            .expect("decode external RAW action")
            .expect("recognized product namespace"),
        product
    );

    state.dispatch_action(external).expect("commit RAW interpretation");

    let stored = state
        .asset_library()
        .expect("asset library")
        .get_asset(asset_id)
        .expect("read asset")
        .expect("asset exists")
        .interpretation;
    assert_eq!(stored, interpretation);

    let invalid = AssetMediaInterpretation {
        camera_raw: CameraRawInterpretation {
            exposure_millistops: 5_001,
            ..CameraRawInterpretation::default()
        },
        ..stored
    };
    state
        .dispatch_action(
            ProductAction::Asset(AssetProductAction::SetInterpretation(
                AssetSetInterpretationPayload { asset_id, interpretation: invalid },
            ))
            .into_external_action(),
        )
        .expect_err("invalid RAW bounds must fail closed");
    assert_eq!(
        state
            .asset_library()
            .expect("asset library")
            .get_asset(asset_id)
            .expect("read asset")
            .expect("asset exists")
            .interpretation,
        stored
    );
}
