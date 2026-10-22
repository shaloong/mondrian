//! Authored seconds/Note IR through the sole installed Runtime and Product.
//! MNE4 has a real stereo Main audio input; zero Graph PCM/no media is not
//! zero-input instrument qualification. No state replacement, RT, whole-heap or
//! original 100ms graceful-shutdown qualification follows from these controls.
use super::native_vst3_note_worker::{audit, NativeCall};
use super::*;
use crate::{
    AudioCompileError, AudioContinuityEpoch, AudioContinuityEvidenceVisitor, AudioExecutionError,
    AudioMediaResolver, AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease,
    AudioNoteCompletionPolicy, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioProcessorFactory, AudioProcessorResolver, AudioProgramRuntime, AudioRuntimeBuildError,
    AudioRuntimeResourceGrant, AudioSessionCursorEvidence, AudioSessionCursorState,
    AudioStateEntry, NativeAudioObjectObservation, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource, AudioSourceComponentId,
    AuthoringList, ExecutionCancellationToken, MusicalTimeReference, TimelineTime,
};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
use mondrian_timeline::Sequence;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const CLASS: &str = "6E4F05A12D8B4C639F017B42A4C2E611";
const BLOCK: usize = 4;
const AUDIT_BOUND: usize = 2 * 1024 * 1024;
const PAIRS: usize = 1025;
const FRAMES: usize = PAIRS * 2;

struct Materials {
    catalog: Arc<DiscoveredVst3AudioProcessorSpecResolver>,
    directory: PathBuf,
}
impl Materials {
    fn load() -> Self {
        let file = |name| {
            let path = PathBuf::from(std::env::var_os(name).expect("explicit native material"));
            assert!(path.is_absolute() && path.is_file(), "{name}");
            path
        };
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_MAIN_NOTE_AUDIT_DIR")
                .expect("caller supplies an existing absolute audit directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(
                file("MONDRIAN_VST3_TEST_HELPER"),
                [file("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN")],
            )
            .expect("normal Product discovery of the actual owned DLL"),
        );
        Self { catalog, directory }
    }
    fn authored(&self, render: AudioRenderContract, pairs: usize, fail_call: u32) -> Sequence {
        let instance = self
            .catalog
            .create_instance(CLASS, render, Some(configuration(fail_call)))
            .expect("ordinary restored MNE4 author snapshot, not a Note execution grant");
        assert!(instance.parameters.is_empty());
        let processor_id = instance.id;
        let mut sequence = Sequence::new("canonical VST3 Main stereo input Note oracle");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        sequence.audio_program.musical_time_reference = Some(reference(render.sample_rate));
        let track = sequence.audio_tracks[0].id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(instance);
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id,
                input_port_key: "vst3.note-input.0".into(),
            },
            notes: AuthoringList::from(
                (0..pairs)
                    .map(|index| AudioNote {
                        id: AudioNoteId::new(),
                        start: time(index * 2, render.sample_rate),
                        end: time(index * 2 + 1, render.sample_rate),
                        channel: 3,
                        key: 61,
                        on_velocity: 0.5,
                        off_velocity: 0.125,
                    })
                    .collect::<Vec<_>>(),
            ),
        });
        sequence
    }
}
fn time(sample: usize, rate: u32) -> TimelineTime {
    TimelineTime::new(i64::try_from(sample).unwrap(), i64::from(rate)).unwrap()
}
fn reference(rate: u32) -> MusicalTimeReference {
    MusicalTimeReference::new(
        TimelineTime::new(-3, i64::from(rate)).unwrap(),
        TimelineTime::new(1024, i64::from(rate)).unwrap(),
    )
    .unwrap()
}
fn configuration(fail_call: u32) -> Vec<u8> {
    let mut component = b"MNE4".to_vec();
    component.extend_from_slice(&4_u32.to_le_bytes());
    component.extend_from_slice(&fail_call.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for value in [1_u32, 12, 12] {
        state.extend_from_slice(&value.to_le_bytes());
    }
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}
fn render(rate: u32) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: rate,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        // Finite budget for this single owned fixture, including its native
        // scoped stacks/control/Note backing; this is not a budget-edge gate.
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..test_render_contract()
    }
}

