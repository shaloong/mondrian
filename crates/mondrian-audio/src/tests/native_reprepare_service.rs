//! Audio-only retained restart controls. No Media/output-quiescence or full C07 qualification.
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

#[test]
#[ignore = "current production helper and owned CLAP mode7; stopped Source installation then explicit Fresh Enter, no Media quiescence"]
fn installed_clap_runtime_restart_installs_same_child_then_requires_fresh_entry() {
    retained_restart_control(Preparation::Install);
}

#[test]
#[ignore = "current production helper and owned CLAP mode7; child commit followed by wrong Source CAS must remain held"]
fn installed_clap_runtime_restart_commit_then_wrong_cas_retains_old_hold_without_retry() {
    retained_restart_control(Preparation::RefuseCas);
}

#[test]
#[ignore = "current production helper and owned CLAP mode7; authenticated Commit reply deliberately hidden from Runtime, not TCP loss"]
fn installed_clap_runtime_restart_lost_commit_reply_is_unconfirmed_and_never_retried() {
    retained_restart_control(Preparation::LostCommitReply);
}

#[test]
#[ignore = "current production helper and isolated CLAP mode13; two same-child restart installations under one Root grant, no Media quiescence"]
fn installed_clap_runtime_repeated_restart_retires_resources_under_the_same_grant() {
    retained_restart_control(Preparation::TwoCycles);
}

fn assert_restarted_pcm(runtime: &mut AudioProgramRuntime, start: i64) {
    let mut pcm = [99.0; 8];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: start, frames: 4 },
            &mut pcm,
        )
        .expect("new canonical PDC public window after explicit Fresh Enter");
    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
        let expected = (start as usize + offset + 1) as f32 * 2.5;
        assert_eq!(
            pair,
            [expected, -expected],
            "unchanged main + routed detector PCM"
        );
    }
}

