//! Actual DLL transport observations plus an independent canonical PCM oracle.
use super::*;
use crate::{
    AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioDecodedSource, AudioExecutionError,
    AudioMediaResolver, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioProgramRuntime, AudioRuntimeResourceGrant, AudioSessionCursorEvidence, AudioStateEntry,
    ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioOwnerInstallationSource, AudioSourceComponentId, ExactAutomationCurve,
    ExactAutomationKeyframe, ExecutionCancellationToken, MusicalTimeReference,
    MusicalTimeSignature, TimeScale, TimelineTime, TimelineTransportActivity,
};
use mondrian_timeline::{Clip, Sequence};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

#[test]
#[ignore = "current IPC22 Product and rebuilt CLAP sidechain mode16; actual meter DSP/PCM/Scope/retime/PDC"]
fn installed_clap_canonical_meter_reaches_dsp_and_pcm() {
    canonical_meter(true);
}

#[test]
#[ignore = "current IPC22 Product and rebuilt VST3 sidechain mode9; actual meter DSP/PCM/Scope/retime/PDC"]
fn installed_vst3_canonical_meter_reaches_dsp_and_pcm() {
    canonical_meter(false);
}

struct IndexedMedia;
impl AudioDecodedSource for IndexedMedia {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        destination: &mut [f32],
        _: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if destination.len() != frames * 2 {
            return Err("stereo extent".into());
        }
        for (offset, pair) in destination.chunks_exact_mut(2).enumerate() {
            let sample = start.checked_add(offset as i64).ok_or("source extent")?;
            let value = if sample < 0 {
                0.0
            } else {
                ((sample % 31) - 15) as f32 / 1024.0
            };
            pair.copy_from_slice(&[value, -0.5 * value]);
        }
        Ok(())
    }
}

struct Media(AtomicUsize);
impl AudioMediaResolver for Media {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(IndexedMedia),
        ))
    }
}

#[derive(Default)]
struct Cursors(
    Vec<AudioSessionCursorEvidence>,
    Vec<AudioProcessorCursorEvidence>,
);
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, value: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.0.push(value);
        Ok(())
    }
    fn processor(
        &mut self,
        value: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.1.push(value);
        Ok(())
    }
}

fn owners(runtime: &AudioProgramRuntime) -> Vec<crate::AudioNativeProcessorOwnerLease> {
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).unwrap();
    assert_eq!((cursors.0.len(), cursors.1.len()), (1, 2));
    cursors
        .1
        .iter()
        .map(|processor| {
            let epoch = match processor.state {
                AudioProcessorCursorState::Unentered => None,
                AudioProcessorCursorState::WaitingForFirstSignal { epoch }
                | AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
                state => panic!("unexpected native state {state:?}"),
            };
            runtime
                .native_owner_lease(crate::AudioNativeProcessorAddress {
                    root: cursors.0[0].identity,
                    local: processor.identity,
                    occurrence: processor.occurrence,
                    epoch,
                })
                .unwrap()
        })
        .collect()
}