struct NoMedia(Arc<AtomicUsize>);
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("no media or synthetic input is authored in this Note score".into())
    }
}
// Observe only the real resolver request/result; never replace or wrap Factory,
// payload, grant, Source binding or mutable native processor.
struct Resolver {
    inner: IsolatedAudioProcessorResolver,
    calls: Arc<AtomicUsize>,
    reference: Option<MusicalTimeReference>,
}
impl AudioProcessorResolver for Resolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(request.note_input_requested());
        assert_eq!(request.musical_time_reference(), self.reference);
        let factory = self.inner.prepare(request)?;
        let contract = factory.note_input_contract();
        contract.validate().unwrap();
        assert_eq!(
            contract.completion,
            AudioNoteCompletionPolicy::SuccessfulOffBlock
        );
        assert_eq!(contract.limits.retained_records, 1024);
        assert_eq!(
            (contract.limits.output_ends, contract.limits.live_voices),
            (0, 0)
        );
        assert_eq!(contract.ports.len(), 1);
        let port = &contract.ports[0];
        assert_eq!(port.port_key, "vst3.note-input.0");
        assert_eq!(
            (port.native_id, port.native_index, port.channel_count),
            (0, 0, 16)
        );
        assert!(factory.auxiliary_input_contract().buses.is_empty());
        assert!(factory.auxiliary_output_contract().buses.is_empty());
        Ok(factory)
    }
}
fn build(
    materials: &Materials,
    sequence: &Sequence,
    render: AudioRenderContract,
    resolves: Arc<AtomicUsize>,
    prepares: Arc<AtomicUsize>,
) -> Result<AudioProgramRuntime, AudioRuntimeBuildError> {
    let resolver = Resolver {
        inner: IsolatedAudioProcessorResolver::new(materials.catalog.clone()),
        calls: prepares,
        reference: sequence.audio_program.musical_time_reference,
    };
    AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        sequence,
        &[],
        &NoMedia(resolves),
        &resolver,
        render,
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Cursors {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for Cursors {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
    let mut result = Cursors::default();
    runtime.visit_continuity_evidence(&mut result).unwrap();
    assert_eq!((result.sessions.len(), result.processors.len()), (1, 1));
    assert_eq!(result.sessions[0].public_output_lookahead_frames, 0);
    result
}
fn owner(runtime: &AudioProgramRuntime) -> AudioNativeProcessorOwnerLease {
    let evidence = cursors(runtime);
    let processor = evidence.processors[0];
    let epoch = match processor.state {
        AudioProcessorCursorState::Unentered => None,
        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        state => panic!("no exact current native owner: {state:?}"),
    };
    runtime
        .native_owner_lease(AudioNativeProcessorAddress {
            root: evidence.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch,
        })
        .expect("lease from this actual installed Runtime")
}
struct Subject {
    runtime: AudioProgramRuntime,
    source: AudioOwnerInstallationSource,
    object: NativeAudioObjectObservation,
    handle: OwnedHandle,
    path: PathBuf,
    prefix: usize,
}
impl Subject {
    fn create(materials: &Materials, sequence: &Sequence, render: AudioRenderContract) -> Self {
        let resolves = Arc::new(AtomicUsize::new(0));
        let prepares = Arc::new(AtomicUsize::new(0));
        let mut runtime = build(
            materials,
            sequence,
            render,
            resolves.clone(),
            prepares.clone(),
        )
        .expect("canonical VST3 Note score must reach the real prepared Runtime");
        assert_eq!(resolves.load(Ordering::SeqCst), 0);
        assert_eq!(prepares.load(Ordering::SeqCst), 1);
        let source = AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().unwrap();
        runtime.bind_native_owner_installation(verifier).unwrap();
        let claim = source
            .reserve_install(&registration, None)
            .and_then(|reservation| source.begin_commit(reservation))
            .unwrap()
            .commit()
            .unwrap();
        runtime
            .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
            .expect("same actual Hosted object acknowledges its installed root");
        runtime
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .expect("no author batch is discarded or falsely acknowledged");
        let object = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
        let pid = object.process_id().get();
        assert_ne!(pid, std::process::id());
        // SAFETY: this authenticated, live Runtime object supplies the PID.
        // The retained HANDLE has query/synchronize rights only, no kill owner.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
            258
        );
        let path = materials.directory.join(format!("mne4-{pid}.audit"));
        let prefix = match std::fs::metadata(&path) {
            Ok(metadata) => usize::try_from(metadata.len()).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("audit prefix: {error}"),
        };
        assert!(prefix <= AUDIT_BOUND);
        // Capture both object and audit boundary before the very first Enter;
        // a hidden initial rebuild must not borrow another instance's evidence.
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: 0,
            })
            .expect("installed initial public origin zero");
        assert_eq!(
            runtime.observe_native_owner_object(owner(&runtime)).unwrap(),
            object
        );
        Self { runtime, source, object, handle, path, prefix }
    }
    fn same_object(&mut self) {
        assert_eq!(
            self.runtime.observe_native_owner_object(owner(&self.runtime)).unwrap(),
            self.object
        );
    }
    fn finish(self, calls: &[NativeCall], tuples: &[[u8; 48]], rate: u32) {
        let Self { runtime, source, object, handle, path, prefix } = self;
        drop(runtime);
        observe_exit(&handle, true);
        let length =
            std::fs::metadata(&path).expect("termination publishes a complete audit").len();
        assert!(length <= AUDIT_BOUND as u64 && length >= prefix as u64);
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.len() <= AUDIT_BOUND && bytes.len() >= prefix);
        let (actual_rate, actual_calls, actual_tuples) =
            audit(&bytes[prefix..], object.process_id().get());
        assert_eq!(actual_rate, f64::from(rate).to_bits());
        assert_eq!(
            actual_calls, calls,
            "every actual native call, no hidden prefix/rebuild"
        );
        assert_eq!(
            actual_tuples, tuples,
            "all ordered On/Off IDs and literal Event PPQ"
        );
        drop(source);
    }
}
fn observe_exit(handle: &OwnedHandle, success: bool) {
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) },
        0
    );
    let mut exit = u32::MAX;
    assert_ne!(
        unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
        0
    );
    if success {
        assert_eq!(exit, 0);
    }
    // Same-HANDLE OS completion within 2s, not the supervisor's 100ms result.
}
fn assert_pcm(pcm: &[f32], start: usize) {
    for (offset, pair) in pcm.chunks_exact(2).enumerate() {
        let left = if (start + offset).is_multiple_of(2) {
            0.25_f32
        } else {
            0.0
        };
        let right = if left == 0.0 { 0.0 } else { -0.125_f32 };
        assert_eq!(
            [pair[0].to_bits(), pair[1].to_bits()],
            [left.to_bits(), right.to_bits()]
        );
    }
}
fn render_block(runtime: &mut AudioProgramRuntime, start: usize, frames: usize) {
    let mut pcm = vec![99.0; frames * 2];
    runtime
        .render_into(
            AudioRenderRequest {
                start_sample: i64::try_from(start).unwrap(),
                frames,
            },
            &mut pcm,
        )
        .expect("real canonical score through its original Source and normal Product");
    assert_pcm(&pcm, start);
}
fn append_expected(
    calls: &mut Vec<NativeCall>,
    tuples: &mut Vec<[u8; 48]>,
    start: usize,
    frames: usize,
) {
    // The fixture has zero latency/lookahead; each public block is one native
    // block. Exactly one authored event occurs at each sample in this score.
    calls.push((
        i32::try_from(frames).unwrap(),
        0,
        tuples.len() as u32,
        frames as u32,
    ));
    let call = calls.len() as u32;
    for (offset, sample) in (start..start + frames).enumerate() {
        let on = sample.is_multiple_of(2);
        let mut row = [0_u8; 48];
        row[..4].copy_from_slice(&call.to_le_bytes());
        row[8..12].copy_from_slice(&(offset as i32).to_le_bytes());
        // Independent dyadic oracle: all integers here are <=2052, hence
        // exactly representable; division by 2^10 is exact binary64. No host
        // Timeline/SDK projection or accumulated musical cursor is reused.
        let q = ((sample + 3) as f64 / 1024.0).to_bits();
        row[12..20].copy_from_slice(&q.to_le_bytes());
        row[22..24].copy_from_slice(&(if on { 0_u16 } else { 1 }).to_le_bytes());
        row[24..26].copy_from_slice(&3_i16.to_le_bytes());
        row[26..28].copy_from_slice(&61_i16.to_le_bytes());
        row[32..36].copy_from_slice(&(if on { 0.5_f32 } else { 0.125 }).to_bits().to_le_bytes());
        row[40..44].copy_from_slice(&i32::try_from(sample / 2).unwrap().to_le_bytes());
        // Bus0, flags0, tuning0, length0 and final reserved bytes remain literal0.
        tuples.push(row);
    }
}

