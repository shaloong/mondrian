//! Initial-only queue construction and final no-native publication.
use super::*;
use crate::initial_parameter_input::{
    InitialParameterHandoffError as Failure, InitialParameterStorage as Storage,
    InitialParameterValue, MAX_INITIAL_PARAMETERS,
};
use crate::plugin::{
    NativeEditorEvent, NativeEditorEventSourceStatus, ParameterEditKind, RestartCohort,
};
use std::alloc::Layout;

fn queue_payload_layout() -> Result<Layout, Failure> {
    // ComWrapperInner is repr(C): public Header<C>, then C. No std Arc prefix.
    Layout::new::<vst3::com_scrape_types::Header<ParameterValueQueue>>()
        .extend(Layout::new::<ParameterValueQueue>())
        .map(|(layout, _)| layout.pad_to_align())
        .map_err(|_| Failure::InvalidParameters)
}
pub(crate) fn storage_request(count: usize) -> Result<Storage, Failure> {
    if count > MAX_INITIAL_PARAMETERS {
        return Err(Failure::InvalidParameters);
    }
    let pool = Layout::array::<ComWrapper<ParameterValueQueue>>(count)
        .map_err(|_| Failure::InvalidParameters)?
        .size();
    let points = Layout::array::<(i32, f64)>(count)
        .map_err(|_| Failure::InvalidParameters)?
        .size();
    Ok(Storage {
        vector_bytes: pool.checked_add(points).ok_or(Failure::InvalidParameters)?,
        com_payload_bytes: queue_payload_layout()?
            .size()
            .checked_mul(count)
            .ok_or(Failure::InvalidParameters)?,
    })
}

fn validate_values(values: &[InitialParameterValue]) -> Result<(), Failure> {
    if values.len() > MAX_INITIAL_PARAMETERS
        || values.iter().enumerate().any(|(index, value)| {
            !value.normalized.is_finite()
                || !(0.0..=1.0).contains(&value.normalized)
                || values[..index].iter().any(|prior| prior.id == value.id)
        })
    {
        return Err(Failure::InvalidParameters);
    }
    Ok(())
}

