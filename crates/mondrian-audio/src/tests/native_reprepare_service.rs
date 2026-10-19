//! Audio-only retained capture. No Media/output-quiescence or full C07 qualification.
use super::*;
use mondrian_core::{AudioOwnerInstallationSource, AudioRenderSessionId};
use std::path::PathBuf;

#[derive(Default, Debug, PartialEq, Eq)]
struct Cursors {
    session: Option<AudioSessionCursorEvidence>,
    processor: Option<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, value: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        assert!(self.session.replace(value).is_none(), "single Root");
        Ok(())
    }
    fn processor(
        &mut self,
        value: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        assert!(
            self.processor.replace(value).is_none(),
            "single native occurrence"
        );
        Ok(())
    }
}
fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
    let mut result = Cursors::default();
    runtime.visit_continuity_evidence(&mut result).expect("actual cursor evidence");
    result
}
fn path(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).expect(name));
    assert!(path.is_absolute() && path.is_file(), "{name}");
    path
}

#[test]
#[ignore = "current production helper and owned CLAP mode7; actual Runtime/Source retained capture, not recovery"]
fn installed_clap_runtime_restart_capture_retains_exact_source_and_ticket() {
    retained_restart_control(Preparation::CaptureOnly);
}

#[test]
#[ignore = "current production helper and owned CLAP mode7; execution-only Factory/PDC/empty Session preparation, not recovery"]
fn installed_clap_runtime_restart_prepares_pdc_without_publication_or_entry() {
    retained_restart_control(Preparation::Ready);
}