#[test]
#[ignore = "fresh normal Product, owned MNE4 and absolute audit dir; actual canonical Note/Off pairing"]
fn installed_vst3_canonical_notes_preserve_literal_pcm_ids_and_same_pid_audit() {
    let materials = Materials::load();
    for rate in [44_100, 48_000] {
        let render = render(rate);
        let sequence = materials.authored(render, PAIRS, 0);
        const { assert!(PAIRS > crate::AUDIO_NOTE_RECORDS) };
        for partition in [&[4_usize][..], &[3, 2, 4]] {
            let mut subject = Subject::create(&materials, &sequence, render);
            let mut calls = Vec::new();
            let mut tuples = Vec::new();
            let mut start = 0;
            for &size in partition.iter().cycle() {
                let frames = size.min(FRAMES - start);
                render_block(&mut subject.runtime, start, frames);
                append_expected(&mut calls, &mut tuples, start, frames);
                start += frames;
                if start == FRAMES {
                    break;
                }
            }
            assert_eq!(calls.len(), if partition.len() == 1 { 513 } else { 684 });
            assert_eq!(tuples.len(), 2050);
            let evidence = cursors(&subject.runtime);
            assert!(matches!(
                evidence.sessions[0].state,
                AudioSessionCursorState::Active { next_public_sample, .. }
                    if next_public_sample.sample() == FRAMES as i64
            ));
            subject.same_object();
            // 1025 distinct On IDs in one retained object exceed its 1024 pairing
            // records. Only successful original Off blocks can return slots;
            // this says nothing about native DSP release/voice completion.
            subject.finish(&calls, &tuples, rate);
        }
    }
}