// Observation-only original child/domain counts, never an execution proof.
#[derive(Default)]
struct ChildCounts {
    scopes: std::sync::atomic::AtomicUsize,
    children: std::sync::atomic::AtomicUsize,
    settled: std::sync::atomic::AtomicUsize,
    unavailable: std::sync::atomic::AtomicBool,
}
impl mondrian_core::NativeChildSettlementReporter for ChildCounts {
    fn begin_scope(&self) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.scopes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn end_scope(&self) {
        self.scopes.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn begin_child(&self) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.children.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn end_child(&self, result: mondrian_core::NativeChildSettlement) {
        self.children.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        if result == mondrian_core::NativeChildSettlement::Settled {
            self.settled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        } else {
            self.unavailable();
        }
    }
    fn unavailable(&self) {
        self.unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Preparation {
    CaptureOnly,
    Ready,
    RefusePdc,
    Install,
    RefuseCas,
    LostCommitReply,
    TwoCycles,
}

fn retained_restart_control(preparation: Preparation) {
    let prepare = preparation != Preparation::CaptureOnly;
    let install = matches!(
        preparation,
        Preparation::Install
            | Preparation::RefuseCas
            | Preparation::LostCommitReply
            | Preparation::TwoCycles
    );
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::atomic::Ordering::SeqCst;
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
    let mode = if preparation == Preparation::TwoCycles {
        13
    } else {
        7
    };
    let mut state = vec![mode];
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
    let mut processors = IsolatedAudioProcessorResolver::new(catalog);
    let lost_reply = (preparation == Preparation::LostCommitReply)
        .then(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
    if let Some(observed) = &lost_reply {
        processors.discard_next_seed_commit_reply_for_test(Arc::clone(observed));
    }
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
    let counts = install.then(|| Arc::new(ChildCounts::default()));
    let mut registrar = None;
    let mut installed = false;
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
        let mut expected = vec![mode];
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
        let mut next_identity = None;
        let mut next_resources = None;
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
                next_identity = Some(facts.identity());
                next_resources = Some(facts.resources());
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
        if install {
            // Deliberately bind the real original child AFTER preparation. The
            // empty destination must inherit this same domain at installation.
            let observed = counts.as_ref().unwrap();
            registrar = Some(
                mondrian_core::NativeChildSettlementRegistrar::try_new(observed.clone())
                    .expect("late original domain"),
            );
            runtime
                .bind_native_child_settlement(registrar.as_ref().unwrap())
                .expect("late actual child registration");
            assert_eq!(observed.children.load(SeqCst), 1);
            assert_eq!(observed.settled.load(SeqCst), 0);
            assert_eq!(observed.scopes.load(SeqCst), 2);
            let mut authorization = source
                .authorize_runtime_installation(
                    source.reserve_install(&registration, Some(&claim)).unwrap(),
                )
                .unwrap();
            let canceled = ExecutionCancellationToken::new();
            canceled.cancel();
            assert!(matches!(
                runtime.install_native_reprepare_plan(&operation, &mut authorization, &canceled),
                Err(NativeAudioReprepareError::Cancelled)
            ));
            assert_eq!(runtime.resource_footprint(), retained_footprint);
            assert_eq!(cursors(&runtime), held);
            // An unrelated real Source permission reaches begin_commit only
            // AFTER actual Stage/Commit and all stopped transfer validations.
            if preparation == Preparation::RefuseCas {
                let unrelated = AudioOwnerInstallationSource::new();
                let (other_registration, _) = unrelated.register().unwrap();
                authorization = unrelated
                    .authorize_runtime_installation(
                        unrelated.reserve_install(&other_registration, None).unwrap(),
                    )
                    .unwrap();
            }
            let outcome = runtime.install_native_reprepare_plan(
                &operation,
                &mut authorization,
                &cancellation,
            );
            if matches!(preparation, Preparation::Install | Preparation::TwoCycles) {
                let receipt = outcome.expect("actual same-child Source installation");
                installed = true;
                assert_eq!(receipt.identity(), next_identity.unwrap());
                assert_eq!(receipt.previous_epoch(), Some(AudioContinuityEpoch::new(1)));
                verifier.validate(receipt.claim()).unwrap();
                assert_ne!(receipt.claim().generation(), claim.generation());
                assert!(verifier.validate(&claim).is_err());
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    receipt.claim().generation()
                );
                assert_eq!(runtime.public_output_lookahead_frames(), 7);
                let unentered = cursors(&runtime);
                assert_eq!(unentered.session.unwrap().identity, receipt.identity());
                assert_eq!(
                    unentered.session.unwrap().state,
                    AudioSessionCursorState::Unentered
                );
                assert_eq!(
                    unentered.processor.unwrap().state,
                    AudioProcessorCursorState::Unentered
                );
                assert!(runtime.native_owner_lease(address).is_err());
                assert_eq!(
                    runtime.render_into(
                        AudioRenderRequest { start_sample: 8, frames: 4 },
                        &mut unpublished
                    ),
                    Err(AudioExecutionError::StateEntryRequired)
                );
                assert_eq!(unpublished, [99.0; 8]);
                assert_eq!(
                    observed.children.load(SeqCst),
                    1,
                    "Hosted transfer does not register/create another child"
                );
                assert_eq!(
                    observed.scopes.load(SeqCst),
                    3,
                    "new Session inherited actual late-bound domain; retired one stays live"
                );
                let published = runtime.resource_footprint();
                assert_eq!(
                    published.fixed_resident_bytes,
                    retained_footprint.fixed_resident_bytes + expected.len()
                );
                assert_eq!(
                    published.prepared_logical_bytes,
                    next_resources.unwrap().prepared_logical_bytes
                );
                assert_eq!(
                    published.compensation_delay_bytes,
                    next_resources.unwrap().compensation_delay_bytes
                );
                assert_eq!(published.runtime_occurrences, 1);
                runtime
                    .enter_state_cancellable(
                        AudioStateEntry {
                            epoch: AudioContinuityEpoch::new(2),
                            start_sample: 8,
                        },
                        &cancellation,
                    )
                    .expect("explicit consumer Fresh Enter");
                let new_address = AudioNativeProcessorAddress {
                    root: receipt.identity(),
                    local: receipt.identity(),
                    occurrence,
                    epoch: Some(AudioContinuityEpoch::new(2)),
                };
                let new_lease = runtime.native_owner_lease(new_address).unwrap();
                assert_ne!(new_lease.binding(), lease.binding());
                assert_eq!(
                    new_lease.installed_root_serial(),
                    receipt.claim().generation()
                );
                assert_eq!(
                    runtime.observe_native_owner_object(new_lease).unwrap(),
                    object,
                    "same authenticated child/object, not a newly created native instance"
                );
                for start in [8, 12] {
                    assert_restarted_pcm(&mut runtime, start);
                }
                let active = cursors(&runtime);
                let rate = active.session.unwrap().sample_rate;
                assert_eq!(
                    active.session.unwrap().state,
                    AudioSessionCursorState::Active {
                        epoch: AudioContinuityEpoch::new(2),
                        next_public_sample: mondrian_core::AudioSamplePosition::new(16, rate),
                        next_execution_sample: mondrian_core::AudioSamplePosition::new(23, rate),
                        alignment_primed: true,
                    }
                );
                assert_eq!(
                    active.processor.unwrap().state,
                    AudioProcessorCursorState::Active {
                        epoch: AudioContinuityEpoch::new(2),
                        next_input_signal_sample: mondrian_core::AudioSamplePosition::new(23, rate),
                    }
                );
                assert!(matches!(
                    runtime.install_native_reprepare_plan(
                        &operation,
                        &mut authorization,
                        &cancellation
                    ),
                    Err(NativeAudioReprepareError::Admission(_))
                ));
                let first_claim = receipt.claim().clone();
                let first_resources = next_resources.unwrap();
                let second_ticket = if preparation == Preparation::TwoCycles {
                    // Public16/20 complete before the actual second callback;
                    // installed PDC7 evaluates through31, threshold29.
                    for start in [16, 20] {
                        assert_restarted_pcm(&mut runtime, start);
                    }
                    let ticket = match runtime.render_into(
                        AudioRenderRequest { start_sample: 24, frames: 4 },
                        &mut unpublished,
                    ) {
                        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
                        other => panic!("actual mode13 second callback: {other:?}"),
                    };
                    assert_eq!(ticket.prepared_plan_id, receipt.identity().prepared_plan_id);
                    assert_eq!(ticket.session_id, receipt.identity().session_id);
                    assert_eq!(ticket.continuity_epoch, Some(AudioContinuityEpoch::new(2)));
                    assert_eq!(ticket.occurrence, occurrence);
                    Some(ticket)
                } else {
                    None
                };
                let still_high = runtime.resource_footprint();
                assert!(matches!(
                    runtime
                        .service_native_reprepare(second_ticket.unwrap_or(ticket), &cancellation),
                    Err(NativeAudioReprepareError::Admission(
                        "an actual restart attempt is already retained"
                    ))
                ));
                assert_eq!(runtime.resource_footprint(), still_high);
                // Actual old Session/Factory/candidate/last logical fee retire
                // here, with no Runtime/metadata/Source guard held.
                let retired = receipt.retire();
                assert_eq!(observed.scopes.load(SeqCst), 2);
                assert_eq!(observed.children.load(SeqCst), 1);
                assert_eq!(observed.settled.load(SeqCst), 0);
                assert_eq!(
                    runtime.resource_footprint(),
                    still_high,
                    "physical Drop alone cannot publish a credit"
                );
                runtime
                    .finish_native_reprepare_retirement(retired)
                    .expect("exact original Runtime consumes actual retirement");
                let normalized_one = runtime.resource_footprint();
                assert!(normalized_one.fixed_resident_bytes < still_high.fixed_resident_bytes);
                assert_eq!(
                    normalized_one.prepared_logical_bytes,
                    first_resources.prepared_logical_bytes
                );
                assert_eq!(normalized_one.runtime_occurrences, 1);
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    first_claim.generation()
                );
                if let Some(ticket_two) = second_ticket {
                    let held_two = cursors(&runtime);
                    let second = runtime
                        .service_native_reprepare(ticket_two, &cancellation)
                        .expect("same Root/grant admits second actual restart after retirement");
                    let mut expected_two = vec![13];
                    expected_two.extend_from_slice(&0.5_f32.to_le_bytes());
                    expected_two.push(2);
                    assert_eq!(
                        runtime.native_reprepare_configuration(&second).unwrap(),
                        expected_two
                    );
                    assert_eq!(
                        second
                            .configuration_identity()
                            .observed_contract
                            .algorithmic_latency_frames(),
                        11
                    );
                    assert_eq!(
                        second.configuration_identity().observed_contract.tail(),
                        AudioProcessorTail::Finite(11)
                    );
                    let second_plan = runtime
                        .prepare_native_reprepare_plan(&second, verifier.clone(), &cancellation)
                        .expect("canonical PDC11 preparation");
                    assert_eq!(second_plan.schedule().public_output_lookahead_frames, 11);
                    assert_eq!(second_plan.schedule().processor_occurrence_count, 1);
                    assert_eq!(second_plan.schedule().sidechain_binding_count, 1);
                    assert_eq!(
                        second_plan.resources().processor_session_bytes,
                        processor_session_bytes
                    );
                    assert_eq!(cursors(&runtime), held_two);
                    let mut second_permission = source
                        .authorize_runtime_installation(
                            source.reserve_install(&registration, Some(&first_claim)).unwrap(),
                        )
                        .unwrap();
                    let receipt_two = runtime
                        .install_native_reprepare_plan(
                            &second,
                            &mut second_permission,
                            &cancellation,
                        )
                        .expect("same actual child installs second stopped candidate");
                    assert_eq!(receipt_two.identity(), second_plan.identity());
                    assert_eq!(
                        receipt_two.previous_epoch(),
                        Some(AudioContinuityEpoch::new(2))
                    );
                    verifier.validate(receipt_two.claim()).unwrap();
                    assert!(verifier.validate(&first_claim).is_err());
                    assert_eq!(runtime.public_output_lookahead_frames(), 11);
                    assert_eq!(
                        cursors(&runtime).session.unwrap().state,
                        AudioSessionCursorState::Unentered
                    );
                    assert_eq!(
                        cursors(&runtime).processor.unwrap().state,
                        AudioProcessorCursorState::Unentered
                    );
                    assert_eq!(
                        runtime.render_into(
                            AudioRenderRequest { start_sample: 24, frames: 4 },
                            &mut unpublished
                        ),
                        Err(AudioExecutionError::StateEntryRequired)
                    );
                    assert_eq!(unpublished, [99.0; 8]);
                    assert!(runtime.native_owner_lease(new_address).is_err());
                    runtime
                        .enter_state_cancellable(
                            AudioStateEntry {
                                epoch: AudioContinuityEpoch::new(3),
                                start_sample: 24,
                            },
                            &cancellation,
                        )
                        .expect("second explicit consumer Fresh Enter");
                    let address_three = AudioNativeProcessorAddress {
                        root: receipt_two.identity(),
                        local: receipt_two.identity(),
                        occurrence,
                        epoch: Some(AudioContinuityEpoch::new(3)),
                    };
                    let lease_three = runtime.native_owner_lease(address_three).unwrap();
                    assert_ne!(lease_three.binding(), new_lease.binding());
                    assert_eq!(
                        lease_three.installed_root_serial(),
                        receipt_two.claim().generation()
                    );
                    assert_eq!(
                        runtime.observe_native_owner_object(lease_three).unwrap(),
                        object
                    );
                    for start in [24, 28] {
                        assert_restarted_pcm(&mut runtime, start);
                    }
                    let active_three = cursors(&runtime);
                    assert_eq!(
                        active_three.session.unwrap().state,
                        AudioSessionCursorState::Active {
                            epoch: AudioContinuityEpoch::new(3),
                            next_public_sample: mondrian_core::AudioSamplePosition::new(32, rate),
                            next_execution_sample: mondrian_core::AudioSamplePosition::new(
                                43, rate
                            ),
                            alignment_primed: true,
                        }
                    );
                    assert_eq!(
                        active_three.processor.unwrap().state,
                        AudioProcessorCursorState::Active {
                            epoch: AudioContinuityEpoch::new(3),
                            next_input_signal_sample: mondrian_core::AudioSamplePosition::new(
                                43, rate
                            ),
                        }
                    );
                    let high_two = runtime.resource_footprint();
                    assert!(high_two.fixed_resident_bytes <= grant.max_fixed_resident_bytes);
                    assert!(matches!(
                        runtime.service_native_reprepare(ticket_two, &cancellation),
                        Err(NativeAudioReprepareError::Admission(
                            "an actual restart attempt is already retained"
                        ))
                    ));
                    assert_eq!(runtime.resource_footprint(), high_two);
                    assert_eq!(observed.children.load(SeqCst), 1);
                    assert_eq!(observed.scopes.load(SeqCst), 3);
                    let current_claim = receipt_two.claim().clone();
                    let retired_two = receipt_two.retire();
                    assert_eq!(runtime.resource_footprint(), high_two);
                    runtime.finish_native_reprepare_retirement(retired_two).unwrap();
                    let normalized_two = runtime.resource_footprint();
                    assert_eq!(normalized_two.fixed_resident_bytes,
                        normalized_one.fixed_resident_bytes
                            - first_resources.fixed_resident_bytes
                            + second_plan.resources().fixed_resident_bytes,
                        "same-sized mode13 metadata/seed replace old extras; no capture/codec/seed fee accumulation");
                    assert_eq!(
                        normalized_two.prepared_logical_bytes,
                        second_plan.resources().prepared_logical_bytes
                    );
                    assert_eq!(normalized_two.runtime_occurrences, 1);
                    assert_eq!(observed.children.load(SeqCst), 1);
                    assert_eq!(observed.scopes.load(SeqCst), 2);
                    assert_eq!(observed.settled.load(SeqCst), 0);
                    verifier.validate(&current_claim).unwrap();
                    assert_eq!(
                        source.current_claim().unwrap().unwrap().generation(),
                        current_claim.generation()
                    );
                    drop(second);
                    assert_eq!(runtime.resource_footprint(), normalized_two);
                }
            } else {
                match outcome {
                    Err(NativeAudioReprepareError::Owner(AudioNativeOwnerError::Installation(
                        mondrian_core::AudioOwnerInstallationError::WrongRegistration,
                    ))) if preparation == Preparation::RefuseCas => {}
                    Err(NativeAudioReprepareError::Processor(
                        AudioProcessorHostError::WorkerFailed {
                            operation: "configuration seed commit",
                            detail,
                        },
                    )) if preparation == Preparation::LostCommitReply => {
                        assert!(detail.contains("test discarded authenticated seed commit reply"));
                        assert!(
                            lost_reply.as_ref().unwrap().load(SeqCst),
                            "actual authenticated Accepted preceded injected parent-visible loss"
                        );
                    }
                    _ => panic!("exact post-commit failure category was not reached"),
                }
                assert_eq!(cursors(&runtime), held);
                verifier.validate(&claim).unwrap();
                assert_eq!(
                    source.current_claim().unwrap().unwrap().generation(),
                    claim.generation()
                );
                assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
                assert_eq!(
                    runtime.native_reprepare_configuration(&operation).unwrap(),
                    expected
                );
                let failed = runtime.resource_footprint();
                assert_eq!(
                    failed.fixed_resident_bytes,
                    retained_footprint.fixed_resident_bytes + expected.len()
                );
                let mut other_weights = failed;
                other_weights.fixed_resident_bytes = retained_footprint.fixed_resident_bytes;
                assert_eq!(other_weights, retained_footprint);
                let mut retry = source
                    .authorize_runtime_installation(
                        source.reserve_install(&registration, Some(&claim)).unwrap(),
                    )
                    .unwrap();
                assert!(matches!(
                    runtime.install_native_reprepare_plan(&operation, &mut retry, &cancellation),
                    Err(NativeAudioReprepareError::Admission(
                        "restart installation already attempted or not prepared"
                    ))
                ));
                assert_eq!(runtime.resource_footprint(), failed);
                assert_eq!(
                    runtime.render_into(
                        AudioRenderRequest { start_sample: 8, frames: 4 },
                        &mut unpublished
                    ),
                    Err(AudioExecutionError::ReprepareRequired(ticket))
                );
                assert_eq!(
                    runtime.enter_state_cancellable(
                        AudioStateEntry {
                            epoch: AudioContinuityEpoch::new(2),
                            start_sample: 8,
                        },
                        &cancellation
                    ),
                    Err(AudioExecutionError::ReprepareRequired(ticket))
                );
                assert_eq!(unpublished, [99.0; 8]);
                assert_eq!(cursors(&runtime), held);
                drop(operation);
                assert_eq!(
                    runtime.resource_footprint(),
                    failed,
                    "failure ownership/seed credit is independent of the external handle"
                );
            }
            assert_eq!(serde_json::to_value(&sequence).unwrap(), author_before);
            return;
        }
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
    drop(registrar);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
    let case = match preparation {
        Preparation::CaptureOnly => "runtime-retained-clap-capture-only",
        Preparation::Ready => "runtime-retained-clap-execution-prepare",
        Preparation::RefusePdc => "runtime-retained-clap-pdc-refusal",
        Preparation::Install => "runtime-retained-clap-install-fresh-entry",
        Preparation::RefuseCas => "runtime-retained-clap-commit-cas-refusal",
        Preparation::LostCommitReply => "runtime-retained-clap-commit-reply-loss",
        Preparation::TwoCycles => "runtime-retained-clap-repeat-install-retire",
    };
    println!(
        "{}",
        serde_json::json!({"case":case, "native_pid":pid,
        "alive_before_drop":alive,"post_drop_wait_status":settled,"exit_query_succeeded":queried!=0,"exit_code":exit,
        "media_quiescence_qualified":false,"canonical_author_or_plan_installed":installed})
    );
    assert_eq!(settled, 0, "original native owner cleanup");
    assert_ne!(queried, 0);
    if preparation != Preparation::LostCommitReply {
        assert_eq!(exit, 0);
    }
    if let Some(observed) = counts {
        assert_eq!(
            observed.scopes.load(SeqCst),
            0,
            "all actual Session/receipt domain forks retired"
        );
        assert_eq!(observed.children.load(SeqCst), 0);
        assert_eq!(
            observed.settled.load(SeqCst),
            1,
            "one original ProcessOwner reports once"
        );
        assert!(!observed.unavailable.load(SeqCst));
    }
    if let Err(payload) = target {
        std::panic::resume_unwind(payload);
    }
    if preparation != Preparation::LostCommitReply {
        assert_eq!(alive, 258);
    }
}
