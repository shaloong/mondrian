//! Product-entrypoint contract for the isolated media Probe Helper.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mondrian_core::ExecutionCancellationToken;

#[test]
fn product_executable_runs_the_versioned_media_probe_helper() {
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_mondrian"));
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/small/h264-bframes.mp4");

    let prepared = mondrian_media::prepare_media_probe_isolated(
        &helper,
        &source,
        &ExecutionCancellationToken::new(),
        Instant::now() + Duration::from_secs(30),
    )
    .expect("packaged product helper must prepare the committed small fixture");

    assert!(prepared.canonical_path.is_absolute());
    assert!(prepared.source_fingerprint.authorizes_reuse());
    assert!(prepared.probe.has_video);
    assert!(!prepared.probe.video_streams.is_empty());
}