struct ReentryExpectedAudit {
    calls: Vec<NativeCall>,
    tuples: Vec<[u8; 48]>,
}

fn assert_retired_note_epochs(
    path: &std::path::Path,
    prefix: usize,
    pid: u32,
    expected: &[ReentryExpectedAudit],
) -> Vec<u8> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("real fixture retirement audit")
        .take(AUDIT_BOUND as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= AUDIT_BOUND && bytes.len() >= prefix);
    let bytes = &bytes[prefix..];
    let mut offset = 0_usize;
    for (epoch, expected) in expected.iter().enumerate() {
        assert!(bytes.len() - offset >= 80, "complete retired epoch {epoch}");
        let length =
            u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()) as usize;
        let end = offset.checked_add(length).expect("bounded audit segment");
        assert!(length >= 80 && end <= bytes.len());
        let packet = &bytes[offset..end];
        // The fixture's DLL-local instance counter may restart after a module
        // reload; the actual SDK object serial below authenticates freshness.
        // The original reader validates the complete header, PID, mode, lengths,
        // capacities and every actual call/event. Each segment is a physical
        // component's real terminate callback, which requires processing=false;
        // it is not a Stop/Deactivate/Terminate-return ACK or DLL-unload witness.
        let (rate, calls, tuples) = audit(packet, pid);
        assert_eq!(rate, 48_000_f64.to_bits());
        assert_eq!(
            calls, expected.calls,
            "all native calls in retired epoch {epoch}"
        );
        assert_eq!(
            tuples, expected.tuples,
            "all native tuples in retired epoch {epoch}"
        );
        offset = end;
    }
    assert_eq!(
        offset,
        bytes.len(),
        "no hidden or unreviewed component lifetime"
    );
    bytes.to_vec()
}