fn canonical_meter(clap: bool) {
    let native_path =
        |name| PathBuf::from(std::env::var_os(name).expect("current native meter input"));
    let clap_catalog = clap.then(|| {
        Arc::new(
            DiscoveredClapAudioProcessorSpecResolver::discover(
                native_path("MONDRIAN_CLAP_TEST_HELPER"),
                [native_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
            )
            .unwrap(),
        )
    });
    let vst_catalog = (!clap).then(|| {
        Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(
                native_path("MONDRIAN_VST3_TEST_HELPER"),
                [native_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")],
            )
            .unwrap(),
        )
    });
    let specs: Arc<dyn IsolatedAudioProcessorSpecResolver> = if let Some(catalog) = &clap_catalog {
        catalog.clone()
    } else {
        vst_catalog.as_ref().unwrap().clone()
    };
    let processors = IsolatedAudioProcessorResolver::new(specs);
    for rate in [44_100_u32, 48_000] {
        let time = |sample| TimelineTime::new(sample, i64::from(rate)).unwrap();
        let boundary_floor = 2 + 21 * i64::from(rate) / 8;
        for activity in [
            None,
            Some(TimelineTransportActivity::Stopped),
            Some(TimelineTransportActivity::Playing),
        ] {
            let render = AudioRenderContract {
                sample_rate: rate,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
                ..test_render_contract()
            };
            let mut sequence = Sequence::new("actual native constant meter Scope");
            sequence.audio_program.musical_time_reference = Some(
                MusicalTimeReference::new(time(2), TimelineTime::new(3, 4).unwrap())
                    .unwrap()
                    .with_time_signature(Some(MusicalTimeSignature::new(7, 8).unwrap())),
            );
            let mut clip = Clip::new(AssetId::new(), time(0), time(boundary_floor + 64)).unwrap();
            clip.set_constant_source_time_map(time(5), TimeScale::new(3, 2).unwrap())
                .unwrap();
            sequence
                .add_media_audio_clip(
                    sequence.audio_tracks[0].id,
                    clip,
                    AudioSourceComponentId::primary(),
                )
                .unwrap();
            sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_in =
                TimelineTime::new(5, 1).unwrap();
            let scope = &mut sequence.audio_program.processing_scopes[0];
            let mut gain = ExactAutomationCurve::new(
                ParameterId::new_static(mondrian_timeline::audio::INPUT_GAIN_DB_PARAMETER_ID),
                -6.0,
            )
            .unwrap();
            gain.set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(5, 1).unwrap(),
                -6.0,
            ))
            .unwrap();
            gain.set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(5, 1).unwrap().checked_add(time(64)).unwrap(),
                0.0,
            ))
            .unwrap();
            scope.input_gain_automation = Some(gain);
            for mode in [0, if clap { 16 } else { 9 }] {
                let mut instance = if let Some(catalog) = &clap_catalog {
                    let mut state = vec![mode as u8];
                    state.extend_from_slice(&0.5_f32.to_le_bytes());
                    catalog
                        .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
                        .unwrap()
                } else {
                    vst_catalog
                        .as_ref()
                        .unwrap()
                        .create_instance(
                            "7486AF22234A480B9DF13173F12EB620",
                            render,
                            Some(vst3_sidechain_snapshot(0.5, mode)),
                        )
                        .unwrap()
                };
                instance.parameters.clear();
                scope.processors.processors.push(instance);
            }
            let author = sequence.clone();
            let media = Media(AtomicUsize::new(0));
            let mut runtime =
                AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                    &sequence,
                    &[],
                    &media,
                    &processors,
                    render,
                    None,
                    AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                )
                .unwrap();
            assert_eq!(runtime.public_output_lookahead_frames(), 6);
            assert_eq!(media.0.load(Ordering::SeqCst), 1);
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().unwrap();
            runtime.bind_native_owner_installation(verifier).unwrap();
            let claim = source
                .reserve_install(&registration, None)
                .and_then(|r| source.begin_commit(r))
                .unwrap()
                .commit()
                .unwrap();
            runtime
                .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
                .unwrap();
            let cold: Vec<_> = owners(&runtime)
                .into_iter()
                .map(|lease| runtime.observe_native_owner_object(lease).unwrap())
                .collect();
            let handles: Vec<_> = cold
                .iter()
                .map(|object| {
                    assert_ne!(object.process_id().get(), std::process::id());
                    // SAFETY: observed current child PID; retain only a read-only exit handle.
                    let handle =
                        unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
                    assert!(!handle.is_null());
                    unsafe { OwnedHandle::from_raw_handle(handle.cast()) }
                })
                .collect();
            let mut previous = cold.clone();
            for (epoch, origin, partitions) in [
                (1, 0, &[4_usize, 4, 4, 2][..]),
                (2, boundary_floor - 2, &[3, 2, 1, 4, 4][..]),
            ] {
                runtime
                    .enter_state(AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(epoch),
                        start_sample: origin,
                    })
                    .unwrap();
                let mut position = origin;
                let mut entered = None;
                for &frames in partitions {
                    let mut pcm = vec![99.0; frames * 2];
                    runtime
                        .render_into(
                            AudioRenderRequest {
                                transport_activity: activity,
                                start_sample: position,
                                frames,
                            },
                            &mut pcm,
                        )
                        .unwrap();
                    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
                        let sample = position + offset as i64;
                        let indexed = 5 + (3 * sample).div_euclid(2);
                        let input = ((indexed % 31) - 15) as f32 / 1024.0;
                        let db = if sample >= 64 {
                            0.0
                        } else {
                            -6.0 + 6.0 * sample as f64 / 64.0
                        };
                        let scope_input = input * 10.0_f64.powf(db / 20.0) as f32;
                        // Independent threshold oracle, never the DLL helper or host conversion.
                        let meter_tag = if sample < 2 {
                            0.0_f32
                        } else if 8 * (sample - 2) < 21 * i64::from(rate) {
                            1.0 / 16.0
                        } else {
                            1.0 / 8.0
                        };
                        let native_gain = 0.5_f32
                            + (sample % 251) as f32 / 1024.0
                            + 80.0 / 1024.0
                            + meter_tag
                            + if activity == Some(TimelineTransportActivity::Playing) {
                                0.125
                            } else {
                                0.0
                            };
                        let expected = (scope_input * 2.0) * native_gain;
                        assert!((pair[0] - expected).abs() <= 2.0e-6,
                            "clap={clap} rate={rate} epoch={epoch} sample={sample} actual={} expected={expected}", pair[0]);
                        assert!((pair[1] + expected * 0.5).abs() <= 1.0e-6);
                    }
                    let actual: Vec<_> = owners(&runtime)
                        .into_iter()
                        .map(|lease| runtime.observe_native_owner_object(lease).unwrap())
                        .collect();
                    if let Some(first) = &entered {
                        assert_eq!(&actual, first);
                    } else {
                        for (fresh, old) in actual.iter().zip(&previous) {
                            assert_eq!(fresh.process_id(), old.process_id());
                            if clap || epoch == 1 {
                                assert_eq!(fresh, old);
                            } else {
                                assert!(fresh.object_serial() > old.object_serial());
                            }
                        }
                        entered = Some(actual);
                    }
                    assert_eq!(sequence, author);
                    position += frames as i64;
                }
                previous = entered.unwrap();
            }
            runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
            drop(runtime);
            for handle in handles {
                // Same-child OS exit does not prove a successful 100ms graceful result.
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2000) },
                    0
                );
                let mut code = u32::MAX;
                assert_ne!(
                    unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut code) },
                    0
                );
                assert_eq!(code, 0);
            }
            println!(
                "{}",
                serde_json::json!({"case":"canonical-native-meter","clap":clap,"rate":rate,
                "activity":activity,"meter":"7/8","epochs":2,"pdc":6,"source_speed":"3/2",
                "scope_in_seconds":5,"native_meter_and_pcm":true,"rt_qualified":false})
            );
        }
    }
}
