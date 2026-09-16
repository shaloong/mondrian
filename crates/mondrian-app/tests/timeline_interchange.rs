use mondrian_app::app::AppState;
use mondrian_core::{AuthoringList, Rational, TimelineTime};
use mondrian_interchange::{
    inspect_import, prepare_export, InterchangeAssetSnapshot, InterchangeExportRequest,
    InterchangeFormatProfile, InterchangeImportRequest, InterchangeLimits, InterchangeLossPolicy,
    InterchangeMediaBinding,
};
use mondrian_timeline::{AudioProgram, Clip, Sequence, Track};

#[test]
fn imported_timeline_is_one_reversible_project_transaction() {
    let root = tempfile::tempdir().expect("temp project root");
    let project_path = root.path().join("interchange.mdp");
    let mut app = AppState::new();
    app.create_new_project_at(project_path, "Interchange", 1920, 1080, Rational::FPS_25)
        .expect("create Project");
    let asset_id = app
        .asset_library()
        .expect("Asset Library")
        .create_solid_color_asset(Some("Interchange binding fixture"))
        .expect("fixture Asset");

    let mut foreign = Sequence::new("Imported OTIO");
    foreign.settings.frame_rate = Rational::FPS_25;
    let mut track = Track::new_video("V1");
    track
        .add_clip(
            Clip::new(
                asset_id,
                TimelineTime::ZERO,
                TimelineTime::new(50, 25).expect("duration"),
            )
            .expect("Clip"),
        )
        .expect("add Clip");
    foreign.video_tracks = AuthoringList::from(vec![track]);
    foreign.audio_tracks = AuthoringList::new();
    foreign.audio_program = AudioProgram::for_tracks([]);
    let artifact = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        sequence: &foreign,
        assets: &[InterchangeAssetSnapshot {
            asset_id,
            name: "fixture.mov".to_owned(),
            locator: Some("file:///fixture.mov".to_owned()),
            editorial_source: None,
            color_space: None,
        }],
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect("prepare OTIO");
    let prepared = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        bytes: artifact.bytes().to_vec(),
        explicit_frame_rate: None,
        limits: InterchangeLimits::default(),
    })
    .expect("inspect OTIO");
    let generation_before = app.project_author_generation();
    let history_before = app.authoring_history().expect("history").diagnostics().undo_entries;
    let report = app
        .import_prepared_timeline_interchange(
            &prepared,
            &[InterchangeMediaBinding {
                key: prepared.media_references()[0].key.clone(),
                asset_id,
            }],
            InterchangeLossPolicy::AllowWithReport,
        )
        .expect("single import transaction");

    assert!(!report.has_blockers());
    assert_eq!(app.project_author_generation(), generation_before + 1);
    assert_eq!(
        app.authoring_history().expect("history").diagnostics().undo_entries,
        history_before + 1
    );
    assert_eq!(app.sequences().len(), 2);
    assert!(app.sequences().iter().any(|sequence| sequence.name == "Imported OTIO"));
    assert_eq!(app.active_sequence().expect("active").name, "Interchange");

    assert!(app.undo_timeline().expect("undo"));
    assert_eq!(app.sequences().len(), 1);
    assert!(app.redo_timeline().expect("redo"));
    assert_eq!(app.sequences().len(), 2);

    let export = app
        .prepare_active_timeline_interchange(
            InterchangeFormatProfile::OtioJsonV1,
            InterchangeLossPolicy::AllowWithReport,
            InterchangeLimits::default(),
        )
        .expect("App-owned immutable export snapshot");
    assert!(!export.bytes().is_empty());
    drop(app);
}