/// Owns new unpublished queues, then displaced old queues after publication.
/// Its caller-owned destructor runs only after all producer guards end.
struct PreparedInput {
    queues: Vec<ComWrapper<ParameterValueQueue>>,
    used: usize,
}
impl PreparedInput {
    fn new(values: &[InitialParameterValue], grant: Storage) -> Result<Self, Failure> {
        if !grant.admits(storage_request(values.len())?) {
            return Err(Failure::StorageCapacity);
        }
        validate_values(values)?;
        let mut prepared = Self {
            queues: Vec::new(),
            used: 0,
        };
        prepared
            .queues
            .try_reserve_exact(values.len())
            .map_err(|_| Failure::AllocationFailed)?;
        for value in values {
            let mut points = Vec::new();
            points
                .try_reserve_exact(1)
                .map_err(|_| Failure::AllocationFailed)?;
            prepared.queues.push(ComWrapper::new(ParameterValueQueue {
                param_id: AtomicU32::new(value.id),
                points: Mutex::new(points),
                origin: std::sync::atomic::AtomicU8::new(
                    super::ParameterQueueOrigin::Ordinary as u8,
                ),
            }));
        }
        if !grant.admits(prepared.census()?) {
            return Err(Failure::StorageCapacity);
        }
        Ok(prepared)
    }
    fn census(&self) -> Result<Storage, Failure> {
        let mut actual = Storage {
            vector_bytes: Layout::array::<ComWrapper<ParameterValueQueue>>(self.queues.capacity())
                .map_err(|_| Failure::InvalidParameters)?
                .size(),
            com_payload_bytes: 0,
        };
        let expected_payload = queue_payload_layout()?;
        for queue in &self.queues {
            let payload = queue.payload_storage();
            if payload.layout() != expected_payload {
                return Err(Failure::StorageCapacity);
            }
            actual.com_payload_bytes = actual
                .com_payload_bytes
                .checked_add(payload.layout().size())
                .ok_or(Failure::InvalidParameters)?;
            let points = queue.points.lock().map_err(|_| Failure::PoisonedOwner)?;
            actual.vector_bytes = actual
                .vector_bytes
                .checked_add(
                    Layout::array::<(i32, f64)>(points.capacity())
                        .map_err(|_| Failure::InvalidParameters)?
                        .size(),
                )
                .ok_or(Failure::InvalidParameters)?;
        }
        Ok(actual)
    }
    fn stage(&mut self, id: u32, value: f64) -> Result<(), Failure> {
        let index = self.queues[..self.used]
            .iter()
            .position(|queue| queue.param_id() == id);
        let index = if let Some(index) = index {
            index
        } else {
            if self.used == self.queues.len() {
                return Err(Failure::StorageCapacity);
            }
            let index = self.used;
            self.queues[index].param_id.store(id, Ordering::Relaxed);
            self.used += 1;
            index
        };
        // Unexposed private queues cannot receive native callbacks. All writes
        // and fallible point locks precede the final cohort CAS.
        let mut points = self.queues[index]
            .points
            .lock()
            .map_err(|_| Failure::PoisonedOwner)?;
        if let Some(point) = points.first_mut() {
            *point = (0, value);
        } else {
            if points.capacity() == 0 {
                return Err(Failure::StorageCapacity);
            }
            points.push((0, value));
        }
        Ok(())
    }
}
impl ComponentHandler {
    pub(crate) fn handoff_initial_parameters(
        &self,
        input: &ParameterChanges,
        cohort: RestartCohort,
        expected_source: NativeEditorEventSourceStatus,
        current_values: &[InitialParameterValue],
        grant: Storage,
    ) -> Result<(), Failure> {
        if thread::current().id() != self.control_thread {
            return Err(Failure::WrongThread);
        }
        if cohort.exhausted() || !matches!(cohort.flags().bits(), 0 | 4 | 20) {
            return Err(Failure::CohortChanged);
        }
        // Refuse retained old storage before allocating a candidate. The final
        // lock domain below rechecks this; no guard spans an allocation.
        {
            let pool = input.queues.lock().map_err(|_| Failure::PoisonedOwner)?;
            if input.used.load(Ordering::Relaxed) != 0 || pool.capacity() != 0 {
                return Err(Failure::InputPending);
            }
        }
        validate_values(current_values)?;
        let prepared_values = if expected_source.pending_dsp_feedback_values() == 0 {
            &[][..]
        } else {
            current_values
        };
        let mut candidate = PreparedInput::new(prepared_values, grant)?;
        // candidate remains caller-owned: COM/vector destructors run after the
        // closure has released every source, feedback and original-input guard.
        (|| {
            let mut events = self
                .editor_events
                .lock()
                .map_err(|_| Failure::PoisonedOwner)?;
            let mut feedback = self
                .parameter_changes
                .lock()
                .map_err(|_| Failure::PoisonedOwner)?;
            let mut old_pool = input.queues.lock().map_err(|_| Failure::PoisonedOwner)?;
            let mut source = events.status();
            source.pending_dsp_feedback_values = feedback.len();
            if source != expected_source
                || source.failure().is_some()
                || source.open_parameter_edits() != 0
                || source.group_depth() != 0
                // Revision does not reset on drain. This proves an intact
                // source history, not an unretired raw callback history.
                || usize::try_from(source.revision()).ok() != Some(events.events.len())
            {
                return Err(Failure::SourceChanged);
            }
            if input.used.load(Ordering::Relaxed) != 0 || old_pool.capacity() != 0 {
                return Err(Failure::InputPending);
            }
            let mut last_marker: Option<RestartCohort> = None;
            let mut value_count = 0;
            // Validate every kind and marker before staging. This conservative
            // initial subset permits public raw retirements between markers.
            for event in &events.events {
                match event {
                    NativeEditorEvent::Parameter(edit)
                        if edit.kind == ParameterEditKind::ValueChange && edit.value.is_some() =>
                    {
                        value_count += 1;
                    }
                    NativeEditorEvent::ParameterRescanRequested {
                        flags,
                        cohort: marker,
                    } => {
                        let post = marker.flags().bits();
                        if !matches!(*flags, 4 | 16 | 20)
                            || !matches!(post, 4 | 16 | 20)
                            || post & *flags != *flags
                            || marker.serial() == 0
                            || marker.exhausted()
                            || last_marker.is_some_and(|last| last.serial() >= marker.serial())
                        {
                            return Err(Failure::SourceChanged);
                        }
                        last_marker = Some(*marker);
                    }
                    _ => return Err(Failure::SourceChanged),
                }
            }
            if value_count != feedback.len()
                || match cohort.flags().bits() {
                    0 => last_marker.is_some(),
                    4 | 20 => last_marker != Some(cohort),
                    _ => true,
                }
            {
                return Err(Failure::SourceChanged);
            }
            let values = events.events.iter().filter_map(|event| match event {
                NativeEditorEvent::Parameter(edit) => Some(edit),
                _ => None,
            });
            for (edit, &(feedback_id, feedback_value)) in values.zip(feedback.iter()) {
                let value = edit.value.ok_or(Failure::SourceChanged)?;
                if edit.id != feedback_id
                    || value.to_bits() != feedback_value.to_bits()
                    || !current_values.iter().any(|current| {
                        current.id == edit.id && current.normalized.to_bits() == value.to_bits()
                    })
                {
                    return Err(Failure::SourceChanged);
                }
                candidate.stage(edit.id, value)?;
            }
            // Even zero-work closure proves the exact unchanged serial.
            // No allocation, locking, native call or fallible work follows.
            if !self
                .retire_restart_cohort_locked(&mut events, cohort)
                .map_err(|_| Failure::CohortChanged)?
            {
                return Err(Failure::CohortChanged);
            }
            std::mem::swap(&mut *old_pool, &mut candidate.queues);
            for queue in &old_pool[..candidate.used] {
                queue.publish_initial_origin();
            }
            input.used.store(candidate.used, Ordering::Relaxed);
            events.events.clear();
            feedback.clear();
            Ok(())
        })()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn handler() -> ComponentHandler {
        ComponentHandler::new(Arc::new(Mutex::new(Vec::with_capacity(
            MAX_EDITOR_FEEDBACK,
        ))))
    }
    fn value(id: u32, normalized: f64) -> InitialParameterValue {
        InitialParameterValue { id, normalized }
    }
    fn source_snapshot(
        handler: &ComponentHandler,
    ) -> (
        NativeEditorEventSourceStatus,
        Vec<NativeEditorEvent>,
        Vec<(u32, u64)>,
    ) {
        let mut events = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        let mut feedback = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        let status = handler
            .observe_editor_source_bounded(MAX_EDITOR_FEEDBACK, &mut events, &mut feedback)
            .unwrap();
        (
            status,
            events,
            feedback
                .into_iter()
                .map(|(id, value)| (id, value.to_bits()))
                .collect(),
        )
    }
    fn seed(handler: &ComponentHandler) {
        unsafe {
            assert_eq!(handler.performEdit(7, 0.25), kResultOk);
            assert_eq!(handler.restartComponent(20), kResultOk);
        }
    }

    #[test]
    fn initial_handoff_real_source_reaches_input_and_host_last_wins_preserves_output_semantics() {
        let handler = handler();
        seed(&handler);
        let source = handler.editor_event_source_status();
        let cohort = handler.pending_restart_cohort();
        let input = ParameterChanges::default();
        let values = [value(7, 0.25), value(9, 0.5)];
        handler
            .handoff_initial_parameters(
                &input,
                cohort,
                source,
                &values,
                storage_request(2).unwrap(),
            )
            .unwrap();
        assert!(handler.editor_event_source_status().closed_empty());
        assert_eq!(
            handler
                .editor_event_source_status()
                .pending_dsp_feedback_values(),
            0
        );
        assert_eq!(handler.pending_restart_cohort().serial(), cohort.serial());
        assert!(!handler.pending_restart_cohort().pending());
        assert!(input.has_active_points().unwrap());
        assert_eq!(input.used.load(Ordering::Relaxed), 1);
        {
            let queues = input.queues.lock().unwrap();
            assert!(queues[0].has_initial_origin());
            assert!(
                !queues[1].has_initial_origin(),
                "unused reserved queues gain no right"
            );
        }
        {
            let queues = input.queues.lock().unwrap();
            assert_eq!(*queues[0].points.lock().unwrap(), vec![(0, 0.25)]);
            assert!(queues[1].points.lock().unwrap().is_empty());
        }
        input.enqueue(7, 0, 0.75);
        input.enqueue(7, 3, 0.9);
        let queues = input.queues.lock().unwrap();
        assert_eq!(*queues[0].points.lock().unwrap(), vec![(0, 0.75), (3, 0.9)]);
        assert_eq!(queues.len(), 2);
        drop(queues);
        input.clear_all();
        assert!(!input.has_active_points().unwrap());
        // Actual plugin-side COM implementation keeps equal-offset points.
        let output = ParameterValueQueue::new(7);
        let mut index = -1;
        unsafe {
            assert_eq!(output.addPoint(0, 0.1, &mut index), kResultOk);
            assert_eq!(output.addPoint(0, 0.2, &mut index), kResultOk);
        }
        assert_eq!(*output.points.lock().unwrap(), vec![(0, 0.1), (0, 0.2)]);
    }

    #[test]
    fn initial_handoff_refuses_capacity_stale_unknown_and_changed_cohort_without_consumption() {
        for case in 0..5 {
            let handler = handler();
            seed(&handler);
            let source = handler.editor_event_source_status();
            let cohort = handler.pending_restart_cohort();
            let input = ParameterChanges::default();
            let mut values = [value(7, 0.25)];
            let mut grant = storage_request(1).unwrap();
            match case {
                0 => grant.vector_bytes -= 1,
                1 => grant.com_payload_bytes -= 1,
                2 => values[0].normalized = 0.75,
                3 => values[0].id = 99,
                4 => unsafe {
                    assert_eq!(handler.restartComponent(20), kResultOk);
                },
                _ => unreachable!(),
            }
            let before = source_snapshot(&handler);
            let retained_cohort = handler.pending_restart_cohort();
            assert!(handler
                .handoff_initial_parameters(&input, cohort, source, &values, grant)
                .is_err());
            assert_eq!(source_snapshot(&handler), before);
            assert_eq!(handler.pending_restart_cohort(), retained_cohort);
            assert!(input.queues.lock().unwrap().is_empty());
            assert!(!input.has_active_points().unwrap());
        }
    }

    #[test]
    fn initial_handoff_refuses_boundaries_and_existing_input_without_replacing_either_owner() {
        for boundary in [false, true] {
            let handler = handler();
            seed(&handler);
            let input = ParameterChanges::default();
            if boundary {
                unsafe {
                    assert_eq!(handler.beginEdit(7), kResultOk);
                }
            } else {
                input.enqueue(11, 2, 0.8);
            }
            let before = source_snapshot(&handler);
            let cohort = handler.pending_restart_cohort();
            // Zero grant would reject candidate allocation with StorageCapacity.
            // Existing input must instead win the pre-allocation preflight.
            let grant = if boundary {
                storage_request(1).unwrap()
            } else {
                Storage::default()
            };
            assert_eq!(
                handler.handoff_initial_parameters(
                    &input,
                    cohort,
                    before.0,
                    &[value(7, 0.25)],
                    grant,
                ),
                Err(if boundary {
                    Failure::SourceChanged
                } else {
                    Failure::InputPending
                })
            );
            assert_eq!(source_snapshot(&handler), before);
            assert_eq!(handler.pending_restart_cohort(), cohort);
            if !boundary {
                assert_eq!(
                    *input.queues.lock().unwrap()[0].points.lock().unwrap(),
                    vec![(2, 0.8)]
                );
            } else {
                assert!(input.queues.lock().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn initial_handoff_poison_and_wrong_thread_cannot_consume_source_or_flags() {
        let handler = handler();
        seed(&handler);
        let input = ParameterChanges::default();
        let before = source_snapshot(&handler);
        let cohort = handler.pending_restart_cohort();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = input.queues.lock().unwrap();
            panic!("owned input poison control");
        }));
        assert_eq!(
            handler.handoff_initial_parameters(
                &input,
                cohort,
                before.0,
                &[value(7, 0.25)],
                storage_request(1).unwrap()
            ),
            Err(Failure::PoisonedOwner)
        );
        assert_eq!(source_snapshot(&handler), before);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    assert_eq!(
                        handler.handoff_initial_parameters(
                            &input,
                            cohort,
                            before.0,
                            &[value(7, 0.25)],
                            storage_request(1).unwrap()
                        ),
                        Err(Failure::WrongThread)
                    );
                })
                .join()
                .unwrap();
        });
        assert_eq!(source_snapshot(&handler), before);
        assert_eq!(handler.pending_restart_cohort(), cohort);
    }

    #[test]
    fn initial_handoff_zero_work_and_request_bounds_do_not_allocate_a_point_pool() {
        assert_eq!(storage_request(0).unwrap(), Storage::default());
        assert!(storage_request(MAX_INITIAL_PARAMETERS + 1).is_err());
        let handler = handler();
        unsafe {
            handler.restartComponent(4);
        }
        let input = ParameterChanges::default();
        let source = handler.editor_event_source_status();
        let cohort = handler.pending_restart_cohort();
        for values in [
            vec![value(7, f64::NAN)],
            vec![value(7, 0.5); MAX_INITIAL_PARAMETERS + 1],
        ] {
            assert_eq!(
                handler.handoff_initial_parameters(
                    &input,
                    cohort,
                    source,
                    &values,
                    Storage::default(),
                ),
                Err(Failure::InvalidParameters)
            );
            assert_eq!(handler.editor_event_source_status(), source);
            assert_eq!(handler.pending_restart_cohort(), cohort);
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
        }
        handler
            .handoff_initial_parameters(
                &input,
                handler.pending_restart_cohort(),
                handler.editor_event_source_status(),
                &[value(7, 0.5)],
                Storage::default(),
            )
            .unwrap();
        assert!(input.queues.lock().unwrap().is_empty());
        assert!(!input.has_active_points().unwrap());
        assert!(!handler.pending_restart_cohort().pending());
        assert!(PreparedInput::new(&[value(7, f64::NAN)], storage_request(1).unwrap()).is_err());
        assert!(
            PreparedInput::new(&[value(7, 0.5), value(7, 0.5)], storage_request(2).unwrap())
                .is_err()
        );
    }

    #[test]
    fn initial_handoff_marker_only_or_zero_work_does_not_replay_current_values() {
        for flags in [0, 4, 20] {
            let handler = handler();
            unsafe {
                assert_eq!(handler.restartComponent(flags), kResultOk);
            }
            let input = ParameterChanges::default();
            let source = handler.editor_event_source_status();
            let cohort = handler.pending_restart_cohort();
            assert_eq!(source.pending_dsp_feedback_values(), 0);
            handler
                .handoff_initial_parameters(
                    &input,
                    cohort,
                    source,
                    &[value(7, 0.5), value(9, 0.75)],
                    Storage::default(),
                )
                .unwrap();
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
            assert_eq!(input.used.load(Ordering::Relaxed), 0);
            assert!(handler.editor_event_source_status().closed_empty());
            assert_eq!(
                handler.editor_event_source_status().revision(),
                source.revision()
            );
            assert_eq!(handler.pending_restart_cohort().serial(), cohort.serial());
            assert!(!handler.pending_restart_cohort().pending());
        }
    }

    #[test]
    fn initial_handoff_accepts_intact_parameter_history_across_legal_raw_retirements() {
        for prefix in [8, 16] {
            let handler = handler();
            unsafe {
                assert_eq!(handler.restartComponent(prefix), kResultOk);
            }
            assert_eq!(handler.take_restart_flags().bits(), prefix);
            // A non-parameter callback can advance the raw serial without an
            // author event. Retiring it does not make a parameter source gap.
            unsafe {
                assert_eq!(handler.restartComponent(8), kResultOk);
            }
            assert_eq!(handler.take_restart_flags().bits(), 8);
            unsafe {
                assert_eq!(handler.restartComponent(4), kResultOk);
            }
            let source = handler.editor_event_source_status();
            let cohort = handler.pending_restart_cohort();
            assert_eq!(cohort.serial(), 3);
            assert_eq!(source.pending_events(), if prefix == 16 { 2 } else { 1 });
            let input = ParameterChanges::default();
            handler
                .handoff_initial_parameters(
                    &input,
                    cohort,
                    source,
                    &[value(7, 0.5)],
                    Storage::default(),
                )
                .unwrap();
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
            assert!(handler.editor_event_source_status().closed_empty());
            assert!(!handler.pending_restart_cohort().pending());
        }
    }

    #[test]
    fn initial_handoff_refuses_drained_or_previously_consumed_history_without_mutation() {
        for consume in [false, true] {
            let handler = handler();
            let input = ParameterChanges::default();
            unsafe {
                assert_eq!(handler.restartComponent(4), kResultOk);
            }
            if consume {
                handler
                    .handoff_initial_parameters(
                        &input,
                        handler.pending_restart_cohort(),
                        handler.editor_event_source_status(),
                        &[],
                        Storage::default(),
                    )
                    .unwrap();
            } else {
                assert!(matches!(handler.take_editor_events(),
                    crate::plugin::NativeEditorEventDrain::Complete { events } if events.len() == 1));
            }
            unsafe {
                assert_eq!(handler.restartComponent(4), kResultOk);
            }
            let before = source_snapshot(&handler);
            let cohort = handler.pending_restart_cohort();
            assert_eq!(
                handler.handoff_initial_parameters(
                    &input,
                    cohort,
                    before.0,
                    &[],
                    Storage::default()
                ),
                Err(Failure::SourceChanged)
            );
            assert_eq!(source_snapshot(&handler), before);
            assert_eq!(handler.pending_restart_cohort(), cohort);
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
        }
    }

    #[test]
    fn initial_handoff_reentrant_same_bit_restart_preserves_every_owner() {
        let handler = handler();
        let input = ParameterChanges::default();
        unsafe {
            assert_eq!(handler.restartComponent(4), kResultOk);
        }
        let source = handler.editor_event_source_status();
        let cohort = handler.pending_restart_cohort();
        // Models a native callback during the caller's real metadata query.
        unsafe {
            assert_eq!(handler.restartComponent(4), kResultOk);
        }
        let before = source_snapshot(&handler);
        let current = handler.pending_restart_cohort();
        assert_eq!(
            handler.handoff_initial_parameters(&input, cohort, source, &[], Storage::default()),
            Err(Failure::SourceChanged)
        );
        assert_eq!(source_snapshot(&handler), before);
        assert_eq!(handler.pending_restart_cohort(), current);
        assert_eq!(input.queues.lock().unwrap().capacity(), 0);
    }

    #[test]
    fn initial_handoff_rejects_unsupported_kinds_and_incomplete_marker_prefix_before_staging() {
        for case in 0..7 {
            let handler = handler();
            seed(&handler);
            match case {
                0 => unsafe {
                    assert_eq!(handler.setDirty(0), kResultOk);
                },
                1 => {
                    handler.editor_events.lock().unwrap().events.pop();
                }
                2 => {
                    let mut source = handler.editor_events.lock().unwrap();
                    let NativeEditorEvent::ParameterRescanRequested { flags, .. } =
                        &mut source.events[1]
                    else {
                        panic!("real marker")
                    };
                    *flags = 8;
                }
                3 => {
                    let mut source = handler.editor_events.lock().unwrap();
                    let NativeEditorEvent::ParameterRescanRequested { cohort, .. } =
                        &mut source.events[1]
                    else {
                        panic!("real marker")
                    };
                    *cohort = RestartCohort::from_word(20);
                }
                4 => {
                    unsafe {
                        assert_eq!(handler.restartComponent(4), kResultOk);
                    }
                    handler.editor_events.lock().unwrap().events.swap(1, 2);
                }
                5 => {
                    assert_eq!(handler.take_restart_flags().bits(), 20);
                }
                6 => {
                    handler.parameter_changes.lock().unwrap()[0].1 = 0.5;
                }
                _ => unreachable!(),
            }
            let before = source_snapshot(&handler);
            let cohort = handler.pending_restart_cohort();
            let input = ParameterChanges::default();
            assert_eq!(
                handler.handoff_initial_parameters(
                    &input,
                    cohort,
                    before.0,
                    &[value(7, 0.25)],
                    storage_request(1).unwrap()
                ),
                Err(Failure::SourceChanged)
            );
            assert_eq!(source_snapshot(&handler), before);
            assert_eq!(handler.pending_restart_cohort(), cohort);
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
        }
    }

    #[test]
    fn initial_handoff_zero_flags_transfers_only_real_feedback_once() {
        let handler = handler();
        unsafe {
            assert_eq!(handler.performEdit(7, 0.25), kResultOk);
        }
        let input = ParameterChanges::default();
        let source = handler.editor_event_source_status();
        let cohort = handler.pending_restart_cohort();
        assert_eq!(cohort.flags().bits(), 0);
        handler
            .handoff_initial_parameters(
                &input,
                cohort,
                source,
                &[value(7, 0.25), value(9, 0.5)],
                storage_request(2).unwrap(),
            )
            .unwrap();
        assert_eq!(input.used.load(Ordering::Relaxed), 1);
        {
            let queues = input.queues.lock().unwrap();
            assert_eq!(queues.len(), 2);
            assert_eq!(queues[0].param_id(), 7);
            assert_eq!(*queues[0].points.lock().unwrap(), [(0, 0.25)]);
            assert!(queues[1].points.lock().unwrap().is_empty());
        }
        let after = source_snapshot(&handler);
        assert!(after.0.closed_empty());
        assert_eq!(after.0.pending_dsp_feedback_values(), 0);
        assert_eq!(
            handler.handoff_initial_parameters(
                &input,
                cohort,
                after.0,
                &[value(7, 0.25)],
                storage_request(1).unwrap()
            ),
            Err(Failure::InputPending)
        );
        assert_eq!(source_snapshot(&handler), after);
        assert_eq!(input.used.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn initial_handoff_uses_locked_retirement_lineage_and_keeps_zero_claim_unmodified() {
        for flags in [0, 4, 20] {
            let handler = handler();
            unsafe {
                assert_eq!(handler.restartComponent(flags), kResultOk);
            }
            let input = ParameterChanges::default();
            handler
                .handoff_initial_parameters(
                    &input,
                    handler.pending_restart_cohort(),
                    handler.editor_event_source_status(),
                    &[],
                    Storage::default(),
                )
                .unwrap();
            assert_eq!(
                handler.editor_events.lock().unwrap().retirement_lineage,
                u64::from(flags != 0)
            );
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
        }
        for flags in [0, 4, 20] {
            let handler = handler();
            unsafe {
                assert_eq!(handler.restartComponent(flags), kResultOk);
            }
            handler.editor_events.lock().unwrap().retirement_lineage = u64::MAX;
            let before = source_snapshot(&handler);
            let cohort = handler.pending_restart_cohort();
            let input = ParameterChanges::default();
            let result = handler.handoff_initial_parameters(
                &input,
                cohort,
                before.0,
                &[],
                Storage::default(),
            );
            if flags == 0 {
                assert_eq!(result, Ok(()));
            } else {
                assert_eq!(result, Err(Failure::CohortChanged));
                assert_eq!(source_snapshot(&handler), before);
                assert_eq!(handler.pending_restart_cohort(), cohort);
            }
            assert_eq!(
                handler.editor_events.lock().unwrap().retirement_lineage,
                u64::MAX
            );
            assert_eq!(input.queues.lock().unwrap().capacity(), 0);
        }
    }
}
