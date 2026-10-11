use mondrian_core::{AssetId, Rational};
use mondrian_interchange::*;

fn inspect(text: &[u8]) -> PreparedInterchangeImport {
    inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::Cmx3600,
        bytes: text.to_vec(),
        explicit_frame_rate: Some(Rational::FPS_25),
        limits: InterchangeLimits::default(),
    })
    .expect("inspect CMX")
}

#[test]
fn names_are_event_local_and_round_trip_without_changing_conform() {
    let input = "TITLE: Names\nFCM: NON-DROP FRAME\n\
* FROM CLIP NAME: orphan\n\
001 A001 V C 10:00:00:00 10:00:02:00 01:00:00:00 01:00:02:00\n\
* FROM CLIP NAME: 上海 wide shot.mov\n\
* unrelated comment\n\
002 A001 V D 010 10:00:04:00 10:00:06:00 01:00:02:00 01:00:04:00\n\
* FROM CLIP NAME: Close: take 2\n\
003 A001 V C 10:00:08:00 10:00:09:00 01:00:04:00 01:00:05:00\n\
* FROM CLIP NAME:   \n";
    let prepared = inspect(input.as_bytes());
    assert_eq!(prepared.media_references().len(), 1);
    assert_eq!(prepared.media_references()[0].key.0, "reel:A001");
    assert_eq!(prepared.media_references()[0].name.as_deref(), Some("A001"));
    let asset_id = AssetId::new();
    let bindings = [InterchangeMediaBinding {
        key: prepared.media_references()[0].key.clone(),
        asset_id,
    }];
    let candidate =
        materialize_import(&prepared, &bindings, InterchangeLossPolicy::AllowWithReport)
            .expect("materialize");
    let clips = &candidate.sequence.video_tracks[0].clips;
    assert_eq!(clips[0].label.as_deref(), Some("上海 wide shot.mov"));
    assert_eq!(clips[1].label.as_deref(), Some("Close: take 2"));
    assert_eq!(clips[2].label.as_deref(), Some("A001"));
    let assets = [InterchangeAssetSnapshot {
        asset_id,
        name: "A001".to_owned(),
        locator: None,
        editorial_source: prepared.media_references()[0].editorial_source.clone(),
        color_space: None,
    }];
    let artifact = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::Cmx3600,
        sequence: &candidate.sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect("export");
    let again = inspect(artifact.bytes());
    assert_eq!(again.media_references(), prepared.media_references());
    let round_trip = materialize_import(&again, &bindings, InterchangeLossPolicy::AllowWithReport)
        .expect("round trip");
    // Generated entity IDs differ on materialization; compare the conform through re-export.
    assert_eq!(
        candidate.sequence.settings.timeline_display,
        round_trip.sequence.settings.timeline_display
    );
    let round_artifact = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::Cmx3600,
        sequence: &round_trip.sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect("re-export");
    assert_eq!(artifact.bytes(), round_artifact.bytes());
    let event_lines = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with(['0', '1', '2', '3', '4', '5', '6', '7', '8', '9']))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        event_lines(input),
        event_lines(std::str::from_utf8(artifact.bytes()).expect("UTF-8"))
    );
}
