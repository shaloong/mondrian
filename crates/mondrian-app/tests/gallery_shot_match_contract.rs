use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use image::ImageEncoder;
use mondrian_app::app::product_action::{
    GalleryApplyShotMatchPayload, GalleryCaptureStillPayload, GalleryComparisonLayout,
    GalleryProductAction, GalleryRenameStillPayload, GallerySetComparisonPayload,
    GalleryStillTargetPayload, GradeCreateDefinitionPayload, GradeProductAction, ProductAction,
};
use mondrian_app::app::AppState;
use mondrian_app::app_ui::panels::ViewerPanelModel;
use mondrian_core::effect_data::EffectType;
use mondrian_core::{
    GalleryColorStatistics, GalleryRasterColorSpace, GalleryStillId, GalleryStillRaster,
    GradeDefinitionId, GradeVersionOrigin, Rational, TimelineTime,
};
use mondrian_editor_state::Action;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "mondrian-gallery-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create Gallery fixture root");
        Self(path)
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stats(low: f32, median: f32, high: f32) -> GalleryColorStatistics {
    GalleryColorStatistics {
        sample_count: 64,
        low_rgb: [low, low, low],
        median_rgb: [median, median, median],
        high_rgb: [high, high, high],
    }
}

fn raster() -> GalleryStillRaster {
    let pixels = [20_u8, 40, 80, 255, 200, 180, 160, 255];
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, 2, 1, image::ExtendedColorType::Rgba8)
        .expect("encode Gallery fixture PNG");
    GalleryStillRaster {
        width: 2,
        height: 1,
        color_space: GalleryRasterColorSpace::Srgb,
        png,
    }
}

fn round_trip(action: ProductAction) {
    let external = action.clone().into_external_action();
    assert_eq!(
        ProductAction::decode_external(&external)
            .expect("decode Gallery action")
            .expect("recognized Gallery namespace"),
        action
    );
}

#[test]
fn gallery_external_codec_round_trips_every_operation() {
    let still_id = GalleryStillId::new();
    let definition_id = GradeDefinitionId::new();
    for action in [
        GalleryProductAction::CaptureStill(Box::new(GalleryCaptureStillPayload {
            name: "Reference".to_owned(),
            presentation_fingerprint: [7; 32],
            raster: raster(),
            statistics: stats(0.1, 0.4, 0.8),
        })),
        GalleryProductAction::RenameStill(GalleryRenameStillPayload {
            still_id,
            name: "Hero".to_owned(),
        }),
        GalleryProductAction::RemoveStill(GalleryStillTargetPayload { still_id }),
        GalleryProductAction::SetComparison(GallerySetComparisonPayload {
            still_id: Some(still_id),
            layout: GalleryComparisonLayout::WipeVertical { position: 0.35 },
        }),
        GalleryProductAction::ApplyShotMatch(Box::new(GalleryApplyShotMatchPayload {
            still_id,
            definition_id,
            target_statistics: stats(0.0, 0.2, 0.4),
            version_name: "Matched".to_owned(),
            activate: true,
        })),
    ] {
        round_trip(ProductAction::Gallery(action));
    }
}