#[test]
#[ignore = "fresh normal Product, owned MNE4 and absolute audit dir; new-epoch public origin0 reentry, no nonzero seek/continuous-history claim"]
fn installed_vst3_canonical_note_origin_reentry_rebuilds_without_reusing_ids() {
    let materials = Materials::load();
    let render = render(48_000);
    let sequence = materials.authored(render, 2, 0);
    // Reuse the existing independent literal PPQ/tuple oracle. Only the new
    // epochs' native IDs differ: the one retained parent ledger must continue
    // its namespace even though the actual native component is replaced.
    let phases: [(&[(usize, usize)], &[i32]); 3] = [
        (&[(0, 3)], &[0, 0, 1]),
        (&[(0, 1), (1, 3)], &[2, 2, 3, 3]),
        (&[(0, 3), (3, 1)], &[4, 4, 5, 5]),
    ];
    let expected: Vec<_> = phases
        .iter()
        .map(|(partition, ids)| {
            let mut calls = Vec::new();
            let mut tuples = Vec::new();
            for &(start, frames) in *partition {
                append_expected(&mut calls, &mut tuples, start, frames);
            }
            assert_eq!(tuples.len(), ids.len());
            for (tuple, id) in tuples.iter_mut().zip(*ids) {
                tuple[40..44].copy_from_slice(&id.to_le_bytes());
            }
            ReentryExpectedAudit { calls, tuples }
        })
        .collect();
    let mut subject = Subject::create(&materials, &sequence, render);
    let pid = subject.object.process_id().get();
    let root = subject.runtime.resource_root_identity();
    let initial_claim = subject.source.current_claim().unwrap().unwrap();
    let mut retired_prefix = Vec::new();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render_block(&mut subject.runtime, 0, 3); // On2 still awaits its original Off3.
        for (phase, epoch) in [(1_usize, 2_u64), (2, 3)] {
            let before = cursors(&subject.runtime);
            let old = owner(&subject.runtime);
            let old_object = subject.runtime.observe_native_owner_object(old).unwrap();
            let claim = subject.source.current_claim().unwrap().unwrap();
            let cancelled = ExecutionCancellationToken::new();
            cancelled.cancel();
            assert_eq!(
                subject.runtime.enter_state_cancellable(
                    AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(epoch),
                        start_sample: 0,
                    },
                    &cancelled,
                ),
                Err(crate::AudioNativeOwnerError::Cancelled.into())
            );
            assert!(matches!(
                subject.runtime.enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: 1,
                }),
                Err(AudioExecutionError::ProcessorHost(
                    AudioProcessorHostError::NoteExecutionRefused(_)
                ))
            ));
            assert_eq!(cursors(&subject.runtime), before);
            assert_eq!(owner(&subject.runtime), old);
            assert_eq!(
                subject.runtime.observe_native_owner_object(old).unwrap(),
                old_object
            );
            assert_eq!(
                subject.source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            subject
                .runtime
                .native_owner_installation_verifier()
                .unwrap()
                .validate(&claim)
                .unwrap();
            subject.runtime.validate_native_owner(old).unwrap();

            // Baseline must reach this actual public API and fail Unsupported.
            // Success means the same prepared initial configuration was rebuilt;
            // it is not a generic absolute-zero DSP or continuous-history promise.
            subject
                .runtime
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: 0,
                })
                .expect(
                    "new-epoch canonical origin0 reentry must rebuild the actual VST3 producer",
                );
            let fresh = owner(&subject.runtime);
            let object = subject.runtime.observe_native_owner_object(fresh).unwrap();
            assert_eq!(object.process_id(), old_object.process_id());
            assert!(object.object_serial() > old_object.object_serial());
            assert_ne!(fresh.binding(), old.binding());
            assert_eq!(fresh.address().root, old.address().root);
            assert_eq!(fresh.address().local, old.address().local);
            assert_eq!(fresh.address().occurrence, old.address().occurrence);
            assert_eq!(
                fresh.address().epoch,
                Some(AudioContinuityEpoch::new(epoch))
            );
            assert!(subject.runtime.validate_native_owner(old).is_err());
            assert!(subject.runtime.observe_native_owner_object(old).is_err());
            subject.runtime.validate_native_owner(fresh).unwrap();
            assert_eq!(subject.runtime.resource_root_identity(), root);
            assert_eq!(
                subject.source.current_claim().unwrap().unwrap().generation(),
                initial_claim.generation()
            );
            subject
                .runtime
                .native_owner_installation_verifier()
                .unwrap()
                .validate(&initial_claim)
                .unwrap();
            let entered = cursors(&subject.runtime);
            assert_eq!(entered.sessions[0].identity, before.sessions[0].identity);
            assert_eq!(
                entered.processors[0].identity,
                before.processors[0].identity
            );
            assert!(matches!(entered.sessions[0].state,
                AudioSessionCursorState::Active { epoch: actual, next_public_sample, .. }
                    if actual == AudioContinuityEpoch::new(epoch) && next_public_sample.sample() == 0));

            // Old component termination evidence must already be complete before
            // the fresh component's first On/PCM. The first old lifetime has no
            // synthetic Off3, and its old pending Off is not sent to the new producer.
            let retired =
                assert_retired_note_epochs(&subject.path, subject.prefix, pid, &expected[..phase]);
            assert!(
                retired.starts_with(&retired_prefix),
                "retired audit prefixes are immutable"
            );
            retired_prefix = retired;
            assert_eq!(
                subject.runtime.enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: 0,
                }),
                Err(AudioExecutionError::ReusedContinuityEpoch(
                    AudioContinuityEpoch::new(epoch)
                ))
            );
            assert_eq!(cursors(&subject.runtime), entered);
            assert_eq!(owner(&subject.runtime), fresh);
            assert_eq!(
                subject.runtime.observe_native_owner_object(fresh).unwrap(),
                object
            );
            assert_eq!(
                subject.source.current_claim().unwrap().unwrap().generation(),
                initial_claim.generation()
            );
            for &(start, frames) in phases[phase].0 {
                render_block(&mut subject.runtime, start, frames);
                assert_eq!(owner(&subject.runtime), fresh);
                assert_eq!(
                    subject.runtime.observe_native_owner_object(fresh).unwrap(),
                    object
                );
            }
            let completed = cursors(&subject.runtime);
            assert!(matches!(completed.sessions[0].state,
                AudioSessionCursorState::Active { epoch: actual, next_public_sample, .. }
                    if actual == AudioContinuityEpoch::new(epoch) && next_public_sample.sample() == 4));
            subject
                .runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
        }
    }));
    let Subject { runtime, source, handle, path, prefix, .. } = subject;
    // The same original Runtime and process HANDLE settle even on baseline RED.
    // Never ask the outer Job to make a leaked processing owner look successful.
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(runtime);
        observe_exit(&handle, true);
        drop(source);
    }));
    if let Err(failure) = target {
        std::panic::resume_unwind(failure);
    }
    if let Err(failure) = cleanup {
        std::panic::resume_unwind(failure);
    }
    let all = assert_retired_note_epochs(&path, prefix, pid, &expected);
    assert!(
        all.starts_with(&retired_prefix),
        "all retired component prefixes remain unchanged"
    );
}

