//! Optional current presentation facts, never author edits or a delivery service.
//! Explicit full-owner diagnostics query the original selected native lanes;
//! source/control observations do not query parameter presentations.
use super::*;
use mondrian_core::{
    NativeAudioParameterFact as Fact, NativeAudioParameterObservationOrigin as Origin,
};
use std::num::NonZeroU64;
use vst3_host::controller_parameters::{
    ControllerParameterPresentationFrame, ControllerParameterPresentationRef,
};
use vst3_host::plugin::{PluginEditorControl, RestartCohort};

/// Fixed main-thread scratch, explicitly included in the existing inline fee.
/// The seen bits validate unique selected identities independently of visit count.
pub(super) struct ParameterPresentationFrame {
    controller: ControllerParameterPresentationFrame,
    seen: [bool; MAX_PARAMETERS],
}
impl Default for ParameterPresentationFrame {
    fn default() -> Self {
        Self {
            controller: ControllerParameterPresentationFrame::default(),
            seen: [false; MAX_PARAMETERS],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    owner: NonZeroU64,
    source: crate::NativeAudioAuthorSourceStatus,
    cohort: RestartCohort,
}
impl Snapshot {
    fn capture(editor: &PluginEditorControl<'_>) -> Option<Self> {
        Some(Self {
            owner: editor.attachment_owner().ok()?.serial(),
            source: native_author::status(editor.editor_event_source_status().ok()?).ok()?,
            cohort: editor.pending_restart_cohort().ok()??,
        })
    }
    fn healthy(self) -> bool {
        self.source.closed_empty() && !self.cohort.pending()
    }
}
pub(super) struct Presentations {
    owner: NonZeroU64,
    revision: u64,
    facts: Vec<Fact>,
}

/// A query's fixed borrowed outputs; not a second parameter inventory or cache.
struct CurrentParameter<'a> {
    id: u32,
    default: f64,
    steps: i32,
    flags: u32,
    title: &'a [u16; 128],
    normalized: f64,
    plain: Option<f64>,
    display: Option<&'a [u16; 128]>,
    hidden: bool,
}
impl<'a> From<ControllerParameterPresentationRef<'a>> for CurrentParameter<'a> {
    fn from(value: ControllerParameterPresentationRef<'a>) -> Self {
        let parameter = value.parameter();
        Self {
            id: parameter.id(),
            default: parameter.default_normalized(),
            steps: parameter.step_count(),
            flags: parameter.flags(),
            title: parameter.title_utf16(),
            normalized: parameter.normalized(),
            plain: value.plain(),
            display: value.display_utf16(),
            hidden: value.hidden(),
        }
    }
}
fn unavailable_parameter() -> vst3_host::Error {
    vst3_host::Error::InvalidParameter(
        "VST3 current parameter presentation is unavailable".to_owned(),
    )
}
fn text(value: &[u16; 128], maximum: usize) -> vst3_host::Result<String> {
    let end = value.iter().position(|code| *code == 0).ok_or_else(unavailable_parameter)?;
    let mut result = String::new();
    // Validate and count before allocating; never truncate captions to fit Core.
    let bytes =
        char::decode_utf16(value[..end].iter().copied()).try_fold(0_usize, |size, code| {
            size.checked_add(code.map_err(|_| unavailable_parameter())?.len_utf8())
                .filter(|size| *size <= maximum)
                .ok_or_else(unavailable_parameter)
        })?;
    result.try_reserve_exact(bytes).map_err(|_| unavailable_parameter())?;
    if result.capacity() > maximum {
        return Err(unavailable_parameter());
    }
    for code in char::decode_utf16(value[..end].iter().copied()) {
        result.push(code.map_err(|_| unavailable_parameter())?);
    }
    Ok(result)
}
fn fact(
    expected: &Vst3ParameterDescriptor,
    revision: u64,
    value: CurrentParameter<'_>,
) -> vst3_host::Result<Fact> {
    validate_parameter(expected).map_err(|_| unavailable_parameter())?;
    let name = text(value.title, 255)?;
    if expected.read_only
        || expected.id != value.id
        || expected.name != name
        || expected.default_normalized.to_bits() != value.default.to_bits()
        || expected.step_count != value.steps
        || expected.flags != value.flags
        || !value.normalized.is_finite()
        || !(0.0..=1.0).contains(&value.normalized)
        || value.plain.is_some_and(|plain| !plain.is_finite())
    {
        return Err(unavailable_parameter());
    }
    let fact = Fact {
        revision,
        parameter: expected.parameter_id().map_err(|_| unavailable_parameter())?,
        event_flags: 0,
        value_bits: value.plain.map(f64::to_bits),
        origin: Origin::MainQuery { rescan_flags: 0 },
        hidden: Some(value.hidden),
        periodic: None,
        name: Some(name),
        module: None,
        info_flags: Some(value.flags),
        display: value.display.map(|value| text(value, 1023)).transpose()?,
    };
    fact.validate().map_err(|_| unavailable_parameter())?;
    Ok(fact)
}
type PresentationVisitor<'visitor> =
    dyn for<'parameter> FnMut(CurrentParameter<'parameter>) -> vst3_host::Result<()> + 'visitor;

