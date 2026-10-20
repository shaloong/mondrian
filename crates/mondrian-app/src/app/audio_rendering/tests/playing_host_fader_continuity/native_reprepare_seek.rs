//! Actual Required-to-quiescence seeks on one retained owned CLAP mode7 Source.
//! Software-output qualification; no claim for every native service phase or DAC.

#[test]
#[ignore = "current normal Product and owned CLAP mode7; public seeks before actual output ack, original producer/retirement queue and software PCM"]
fn native_clap_seek_during_restart_preserves_same_source_and_latest_pdc_pcm() {
    super::native_reprepare::run_native_restart_control(&[0, 1, 2]);
}