#[test]
#[ignore = "current production helper and owned CLAP mode7; rejected PDC candidate must preserve original held Runtime"]
fn installed_clap_runtime_restart_pdc_refusal_keeps_capture_and_old_source_held() {
    retained_restart_control(Preparation::RefusePdc);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Preparation {
    CaptureOnly,
    Ready,
    RefusePdc,
}

fn retained_restart_control(preparation: Preparation) {
    let prepare = preparation != Preparation::CaptureOnly;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    };
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            path("MONDRIAN_CLAP_TEST_HELPER"),
            [path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")],
        )
        .expect("real mode7 catalog"),
    );
    let mut render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: if preparation == Preparation::RefusePdc {
            3
        } else {
            1024
        },
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let mut state = vec![7];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    state.push(0);
    let instance = catalog
        .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
        .expect("actual old activation profile");
    assert!(instance.parameters.is_empty());
    let mut sequence = sequence_with_audio_clip();
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.settings.audio_sample_rate = render.sample_rate;
    let occurrence = AudioProcessorOccurrence {
        instance_id: instance.id,
        owner: AudioProcessorOccurrenceOwner::Output(sequence.audio_program.outputs[0].id),
        insertion: AudioProcessorInsertionPoint::PreFader,
    };
    let spec = catalog
        .resolve(AudioProcessorPrepareRequest::new(
            occurrence,
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            render,
        ))
        .expect("actual unchanged ports/profile");
    // This no-Note catalog resolve already carries NativeProbed evidence.
    // Consume that same Spec without another validation worker or native probe.
    let metadata_factory = crate::processor_isolation::IsolatedAudioProcessorFactory::prepare(spec)
        .expect("existing native-probed preparation metadata");
    let execution_contract =
        crate::AudioProcessorFactory::execution_contract(metadata_factory.as_ref());
    assert_eq!(execution_contract.algorithmic_latency_frames(), 3);
    let processor_session_bytes = execution_contract.session_scratch_bytes();
    let grant = AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024);
    assert!(
        processor_session_bytes > 0 && processor_session_bytes <= grant.max_fixed_resident_bytes
    );
    // The small probe allowance does not cover the real isolated worker's
    // lane/stacks/control owners. This single-processor Runtime admits exactly
    // its existing Factory contract, under the unchanged bounded Root grant.
    render.processor_session_scratch_budget_bytes = processor_session_bytes;
    let detector =
        crate::AudioProcessorFactory::auxiliary_input_contract(metadata_factory.as_ref())
            .buses
            .first()
            .expect("real first sidechain")
            .bus_key
            .clone();
    drop(metadata_factory);
    sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
        AudioRouteSource::Track {
            track_id: sequence.audio_tracks[0].id,
            port: AudioChannelStripOutputPort::PostMute,
        },
        instance.id,
        detector,
    ));
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let author_before = serde_json::to_value(&sequence).unwrap();
    let processors = IsolatedAudioProcessorResolver::new(catalog);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &StereoRampResolver,
        &processors,
        render,
        None,
        grant,
    )
    .expect("actual original Runtime");
    assert_eq!(
        runtime.resource_footprint().processor_session_bytes,
        processor_session_bytes,
        "the sole native occurrence retains its exact Factory Session request"
    );
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier.clone()).unwrap();
    let claim = source
        .reserve_install(&registration, None)
        .and_then(|r| source.begin_commit(r))
        .unwrap()
        .commit()
        .unwrap();
    let cancellation = ExecutionCancellationToken::new();
    runtime.install_native_owner_bindings(&claim, &cancellation).unwrap();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let initial = cursors(&runtime);
    let address = AudioNativeProcessorAddress {
        root: initial.session.unwrap().identity,
        local: initial.processor.unwrap().identity,
        occurrence,
        epoch: Some(AudioContinuityEpoch::new(1)),
    };
    let lease = runtime.native_owner_lease(address).expect("actual installed lease");
    let object = runtime.observe_native_owner_object(lease).expect("authenticated same child");
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: actual Runtime object supplies PID; retain query/synchronize only.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(runtime.public_output_lookahead_frames(), 3);
        // Priming evaluates 0..3, then these public windows evaluate 3..7 and
        // 7..11. The latter completes before its real restart callback is held.
        for start in [0, 4] {
            let mut pcm = [99.0; 8];
            runtime
                .render_into(
                    AudioRenderRequest { start_sample: start, frames: 4 },
                    &mut pcm,
                )
                .expect("old complete public window including trigger");
            for (offset, pair) in pcm.chunks_exact(2).enumerate() {
                let expected = (start as usize + offset + 1) as f32 * 2.5;
                assert_eq!(
                    pair,
                    [expected, -expected],
                    "independent main + routed detector PCM"
                );
            }
        }
        let mut unpublished = [99.0; 8];
        let ticket = match runtime.render_into(
            AudioRenderRequest { start_sample: 8, frames: 4 },
            &mut unpublished,
        ) {
            Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
            other => panic!("actual mode7 pending required: {other:?}"),
        };
        assert_eq!(unpublished, [99.0; 8]);
        assert_eq!(ticket.occurrence, occurrence);
        assert_eq!(ticket.continuity_epoch, Some(AudioContinuityEpoch::new(1)));
        let NativeObservedRequest::Cohort(cohort) = ticket.request else {
            panic!("exact callback cohort");
        };
        assert_eq!(
            cohort.request,
            mondrian_core::NativeAudioReprepareRequest::ClapRestart
        );
        let held = cursors(&runtime);
        assert_eq!(
            held.session.unwrap().state,
            AudioSessionCursorState::Active {
                epoch: AudioContinuityEpoch::new(1),
                next_public_sample: mondrian_core::AudioSamplePosition::new(
                    8,
                    held.session.unwrap().sample_rate
                ),
                next_execution_sample: mondrian_core::AudioSamplePosition::new(
                    11,
                    held.session.unwrap().sample_rate
                ),
                alignment_primed: true,
            }
        );
        let original_footprint = runtime.resource_footprint();
        assert!(runtime
            .check_resource_ceiling(
                AudioRuntimeResourceCategory::FixedResidentBytes,
                grant.max_fixed_resident_bytes
            )
            .is_ok());
        let canceled = ExecutionCancellationToken::new();
        canceled.cancel();
        assert!(matches!(
            runtime.service_native_reprepare(ticket, &canceled),
            Err(NativeAudioReprepareError::Cancelled)
        ));
        let mut foreign = ticket;
        foreign.session_id = AudioRenderSessionId::new();
        assert!(matches!(
            runtime.service_native_reprepare(foreign, &cancellation),
            Err(NativeAudioReprepareError::Admission(_))
        ));
        assert_eq!(cursors(&runtime), held);
        assert_eq!(
            runtime.resource_footprint(),
            original_footprint,
            "refused preflight owns no capture"
        );
        let operation = runtime
            .service_native_reprepare(ticket, &cancellation)
            .expect("actual retained Runtime capture must succeed");
        let identity = operation.configuration_identity();
        let mut expected = vec![7];
        expected.extend_from_slice(&0.5_f32.to_le_bytes());
        expected.push(1);
        assert_eq!(
            runtime.native_reprepare_configuration(&operation).unwrap(),
            expected
        );
        assert_eq!(identity.serviced_cohort, cohort);
        assert_eq!(identity.format, NativeConfigurationFormat::ClapState);
        assert_eq!(identity.configuration_bytes, expected.len());
        assert_eq!(identity.observed_contract.algorithmic_latency_frames(), 7);
        assert_eq!(
            identity.observed_contract.tail(),
            AudioProcessorTail::Finite(7)
        );
        assert_eq!(
            operation.capture_local_peak().process_id(),
            std::process::id()
        );
        assert!(operation.capture_local_peak().allocation_bytes() >= expected.len());
        assert_eq!(
            cursors(&runtime),
            held,
            "capture does not enter/reset Runtime cursors or replace its Session"
        );
        let captured_footprint = runtime.resource_footprint();
        assert!(captured_footprint.fixed_resident_bytes > original_footprint.fixed_resident_bytes);
        assert!(captured_footprint.fixed_resident_bytes <= grant.max_fixed_resident_bytes);
        let mut unchanged_weights = captured_footprint;
        unchanged_weights.fixed_resident_bytes = original_footprint.fixed_resident_bytes;
        assert_eq!(
            unchanged_weights, original_footprint,
            "original Plan/PDC weights stay unchanged"
        );
        assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        verifier.validate(&claim).unwrap();
        let current = source.current_claim().unwrap().expect("original Source claim");
        verifier.validate(&current).unwrap();
        assert_eq!(current.generation(), claim.generation());
        assert!(matches!(
            runtime.service_native_reprepare(ticket, &cancellation),
            Err(NativeAudioReprepareError::Admission(_))
        ));
        let retained_footprint = if prepare {
            let canceled = ExecutionCancellationToken::new();
            canceled.cancel();
            assert!(matches!(
                runtime.prepare_native_reprepare_plan(&operation, verifier.clone(), &canceled),
                Err(NativeAudioReprepareError::Cancelled)
            ));
            let unrelated = AudioOwnerInstallationSource::new();
            let (_, wrong_destination) = unrelated.register().unwrap();
            assert!(matches!(
                runtime.prepare_native_reprepare_plan(&operation, wrong_destination, &cancellation),
                Err(NativeAudioReprepareError::Admission(_))
            ));
            assert_eq!(runtime.resource_footprint(), captured_footprint);
            assert_eq!(cursors(&runtime), held);
            let planned =
                runtime.prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation);
            if preparation == Preparation::RefusePdc {
                assert!(matches!(
                    planned,
                    Err(NativeAudioReprepareError::Compile(
                        AudioCompileError::PublicOutputLookaheadBudgetExceeded {
                            required_frames: 7,
                            budget_frames: 3
                        }
                    ))
                ));
            } else {
                let facts = planned.expect(
                    "actual retained capture must prepare the canonical PDC and empty Session",
                );
                assert_ne!(
                    facts.identity().prepared_plan_id,
                    held.session.unwrap().identity.prepared_plan_id
                );
                assert_ne!(
                    facts.identity().session_id,
                    held.session.unwrap().identity.session_id
                );
                assert_eq!(facts.schedule().public_output_lookahead_frames, 7);
                assert_eq!(facts.schedule().processor_occurrence_count, 1);
                assert_eq!(
                    facts.schedule().processor_session_scratch_bytes,
                    processor_session_bytes
                );
                assert_eq!(facts.schedule().sidechain_binding_count, 1);
                assert!(
                    facts.resources().compensation_delay_bytes
                        <= render.compensation_delay_scratch_budget_bytes
                );
                assert_eq!(
                    facts.resources().processor_session_bytes,
                    processor_session_bytes
                );
            }
            assert_eq!(
                runtime.native_reprepare_configuration(&operation).unwrap(),
                expected
            );
            assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
            assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
            verifier.validate(&claim).unwrap();
            assert_eq!(
                source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            assert_eq!(
                cursors(&runtime),
                held,
                "prepare must not reset/Enter the installed Session"
            );
            let prepared = runtime.resource_footprint();
            assert!(prepared.fixed_resident_bytes > captured_footprint.fixed_resident_bytes);
            assert!(prepared.fixed_resident_bytes <= grant.max_fixed_resident_bytes);
            assert_eq!(
                prepared.runtime_occurrences,
                captured_footprint.runtime_occurrences
            );
            assert_eq!(
                prepared.processor_session_bytes,
                captured_footprint.processor_session_bytes
            );
            assert_eq!(
                prepared.compensation_delay_bytes,
                captured_footprint.compensation_delay_bytes
            );
            assert!(matches!(
                runtime.prepare_native_reprepare_plan(&operation, verifier.clone(), &cancellation),
                Err(NativeAudioReprepareError::Admission(_))
            ));
            assert_eq!(runtime.resource_footprint(), prepared);
            prepared
        } else {
            captured_footprint
        };
        drop(operation);
        // Handle Drop cannot release parent candidate/preparation or child fees.
        assert_eq!(runtime.resource_footprint(), retained_footprint);
        assert_eq!(
            runtime.render_into(
                AudioRenderRequest { start_sample: 8, frames: 4 },
                &mut unpublished
            ),
            Err(AudioExecutionError::ReprepareRequired(ticket))
        );
        assert_eq!(unpublished, [99.0; 8]);
        assert_eq!(
            runtime.enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(2),
                start_sample: 8
            }),
            Err(AudioExecutionError::ReprepareRequired(ticket))
        );
        assert_eq!(cursors(&runtime), held);
        assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
    }));
    // Always retire the original owner before asserting target facts. A failed
    // target is not evidence of successful cleanup or physical-output quiescence.
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    let case = match preparation {
        Preparation::CaptureOnly => "runtime-retained-clap-capture-only",
        Preparation::Ready => "runtime-retained-clap-execution-prepare",
        Preparation::RefusePdc => "runtime-retained-clap-pdc-refusal",
    };
    println!(
        "{}",
        serde_json::json!({"case":case, "native_pid":pid,
        "alive_before_drop":alive,"post_drop_wait_status":settled,"exit_query_succeeded":queried!=0,"exit_code":exit,
        "media_quiescence_qualified":false,"canonical_author_or_plan_installed":false})
    );
    assert_eq!(settled, 0, "original native owner cleanup");
    assert_ne!(queried, 0);
    assert_eq!(exit, 0);
    if let Err(payload) = target {
        std::panic::resume_unwind(payload);
    }
    assert_eq!(alive, 258);
}