fn capture_with(
    parameters: &[Vst3ParameterDescriptor],
    selected: &[u32],
    mut snapshot: impl FnMut() -> Option<Snapshot>,
    query: impl FnOnce(
        &mut ControllerParameterPresentationFrame,
        &mut PresentationVisitor<'_>,
    ) -> vst3_host::Result<usize>,
) -> Option<Presentations> {
    if parameters.len() > MAX_PARAMETERS
        || selected.len() > MAX_PARAMETERS
        || selected.is_empty()
        || selected.iter().enumerate().any(|(index, id)| selected[..index].contains(id))
    {
        return None;
    }
    let before = snapshot()?;
    if !before.healthy() {
        return None;
    }
    let mut facts = Vec::new();
    facts.try_reserve_exact(selected.len()).ok()?;
    if facts.capacity() > MAX_PARAMETERS {
        return None;
    }
    let mut frame = ParameterPresentationFrame::default();
    let count = query(&mut frame.controller, &mut |value| {
        let selected_index = selected
            .iter()
            .position(|id| *id == value.id)
            .ok_or_else(unavailable_parameter)?;
        if frame.seen[selected_index] {
            return Err(unavailable_parameter());
        }
        let expected = parameters
            .iter()
            .find(|expected| expected.id == value.id)
            .ok_or_else(unavailable_parameter)?;
        facts.push(fact(expected, before.source.revision(), value)?);
        frame.seen[selected_index] = true;
        // Include actual Vec and String capacities at each step, not lengths.
        mondrian_core::NativeAudioParameterObservation::validate_facts(&facts)
            .map_err(|_| unavailable_parameter())?;
        Ok(())
    })
    .ok()?;
    if count != parameters.len()
        || frame.seen[..selected.len()].iter().any(|seen| !seen)
        || snapshot()? != before
    {
        return None;
    }
    Some(Presentations {
        owner: before.owner,
        revision: before.source.revision(),
        facts,
    })
}
pub(super) fn capture(
    editor: &PluginEditorControl<'_>,
    parameters: &[Vst3ParameterDescriptor],
    selected: &[u32],
) -> Option<Presentations> {
    capture_with(
        parameters,
        selected,
        || Snapshot::capture(editor),
        |frame, visitor| {
            editor.visit_controller_parameter_presentations_bounded(
                MAX_PARAMETERS,
                selected,
                frame,
                &mut |_, value| visitor(value.into()),
            )
        },
    )
}