#[test]
#[ignore = "fresh normal Product and owned MNE4; Source/owner preserving cold and repeated-entry refusal"]
fn installed_vst3_canonical_note_entry_refusals_preserve_source_and_pending_off() {
    let materials = Materials::load();
    let render = render(48_000);
    let sequence = materials.authored(render, 5, 0);
    let mut cold = sequence.clone();
    cold.audio_program.musical_time_reference = None;
    let resolves = Arc::new(AtomicUsize::new(0));
    let prepares = Arc::new(AtomicUsize::new(0));
    let error = match build(
        &materials,
        &cold,
        render,
        resolves.clone(),
        prepares.clone(),
    ) {
        Ok(_) => panic!("canonical VST3 Notes without an author reference were admitted"),
        Err(error) => error,
    };
    let lane = &cold.audio_program.note_lanes[0];
    assert!(matches!(
        error,
        AudioRuntimeBuildError::Compile(AudioCompileError::NoteExecutionNotSupported {
            lane_id, processor_id
        }) if lane_id == lane.id && processor_id == lane.target.processor_id
    ));
    assert_eq!(resolves.load(Ordering::SeqCst), 0);
    assert_eq!(
        prepares.load(Ordering::SeqCst),
        0,
        "no resolver, therefore no factory/create"
    );
    // No Runtime was returned, so there is no instance/lease to publish into
    // the installation Source. An unrelated empty Source is not evidence.

    let mut subject = Subject::create(&materials, &sequence, render);
    render_block(&mut subject.runtime, 0, 3); // On2 awaits its original Off3.
    let before = cursors(&subject.runtime);
    let lease = owner(&subject.runtime);
    let generation = subject.source.current_claim().unwrap().unwrap().generation();
    for (epoch, start) in [(2, 0), (2, 3), (3, 9)] {
        let error = subject
            .runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(epoch),
                start_sample: start,
            })
            .unwrap_err();
        assert_eq!(
            error,
            AudioExecutionError::ProcessorHost(AudioProcessorHostError::NoteExecutionRefused(
                "VST3 Note score supports only initial entry"
            ))
        );
        assert_eq!(cursors(&subject.runtime), before);
        assert_eq!(
            owner(&subject.runtime),
            lease,
            "original binding and continuity lease"
        );
        assert_eq!(
            subject.source.current_claim().unwrap().unwrap().generation(),
            generation
        );
        assert_eq!(
            subject.runtime.observe_native_owner_object(lease).unwrap(),
            subject.object
        );
    }
    let mut calls = Vec::new();
    let mut tuples = Vec::new();
    append_expected(&mut calls, &mut tuples, 0, 3);
    for (start, frames) in [(3, 1), (4, 4), (8, 2)] {
        render_block(&mut subject.runtime, start, frames);
        append_expected(&mut calls, &mut tuples, start, frames);
    }
    subject.same_object();
    subject.finish(&calls, &tuples, 48_000);
}