#[test]
fn gallery_capture_compare_and_shot_match_are_atomic_and_undoable() {
    let fixture = FixtureRoot::new();
    let mut state = AppState::new();
    state
        .create_new_project_at(
            fixture.0.join("gallery.mdp"),
            "Gallery",
            1920,
            1080,
            Rational::new(24, 1),
        )
        .expect("create Gallery Project");
    state
        .dispatch_action(
            ProductAction::Grade(GradeProductAction::CreateDefinition(
                GradeCreateDefinitionPayload { name: "Target".to_owned(), assign_to: None },
            ))
            .into_external_action(),
        )
        .expect("create target Grade Definition");
    let definition_id = state.active_sequence().expect("Sequence").grade_definitions[0].id;
    let generation_before = state.project_author_generation();
    let history_before = state.authoring_history().expect("History").diagnostics().undo_entries;
    state
        .dispatch_action(
            ProductAction::Gallery(GalleryProductAction::CaptureStill(Box::new(
                GalleryCaptureStillPayload {
                    name: "Reference".to_owned(),
                    presentation_fingerprint: [9; 32],
                    raster: raster(),
                    statistics: stats(0.1, 0.5, 0.9),
                },
            )))
            .into_external_action(),
        )
        .expect("capture Gallery still");
    assert_eq!(state.project_author_generation(), generation_before + 1);
    assert_eq!(
        state.authoring_history().expect("History").diagnostics().undo_entries,
        history_before + 1
    );
    let still = &state.project_gallery().expect("Gallery").stills[0];
    let still_id = still.id;
    assert_eq!(still.active_grade_versions[0].definition_id, definition_id);
    assert_eq!(still.source_time, TimelineTime::ZERO);

    state.dispatch_action(Action::Undo).expect("undo capture");
    assert!(state.project_gallery().expect("Gallery").stills.is_empty());
    state.dispatch_action(Action::Redo).expect("redo capture");
    assert_eq!(
        state.project_gallery().expect("Gallery").stills[0].id,
        still_id
    );

    state
        .dispatch_action(
            ProductAction::Gallery(GalleryProductAction::SetComparison(
                GallerySetComparisonPayload {
                    still_id: Some(still_id),
                    layout: GalleryComparisonLayout::SplitVertical,
                },
            ))
            .into_external_action(),
        )
        .expect("enable split comparison");
    let viewer = ViewerPanelModel::from_app_state(&state);
    let reference = viewer.comparison_reference.expect("Viewer comparison reference");
    assert_eq!(reference.frame.width, 2);
    assert_eq!(reference.frame.height, 1);

    state
        .dispatch_action(
            ProductAction::Gallery(GalleryProductAction::ApplyShotMatch(Box::new(
                GalleryApplyShotMatchPayload {
                    still_id,
                    definition_id,
                    target_statistics: stats(0.0, 0.25, 0.5),
                    version_name: "Matched".to_owned(),
                    activate: true,
                },
            )))
            .into_external_action(),
        )
        .expect("apply Shot Match");
    let definition = state
        .active_sequence()
        .expect("Sequence")
        .grade_definition(definition_id)
        .expect("definition");
    assert_eq!(definition.versions.len(), 2);
    let matched = definition.active().expect("matched active version");
    let GradeVersionOrigin::ShotMatch { evidence } = &matched.origin else {
        panic!("Shot Match version retains evidence");
    };
    assert_eq!(evidence.reference_still_id, still_id);
    for gain in evidence.gain_rgb {
        assert!((gain - 1.6).abs() < 1.0e-6);
    }
    let effect = match &matched.graph.nodes.last().expect("Shot Match node").kind {
        mondrian_core::GradeGraphNodeKind::Effect { effect, .. } => effect,
        other => panic!("expected Effect node, got {other:?}"),
    };
    let gain_parameter = EffectType::ColorWheel.parameter_id("gain").expect("Gain parameter");
    let authored_gain =
        effect.evaluate_vec3_parameter(&gain_parameter, TimelineTime::ZERO, glam::Vec3::ZERO);
    assert!((authored_gain - glam::Vec3::splat(1.6)).abs().max_element() < 1.0e-6);

    state
        .dispatch_action(
            ProductAction::Gallery(GalleryProductAction::RenameStill(
                GalleryRenameStillPayload { still_id, name: "Hero".to_owned() },
            ))
            .into_external_action(),
        )
        .expect("rename still");
    assert_eq!(
        state.project_gallery().expect("Gallery").stills[0].name,
        "Hero"
    );
    state
        .dispatch_action(
            ProductAction::Gallery(GalleryProductAction::RemoveStill(
                GalleryStillTargetPayload { still_id },
            ))
            .into_external_action(),
        )
        .expect("remove still");
    assert!(state.project_gallery().expect("Gallery").stills.is_empty());
    assert!(ViewerPanelModel::from_app_state(&state).comparison_reference.is_none());

    state.close_project().expect("close Gallery Project");
}