/// An optional presentation cannot change a successful author/ack result.
/// Control observations never query presentations; acknowledgements never run
/// a new getter after their irreversible operation.
pub(super) fn append_optional(
    observations: &mut crate::NativeAudioObservations,
    consumption: crate::NativeAudioObservationConsumption,
    no_retained_delivery_or_configuration: bool,
    query: impl FnOnce() -> Option<Presentations>,
) {
    use crate::NativeAudioObservationConsumption as Consumption;
    if matches!(
        consumption,
        Consumption::Preserve
            | Consumption::AcknowledgeAuthor(_)
            | Consumption::AcknowledgeReenteredAuthor(_)
    ) || !no_retained_delivery_or_configuration
        || !observations.author.as_ref().is_some_and(|author| {
            author.source.closed_empty()
                && matches!(
                    author.delivery,
                    crate::NativeAudioAuthorDeliveryState::Preserved
                )
        })
    {
        return;
    }
    if let Some(current) = query()
        && observations
            .object
            .is_some_and(|object| object.object_serial() == current.owner)
    {
        observations.parameter_revision = current.revision;
        observations.parameters = current.facts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeAudioObservationConsumption as Consumption;

    fn parameter(id: u32) -> Vst3ParameterDescriptor {
        Vst3ParameterDescriptor {
            id,
            name: "Gain".to_owned(),
            default_normalized: 0.5,
            step_count: 0,
            can_automate: true,
            read_only: false,
            flags: 1,
        }
    }
    fn wide(text: &str) -> [u16; 128] {
        let mut result = [0; 128];
        for (slot, code) in result.iter_mut().zip(text.encode_utf16()) {
            *slot = code;
        }
        result
    }
    fn current<'a>(
        expected: &Vst3ParameterDescriptor,
        title: &'a [u16; 128],
        display: &'a [u16; 128],
    ) -> CurrentParameter<'a> {
        CurrentParameter {
            id: expected.id,
            default: expected.default_normalized,
            steps: expected.step_count,
            flags: expected.flags,
            title,
            normalized: 0.25,
            plain: Some(250.0),
            display: Some(display),
            hidden: false,
        }
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            owner: NonZeroU64::new(7).unwrap(),
            source: crate::NativeAudioAuthorSourceStatus {
                revision: 3,
                pending_events: 0,
                pending_parameter_observations: 0,
                pending_dsp_feedback_values: 0,
                open_parameter_edits: 0,
                group_depth: 0,
                failure: None,
                parameter_flush_pending: false,
                publishing: false,
            },
            cohort: serde_json::from_str("0").unwrap(),
        }
    }
    fn observations() -> crate::NativeAudioObservations {
        let before = snapshot();
        crate::NativeAudioObservations {
            object: Some(crate::NativeAudioObjectObservation::new(
                std::num::NonZeroU32::new(42).unwrap(),
                before.owner,
            )),
            author: Some(crate::NativeAudioAuthorObservation {
                source: before.source,
                delivery: crate::NativeAudioAuthorDeliveryState::Preserved,
            }),
            ..Default::default()
        }
    }
    fn one() -> Presentations {
        let expected = parameter(0);
        capture_with(
            &[expected.clone()],
            &[0],
            || Some(snapshot()),
            |_, visit| {
                visit(current(&expected, &wide("Gain"), &wide("250 Hz")))?;
                Ok(1)
            },
        )
        .unwrap()
    }

    #[test]
    fn vst3_current_presentations_keep_native_plain_display_and_only_admitted_lanes() {
        let expected = parameter(0);
        let mut unsupported = parameter(99);
        unsupported.default_normalized = f64::NAN;
        let result = capture_with(
            &[expected.clone(), unsupported],
            &[0],
            || Some(snapshot()),
            |_, visit| {
                visit(current(&expected, &wide("Gain"), &wide("250 Hz")))?;
                Ok(2) // Full native census, not the one selected lane.
            },
        )
        .unwrap();
        assert_eq!(result.owner, snapshot().owner);
        assert_eq!(result.revision, 3);
        assert_eq!(result.facts.len(), 1);
        let actual = &result.facts[0];
        assert_eq!(actual.parameter, expected.parameter_id().unwrap());
        assert_eq!(actual.value_bits, Some(250.0_f64.to_bits()));
        assert_ne!(actual.value_bits, Some(0.25_f64.to_bits()));
        assert_eq!(actual.display.as_deref(), Some("250 Hz"));
        assert_eq!(actual.name.as_deref(), Some("Gain"));
        assert_eq!(actual.origin, Origin::MainQuery { rescan_flags: 0 });
        assert_eq!(
            (
                actual.event_flags,
                actual.info_flags,
                actual.hidden,
                actual.periodic
            ),
            (0, Some(1), Some(false), None)
        );
        assert_eq!(actual.module, None);
        let title = wide("Gain");
        let display = wide("250 Hz");
        let mut missing = current(&expected, &title, &display);
        missing.plain = None;
        missing.display = None;
        let unavailable = fact(&expected, 3, missing).unwrap();
        assert_eq!((unavailable.value_bits, unavailable.display), (None, None));
    }

    #[test]
    fn vst3_current_presentations_reject_duplicate_missing_and_metadata_drift() {
        let parameters = [parameter(0), parameter(1)];
        // Two visits alone cannot prove identity coverage: duplicate zero hides missing one.
        assert!(capture_with(
            &parameters,
            &[0, 1],
            || Some(snapshot()),
            |_, visit| {
                for _ in 0..2 {
                    visit(current(&parameters[0], &wide("Gain"), &wide("250 Hz")))?;
                }
                Ok(2)
            }
        )
        .is_none());
        assert!(capture_with(
            &parameters,
            &[0, 1],
            || Some(snapshot()),
            |_, visit| {
                visit(current(&parameters[0], &wide("Gain"), &wide("250 Hz")))?;
                Ok(2)
            }
        )
        .is_none());
        assert!(capture_with(
            &parameters,
            &[0, 0],
            || Some(snapshot()),
            |_, _| panic!("duplicate selection before query")
        )
        .is_none());
        assert!(
            capture_with(
                &parameters,
                &[0],
                || Some(snapshot()),
                |_, visit| {
                    visit(current(&parameters[0], &wide("Gain"), &wide("250 Hz")))?;
                    Ok(1)
                }
            )
            .is_none(),
            "native census shrank"
        );
        for drift in 0..7 {
            assert!(capture_with(
                &parameters[..1],
                &[0],
                || Some(snapshot()),
                |_, visit| {
                    let title = wide(if drift == 4 { "Other" } else { "Gain" });
                    let display = wide("250 Hz");
                    let mut value = current(&parameters[0], &title, &display);
                    match drift {
                        0 => value.id = 7,
                        1 => value.default = -0.0,
                        2 => value.steps = 1,
                        3 => value.flags = 3,
                        4 => {}
                        5 => value.normalized = f64::NAN,
                        6 => value.plain = Some(f64::INFINITY),
                        _ => unreachable!(),
                    }
                    visit(value)?;
                    Ok(1)
                }
            )
            .is_none());
        }
    }

    #[test]
    fn vst3_current_presentations_refuse_unhealthy_or_changed_owner_source_and_cohort() {
        let expected = parameter(0);
        let before = snapshot();
        for change in 0..10 {
            let mut after = before;
            match change {
                0 => after.owner = NonZeroU64::new(8).unwrap(),
                1 => after.source.revision += 1,
                2 => after.source.pending_events = 1,
                3 => after.source.pending_dsp_feedback_values = 1,
                4 => after.source.open_parameter_edits = 1,
                5 => after.source.group_depth = 1,
                6 => after.source.failure = Some(crate::NativeAudioAuthorSourceFailure::Overflow),
                7 => after.cohort = serde_json::from_str("4294967300").unwrap(),
                8 => after.cohort = serde_json::from_str("4294967296").unwrap(), // retired serial still changed
                9 => after.source.publishing = true,
                _ => unreachable!(),
            }
            let mut queries = 0;
            if !after.healthy() {
                assert!(capture_with(
                    &[expected.clone()],
                    &[0],
                    || Some(after),
                    |_, _| {
                        queries += 1;
                        Ok(1)
                    }
                )
                .is_none());
                assert_eq!(queries, 0);
            }
            let mut reads = 0;
            assert!(capture_with(
                &[expected.clone()],
                &[0],
                || {
                    reads += 1;
                    Some(if reads == 1 { before } else { after })
                },
                |_, visit| {
                    queries += 1;
                    visit(current(&expected, &wide("Gain"), &wide("250 Hz")))?;
                    Ok(1)
                }
            )
            .is_none());
            assert_eq!(reads, 2);
            assert_eq!(queries, 1);
        }
    }

    #[test]
    fn vst3_current_presentations_validate_utf16_count_and_aggregate_actual_storage() {
        let title = wide("Gain");
        assert!(text(&[0x41; 128], 255).is_err(), "missing terminator");
        let mut bad = [0; 128];
        bad[0] = 0xd800;
        assert!(text(&bad, 255).is_err());
        let expected = parameter(0);
        assert!(fact(
            &expected,
            3,
            CurrentParameter {
                display: Some(&bad),
                ..current(&expected, &title, &title)
            }
        )
        .is_err());
        let parameters = (0..MAX_PARAMETERS as u32).map(parameter).collect::<Vec<_>>();
        let selected = parameters.iter().map(|parameter| parameter.id).collect::<Vec<_>>();
        assert_eq!(
            capture_with(
                &parameters,
                &selected,
                || Some(snapshot()),
                |_, visit| {
                    for parameter in &parameters {
                        visit(current(parameter, &title, &title))?;
                    }
                    Ok(MAX_PARAMETERS)
                }
            )
            .unwrap()
            .facts
            .len(),
            MAX_PARAMETERS
        );
        let mut too_many = parameters.clone();
        too_many.push(parameter(1024));
        assert!(capture_with(
            &too_many,
            &selected,
            || Some(snapshot()),
            |_, _| panic!("count bound before query")
        )
        .is_none());
        let mut long = [0; 128];
        long[..85].fill(0x800); // Exact 255-byte UTF-8 caption, individually legal.
        let mut large = parameters;
        for parameter in &mut large {
            parameter.name = "\u{800}".repeat(85);
        }
        assert_eq!(text(&long, 255).unwrap().len(), 255);
        assert!(
            capture_with(
                &large,
                &selected,
                || Some(snapshot()),
                |_, visit| {
                    for parameter in &large {
                        visit(current(parameter, &long, &long))?;
                    }
                    Ok(MAX_PARAMETERS)
                }
            )
            .is_none(),
            "actual Vec plus String capacities exceed the shared aggregate bound"
        );
    }

    fn acknowledgements() -> [Consumption; 2] {
        let object = observations().object.unwrap();
        let binding = crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: object,
        };
        let receipt = crate::NativeAudioAuthorStateEntryReceipt {
            source_binding: binding,
            source: crate::NativeAudioAuthorStateEntrySource::CanonicalPeer { revision: 3 },
            object,
            configuration_sha256: [0; 32],
            configuration_bytes: 0,
            contract: AudioProcessorExecutionContract::new(
                0,
                AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .unwrap(),
        };
        [
            Consumption::AcknowledgeAuthor(crate::NativeAudioAuthorAcknowledgement {
                source_binding: binding,
                installed_binding: binding,
                revision: 3,
            }),
            Consumption::AcknowledgeReenteredAuthor(
                crate::NativeAudioReenteredAuthorAcknowledgement {
                    preparation: receipt,
                    installed_binding: binding,
                },
            ),
        ]
    }
    #[test]
    fn vst3_preserve_control_observation_does_not_query_optional_presentations() {
        let mut original = observations();
        let before = original.clone();
        let mut calls = 0;
        append_optional(&mut original, Consumption::Preserve, true, || {
            calls += 1;
            Some(one())
        });
        assert_eq!(
            calls, 0,
            "control preflight cannot query parameter presentations"
        );
        assert_eq!(
            original, before,
            "Preserve retains every original control fact"
        );

        let current = one();
        let mut expected = before;
        expected.parameter_revision = current.revision;
        expected.parameters = current.facts.clone();
        append_optional(&mut original, Consumption::Drain, true, || {
            calls += 1;
            Some(current)
        });
        assert_eq!(
            calls, 1,
            "the explicit diagnostic consumer still queries once"
        );
        assert_eq!(
            original, expected,
            "Drain retains the complete actual presentation"
        );
    }

    #[test]
    fn vst3_optional_presentations_never_query_after_ack_or_held_author_work() {
        for consumption in acknowledgements() {
            let mut original = observations();
            let before = original.clone();
            let mut calls = 0;
            append_optional(&mut original, consumption, true, || {
                calls += 1;
                Some(one())
            });
            assert_eq!(calls, 0);
            assert_eq!(original, before);
        }
        let mut original = observations();
        let before = original.clone();
        append_optional(&mut original, Consumption::Drain, false, || {
            panic!("held receipt or pending configuration cannot query")
        });
        assert_eq!(original, before);
        for busy in 0..3 {
            let mut original = observations();
            match busy {
                0 => {
                    original.author.as_mut().unwrap().delivery =
                        crate::NativeAudioAuthorDeliveryState::Refused(
                            crate::NativeAudioAuthorRefusal::SourceChanged,
                        )
                }
                1 => original.author.as_mut().unwrap().source.pending_events = 1,
                2 => original.author.as_mut().unwrap().source.pending_dsp_feedback_values = 1,
                _ => unreachable!(),
            }
            let before = original.clone();
            append_optional(&mut original, Consumption::Drain, true, || {
                panic!("pending author/feedback cannot query")
            });
            assert_eq!(original, before);
        }
    }
    #[test]
    fn vst3_optional_presentation_failure_preserves_original_author_observation() {
        let mut original = observations();
        let before = original.clone();
        let mut calls = 0;
        append_optional(&mut original, Consumption::Drain, true, || {
            calls += 1;
            None
        });
        assert_eq!(calls, 1);
        assert_eq!(original, before);
        append_optional(&mut original, Consumption::Drain, true, || {
            let mut wrong = one();
            wrong.owner = NonZeroU64::new(8).unwrap();
            Some(wrong)
        });
        assert_eq!(original, before);
        append_optional(&mut original, Consumption::Drain, true, || Some(one()));
        assert_eq!(original.object, before.object);
        assert_eq!(original.author, before.author);
        assert_eq!(original.pending_reprepare, before.pending_reprepare);
        assert_eq!(original.parameter_revision, 3);
        assert_eq!(original.parameters.len(), 1);
    }
    // Actual owned DLL controls for both existing main-role surfaces. This is a
    // child adapter qualification, not App GUI, author permission or native FFI
    // call counting. The fixture is never given a view/parent window here.
    #[cfg(target_os = "windows")]
    fn prepare_editor() -> Vst3Worker {
        let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(PathBuf::from)
            .expect("current normal Product helper");
        let binary = std::env::var_os("MONDRIAN_VST3_EDITOR_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("existing owned editor DLL");
        assert!(helper.is_absolute() && binary.is_absolute());
        let render = crate::AudioRenderContract {
            sample_rate: 48000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 8,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 64 * 1024 * 1024,
            public_output_lookahead_budget_frames: 0,
            compensation_delay_scratch_budget_bytes: 64 * 1024 * 1024,
        };
        // Independent existing MEV1 configuration. Mode1 has a native stepped
        // current value .5 normalized -> 1 plain; no view is opened or removed.
        let mut native = Vec::from(&b"MEV1"[..]);
        native.extend_from_slice(&1_u32.to_le_bytes());
        native.extend_from_slice(&0.5_f64.to_le_bytes());
        native.extend_from_slice(&0.5_f64.to_le_bytes());
        native.extend_from_slice(&1_u32.to_le_bytes());
        native.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(native.len(), 32);
        let mut state = Vec::from(&b"VST3HOST_STATE\0\0"[..]);
        state.extend_from_slice(&1_u32.to_le_bytes());
        state.extend_from_slice(&32_u32.to_le_bytes());
        state.extend_from_slice(&32_u32.to_le_bytes());
        state.extend_from_slice(&native);
        state.extend_from_slice(&native);
        assert_eq!(state.len(), 92);
        let registration =
            crate::processor_isolation::vst3_discovery::probe_vst3_plugin_registration(
                &helper,
                &binary,
                "C48B1B724CDE4A91A2C9C4EBB7DD1E83",
                render,
                Some(&state),
            )
            .expect("actual current Product probe");
        assert_eq!(registration.parameters.len(), 11);
        let selected = registration
            .parameters
            .iter()
            .filter(|parameter| !parameter.read_only)
            .map(|parameter| parameter.id)
            .collect::<Vec<_>>();
        assert_eq!(selected, [0, 1, 2], "all actual admitted lanes retained");
        let parameter_ids = registration
            .parameters
            .iter()
            .filter(|parameter| !parameter.read_only)
            .map(Vst3ParameterDescriptor::parameter_id)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let native_lane = native_worker_lane_profile_for_preparation(
            &registration.parameters,
            &crate::AudioProcessorNoteInputContract::default(),
            render,
        )
        .unwrap();
        let payload = Vst3Payload {
            schema_version: PAYLOAD_VERSION,
            main_audio_input: registration.main_audio_input,
            audio_output_count: registration.audio_output_count,
            event_bus_contract: registration.event_bus_contract,
            binary_path: registration.binary_path,
            class_id: registration.class_id,
            vendor: registration.vendor,
            binary_sha256: registration.binary_sha256,
            state: Some(state),
            parameters: registration.parameters,
            parameter_ids: selected,
        };
        let mut worker = Vst3AudioProcessorWorkerFactory
            .prepare_native(IsolatedAudioProcessorWorkerPrepareRequest {
                musical_time_reference: None,
                auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
                note_input_contract: crate::AudioProcessorNoteInputContract::default(),
                native_lane,
                payload: serde_json::to_vec(&payload).unwrap(),
                plugin_execution_contract: registration.execution_contract,
                auxiliary_inputs: registration.auxiliary_inputs,
                parameter_ids,
                render_contract: render,
            })
            .expect("actual owned DLL worker");
        let actual = worker.native_audio_object_observation().unwrap().unwrap();
        assert_eq!(actual.process_id().get(), std::process::id());
        let binding = crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: actual,
        };
        assert!(matches!(
            worker.bind_native_audio_object(binding).unwrap(),
            crate::NativeAudioObjectBindOutcome::Bound(_)
        ));
        worker
    }
    #[cfg(target_os = "windows")]
    fn assert_editor_facts(
        observed: &crate::NativeAudioObservations,
        actual: crate::NativeAudioObjectObservation,
    ) {
        assert_eq!(observed.object, Some(actual));
        assert!(observed.author.as_ref().unwrap().source.closed_empty());
        assert_eq!(observed.parameters.len(), 3);
        let expected = [
            ("vst3.param.0", 0.5),
            ("vst3.param.1", 0.5),
            ("vst3.param.2", 1.0),
        ];
        for (fact, (id, plain)) in observed.parameters.iter().zip(expected) {
            assert_eq!(fact.parameter.as_str(), id);
            assert_eq!(fact.value_bits, Some(f64::to_bits(plain)));
            assert_eq!(
                fact.display.as_deref(),
                Some("0.5"),
                "actual native formatting receives normalized .5"
            );
            assert_eq!(fact.revision, observed.parameter_revision);
            assert_eq!(fact.origin, Origin::MainQuery { rescan_flags: 0 });
            assert_eq!(fact.periodic, None);
            assert_eq!(fact.module, None);
        }
        assert_ne!(
            observed.parameters[2].value_bits,
            Some(0.5_f64.to_bits()),
            "plain is not normalized"
        );
        mondrian_core::NativeAudioParameterObservation::validate_facts(&observed.parameters)
            .unwrap();
    }
    #[cfg(target_os = "windows")]
    #[derive(Debug, PartialEq)]
    struct NativeSnapshot {
        object: crate::NativeAudioObjectObservation,
        source: vst3_host::plugin::NativeEditorEventSourceStatus,
        cohort: Option<RestartCohort>,
        state: Vec<u8>,
        last_end: Option<i64>,
        phase: InitialSignalPhase,
    }
    #[cfg(target_os = "windows")]
    fn native_snapshot(worker: &Vst3Worker) -> NativeSnapshot {
        let plugin = worker.plugin.as_ref().unwrap();
        NativeSnapshot {
            object: worker.native_audio_object_observation().unwrap().unwrap(),
            source: plugin.editor_event_source_status().unwrap(),
            cohort: plugin.pending_restart_cohort(),
            state: plugin.save_state().unwrap(),
            last_end: worker.last_end,
            phase: worker.initial_signal_phase,
        }
    }
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires current normal Product and owned editor DLL under native Job"]
    fn native_vst3_full_owner_parameter_presentations_use_actual_selected_lanes() {
        let mut worker = prepare_editor();
        let before = native_snapshot(&worker);
        let preserved = worker.poll_native_observations(Consumption::Preserve).unwrap();
        assert!(preserved.parameters.is_empty());
        assert_eq!(preserved.parameter_revision, 0);
        assert_eq!(native_snapshot(&worker), before);
        let observed = worker.poll_native_observations(Consumption::Drain).unwrap();
        assert_editor_facts(&observed, before.object);
        let mut control = observed.clone();
        control.parameters.clear();
        control.parameter_revision = 0;
        assert_eq!(
            preserved, control,
            "Drain changes only optional presentation facts"
        );
        assert_eq!(native_snapshot(&worker), before);
        let binding = worker.native_binding.unwrap();
        let request = crate::NativeAudioObjectBindRequest {
            binding: binding.binding(),
            expected_object: binding.object(),
        };
        let ack = crate::NativeAudioAuthorAcknowledgement {
            source_binding: request,
            installed_binding: request,
            revision: before.source.revision(),
        };
        let refused = worker.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
        assert!(matches!(
            refused.author.unwrap().delivery,
            crate::NativeAudioAuthorDeliveryState::Refused(
                crate::NativeAudioAuthorRefusal::StaleDelivery
            )
        ));
        assert!(refused.parameters.is_empty());
        assert_eq!(refused.parameter_revision, 0);
        assert_eq!(native_snapshot(&worker), before);
        assert_eq!(worker.native_binding, Some(binding));
        assert!(worker.native_author_delivery.is_none());
        assert!(worker.last_parameter_values.iter().all(Option::is_none));
    }
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires current normal Product and owned editor DLL under native Job"]
    fn native_vst3_live_main_preserves_control_facts_without_parameter_queries() {
        let mut worker = prepare_editor();
        let before = native_snapshot(&worker);
        let binding = worker.native_binding;
        let super::super::super::IsolatedAudioProcessorWorkerSplit { main, processing } =
            worker.split_native_processing().unwrap();
        let observed = main.preserve_native_observations().unwrap();
        assert_eq!(
            observed,
            crate::NativeAudioObservations {
                object: Some(before.object),
                author: Some(crate::NativeAudioAuthorObservation {
                    source: native_author::status(before.source).unwrap(),
                    delivery: crate::NativeAudioAuthorDeliveryState::Preserved,
                }),
                ..Default::default()
            }
        );
        assert_eq!(main.preserve_native_observations().unwrap(), observed);
        assert_eq!(main.pending_reprepare().unwrap(), None);
        drop(processing);
        drop(main);
        assert_eq!(native_snapshot(&worker), before);
        let presentation = worker.poll_native_observations(Consumption::Drain).unwrap();
        assert_editor_facts(&presentation, before.object);
        let mut control = presentation;
        control.parameters.clear();
        control.parameter_revision = 0;
        assert_eq!(
            observed, control,
            "joined diagnostics preserve the same control facts"
        );
        assert_eq!(native_snapshot(&worker), before);
        assert_eq!(worker.native_binding, binding);
        assert!(worker.native_author_delivery.is_none());
        assert!(worker.last_parameter_values.iter().all(Option::is_none));
    }
}
