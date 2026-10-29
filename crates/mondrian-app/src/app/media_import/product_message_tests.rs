use super::*;
use crate::app_ui::localization::AppUiLocale;
use crate::app_ui::localization::Localizer;
use execution::MediaImportAdmissionError;

#[test]
fn media_import_admission_messages_cover_every_variant_and_locale() {
    let errors = [
        MediaImportAdmissionError::ProjectUnavailable,
        MediaImportAdmissionError::WorkerUnavailable,
        MediaImportAdmissionError::EmptyBatch,
        MediaImportAdmissionError::BatchCapacityExceeded { capacity: 16 },
        MediaImportAdmissionError::BatchTooLarge { files: 513, capacity: 512 },
        MediaImportAdmissionError::FileCapacityExceeded { files: usize::MAX, available: 0 },
    ];
    for error in errors {
        let reason = media_import_admission_message(&error);
        let message =
            AppMessage::new("status-import-failed").with_message("reason", reason.clone());
        let retained = message.clone();
        for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            let inner = localizer.message(&reason);
            let rendered = localizer.message(&message);
            assert_ne!(inner, reason.id);
            assert!(rendered.contains(&inner), "{rendered}");
            assert!(!rendered.contains("{$"), "{rendered}");
            if matches!(
                error,
                MediaImportAdmissionError::FileCapacityExceeded { .. }
            ) {
                assert!(rendered.contains(&usize::MAX.to_string()), "{rendered}");
            }
        }
        assert_eq!(message, retained);
    }
}

#[test]
fn media_import_missing_library_retains_localized_reason_without_admission() {
    let mut state = AppState::new();
    state.test_set_asset_library(None);
    let generation = state.project_author_generation();
    let before = state.media_import_diagnostics();
    assert!(state.start_media_import_batch(vec![PathBuf::from("clip.mov")], None).is_err());
    let (message, is_error) = state.status_hint().expect("specific status");
    assert!(*is_error);
    let english = Localizer::new(AppUiLocale::EnUs).expect("English").message(message);
    assert!(
        english.contains("The project asset library is not connected"),
        "{english}"
    );
    assert_eq!(state.project_author_generation(), generation);
    assert_eq!(
        state.media_import_diagnostics().admissions,
        before.admissions
    );
    assert_eq!(state.media_import_diagnostics().outstanding_files, 0);
}

#[test]
fn media_import_oversized_batch_rejects_without_author_or_library_mutation() {
    let mut state = AppState::new();
    state.test_set_sequence(Some(mondrian_timeline::Sequence::new("import")));
    state.media_import.bind_project(state.project_id());
    let generation = state.project_author_generation();
    let assets = state.asset_library().expect("library").list_assets().expect("assets");
    let paths = (0..513).map(|i| PathBuf::from(format!("clip-{i}.mov"))).collect();
    assert!(state.start_media_import_batch(paths, None).is_err());
    let (message, is_error) = state.status_hint().expect("specific status");
    assert!(*is_error);
    let english = Localizer::new(AppUiLocale::EnUs).expect("English").message(message);
    assert!(
        english.contains("513") && english.contains("512"),
        "{english}"
    );
    assert!(!english.contains("媒体"), "{english}");
    let diagnostics = state.media_import_diagnostics();
    assert_eq!(diagnostics.rejections, 1);
    assert_eq!(diagnostics.admissions, 0);
    assert_eq!(diagnostics.outstanding_files, 0);
    assert_eq!(state.project_author_generation(), generation);
    assert_eq!(
        state.asset_library().expect("library").list_assets().expect("assets").len(),
        assets.len()
    );
}

#[test]
fn media_import_missing_folder_keeps_opaque_identifier_in_every_locale() {
    let mut state = AppState::new();
    state.test_set_sequence(Some(mondrian_timeline::Sequence::new("import")));
    let folder = "不存在-{folder}-9007199254740993";
    let generation = state.project_author_generation();
    assert!(state
        .start_media_import_batch(vec![PathBuf::from("clip.mov")], Some(folder.to_owned()))
        .is_err());
    let (message, is_error) = state.status_hint().expect("specific status");
    assert!(*is_error);
    for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
        let rendered = Localizer::new(locale).expect("catalog").message(message);
        assert!(rendered.contains(folder), "{rendered}");
    }
    assert_eq!(state.project_author_generation(), generation);
    assert_eq!(state.media_import_diagnostics().admissions, 0);
}

#[test]
fn timeline_file_admission_retains_specific_localized_failure_and_no_partial_import() {
    use crate::app::product_action::TimelineDropFilePayload;
    use mondrian_core::FramePosition;
    let mut state = AppState::new();
    let sequence = mondrian_timeline::Sequence::new("drop");
    let payload = TimelineDropFilePayload {
        path: PathBuf::from("external.mov"),
        target_track_id: sequence.video_tracks[0].id,
        position: FramePosition::new(0, sequence.time_base()),
    };
    state.test_set_sequence(Some(sequence));
    state.media_import.bind_project(None);
    assert!(state.can_queue_file_on_timeline(&payload));
    let generation = state.project_author_generation();
    let library = state.asset_library_handle().expect("library");
    let revision = library.database_revision().expect("revision");
    assert!(state.queue_file_on_timeline(payload).is_err());
    let (message, is_error) = state.status_hint().expect("specific status");
    assert!(*is_error);
    assert_eq!(message.id, "status-drop-failed");
    let english = Localizer::new(AppUiLocale::EnUs).expect("English").message(message);
    assert!(
        english.contains("Open a project before importing media"),
        "{english}"
    );
    assert!(state.pending_timeline_file_drops.is_empty());
    assert_eq!(state.media_import_diagnostics().admissions, 0);
    assert_eq!(state.media_import_diagnostics().outstanding_files, 0);
    assert_eq!(state.project_author_generation(), generation);
    assert_eq!(library.database_revision().expect("revision"), revision);
    assert!(library.list_assets().expect("assets").is_empty());
    assert!(state.active_sequence().expect("sequence").video_tracks[0].clips.is_empty());
}
