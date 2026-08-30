use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mondrian_app::app::product_action::{
    ProductAction, DYNAMIC_HDR_APPLY_EDIT, DYNAMIC_HDR_NAMESPACE,
};
use mondrian_app::app::AppState;
use mondrian_app::app_ui::panels::ExportPanelModel;
use mondrian_core::{
    DynamicHdrMetadataFamily, DynamicHdrShotMetadata, DynamicHdrStandard, Rational,
    St2094Application4ShotMetadata, St2094DistributionPoint, TimelineTime, TimelineTimeRange,
};
use mondrian_editor_state::Action;
use mondrian_timeline::{
    DynamicHdrAnalysisProvenance, DynamicHdrAuthorEdit, DynamicHdrDeliveryIntent,
    DynamicHdrProgram, DynamicHdrShot,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mondrian-dynamic-hdr-authoring-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create Dynamic HDR fixture root");
        Self(path)
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn analyzed_program() -> DynamicHdrProgram {
    let range = TimelineTimeRange::new(
        TimelineTime::ZERO,
        TimelineTime::new(1, 24).expect("one frame"),
    )
    .expect("shot range");
    DynamicHdrProgram::new(
        "Master analysis",
        DynamicHdrStandard::St2094_40Application4 { application_version: 0 },
        DynamicHdrAnalysisProvenance {
            adapter_id: "fixture-analyzer".to_owned(),
            adapter_version: "1.0".to_owned(),
            metadata_schema: "st2094-40:2020".to_owned(),
            visual_author_fingerprint: [3; 32],
            canonical_metadata_sha256: [4; 32],
        },
        [DynamicHdrShot::new(
            "Shot 1",
            range,
            DynamicHdrShotMetadata::St2094_40Application4(St2094Application4ShotMetadata {
                targeted_system_display_maximum_luminance: 1_000,
                max_scl: [10_000, 9_000, 8_000],
                average_max_rgb: 1_500,
                distribution: vec![
                    St2094DistributionPoint { percentile: 50, value: 1_000 },
                    St2094DistributionPoint { percentile: 99, value: 9_500 },
                ],
                fraction_bright_pixels: 50,
                tone_mapping: None,
                color_saturation_weight: None,
            }),
        )],
    )
}

fn round_trip(edit: DynamicHdrAuthorEdit) {
    let action = ProductAction::DynamicHdr(edit);
    let external = action.clone().into_external_action();
    let Action::Custom { namespace, name, .. } = &external else {
        panic!("Dynamic HDR product action must use the external codec");
    };
    assert_eq!(namespace, DYNAMIC_HDR_NAMESPACE);
    assert_eq!(name, DYNAMIC_HDR_APPLY_EDIT);
    assert_eq!(
        ProductAction::decode_external(&external)
            .expect("decode Dynamic HDR action")
            .expect("recognized Dynamic HDR namespace"),
        action
    );
}

#[test]
fn dynamic_hdr_external_codec_round_trips_every_operation() {
    let program = analyzed_program();
    let program_id = program.id;
    for edit in [
        DynamicHdrAuthorEdit::SetDeliveryIntent {
            intent: DynamicHdrDeliveryIntent::PreserveSourceExact {
                family: DynamicHdrMetadataFamily::St2094_40Application4,
            },
        },
        DynamicHdrAuthorEdit::InstallAnalyzedProgram { program },
        DynamicHdrAuthorEdit::SetDeliveryIntent {
            intent: DynamicHdrDeliveryIntent::Remake { program_id },
        },
        DynamicHdrAuthorEdit::RemoveProgram { program_id },
    ] {
        round_trip(edit);
    }
}

#[test]
fn dynamic_hdr_authoring_is_atomic_undoable_and_projected_without_brand_claims() {
    let fixture = FixtureRoot::new();
    let mut state = AppState::new();
    state
        .create_new_project_at(
            fixture.0.join("dynamic-hdr.mdp"),
            "Dynamic HDR",
            1920,
            1080,
            Rational::new(24, 1),
        )
        .expect("create Dynamic HDR project");

    let initial = ExportPanelModel::from_app_state(&state);
    assert_eq!(initial.dynamic_hdr.intent_label, "省略 Dynamic HDR");
    assert_eq!(initial.dynamic_hdr.program_count, 0);
    assert!(initial.dynamic_hdr.intent_actions.iter().any(|item| {
        item.label.contains("ST 2094-40")
            && !item.label.contains("HDR10+")
            && ProductAction::decode_external(&item.action)
                .ok()
                .flatten()
                .is_some_and(|action| {
                    action
                        == ProductAction::DynamicHdr(DynamicHdrAuthorEdit::SetDeliveryIntent {
                            intent: DynamicHdrDeliveryIntent::PreserveSourceExact {
                                family: DynamicHdrMetadataFamily::St2094_40Application4,
                            },
                        })
                })
    }));

    let program = analyzed_program();
    let program_id = program.id;
    let generation_before = state.project_author_generation();
    let revision_before = state.active_sequence().expect("Sequence").revision;
    let history_before = state.authoring_history().expect("History").diagnostics().undo_entries;
    state
        .dispatch_action(
            ProductAction::DynamicHdr(DynamicHdrAuthorEdit::InstallAnalyzedProgram { program })
                .into_external_action(),
        )
        .expect("install analyzed Program");
    assert_eq!(state.project_author_generation(), generation_before + 1);
    assert_eq!(
        state.active_sequence().expect("Sequence").revision.get(),
        revision_before.get() + 1
    );
    assert_eq!(
        state.authoring_history().expect("History").diagnostics().undo_entries,
        history_before + 1,
        "one Dynamic HDR edit must produce one author transaction"
    );

    let remake = ProductAction::DynamicHdr(DynamicHdrAuthorEdit::SetDeliveryIntent {
        intent: DynamicHdrDeliveryIntent::Remake { program_id },
    });
    assert!(state.product_action_availability().allows(&remake));
    state
        .dispatch_action(remake.into_external_action())
        .expect("select Remake Program");
    let model = ExportPanelModel::from_app_state(&state);
    assert_eq!(model.dynamic_hdr.program_count, 1);
    assert_eq!(model.dynamic_hdr.intent_label, "重制：Master analysis");
    assert!(model.dynamic_hdr.readiness.contains("合格且已授权"));
    assert!(!model.dynamic_hdr.readiness.contains("已认证"));

    let remove = ProductAction::DynamicHdr(DynamicHdrAuthorEdit::RemoveProgram { program_id });
    assert!(!state.product_action_availability().allows(&remove));
    assert!(state.dispatch_action(remove.into_external_action()).is_err());

    state.dispatch_action(Action::Undo).expect("undo Remake intent");
    assert!(matches!(
        state.active_sequence().expect("Sequence").dynamic_hdr.delivery_intent(),
        DynamicHdrDeliveryIntent::Omit
    ));
    state.dispatch_action(Action::Redo).expect("redo Remake intent");
    assert!(matches!(
        state.active_sequence().expect("Sequence").dynamic_hdr.delivery_intent(),
        DynamicHdrDeliveryIntent::Remake { program_id: id } if *id == program_id
    ));

    state.close_project().expect("close Dynamic HDR project");
}