#[test]
#[ignore = "fresh normal Product and owned MNE4; failed parent non-reuse, no mandatory failed-tail MNA4"]
fn installed_vst3_canonical_note_failure_poison_preserves_parent_terminal_boundary() {
    let materials = Materials::load();
    let render = render(48_000);
    let sequence = materials.authored(render, 6, 2);
    let mut subject = Subject::create(&materials, &sequence, render);
    render_block(&mut subject.runtime, 0, 4);
    subject.same_object();
    let mut failed = [99.0; 8];
    let error = subject
        .runtime
        .render_into(
            AudioRenderRequest { start_sample: 4, frames: 4 },
            &mut failed,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AudioExecutionError::ProcessorHost(AudioProcessorHostError::WorkerFailed {
            operation: "block processing", detail
        }) if detail == "audio processor block execution failed: VST3 processing failed: Plugin process() failed: 0x1"
    ));
    // Runtime may already clear/copy a prefix on the original failed request.
    // This is not a DSP/parameter/caller-buffer transaction rollback assertion.
    let evidence = cursors(&subject.runtime);
    assert_eq!(
        evidence.sessions[0].state,
        AudioSessionCursorState::Poisoned { epoch: AudioContinuityEpoch::new(1) }
    );
    assert_eq!(
        evidence.processors[0].state,
        AudioProcessorCursorState::Failed
    );
    for _ in 0..2 {
        let mut untouched = [99.0; 8];
        let error = subject
            .runtime
            .render_into(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut untouched,
            )
            .unwrap_err();
        assert_eq!(
            error,
            AudioExecutionError::ProcessorHost(AudioProcessorHostError::PoisonedInstance)
        );
        assert_eq!(
            untouched, [99.0; 8],
            "terminal preflight does not publish PCM"
        );
        assert_eq!(
            subject.runtime.enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(2),
                start_sample: 0
            }),
            Err(AudioExecutionError::ProcessorHost(
                AudioProcessorHostError::PoisonedInstance
            ))
        );
        assert_eq!(cursors(&subject.runtime), evidence);
    }
    let Subject { runtime, source, handle, .. } = subject;
    drop(runtime);
    observe_exit(&handle, false);
    drop(source);
    // Failed is replied before child scope join/terminate. Parent immediately
    // kills its sole child, so a missing terminal audit is not false success,
    // and no failed-tail tuple/no-rebuild claim is made here.
}
