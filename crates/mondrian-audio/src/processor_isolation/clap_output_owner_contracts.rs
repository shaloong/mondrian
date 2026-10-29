//! New typed owner controls; these are not advertised as old-API behavioral RED.
use super::super::clap_output_events::*;
use super::super::ClapParameterDescriptor;
use clack_host::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamModEvent, ParamValueEvent,
};
use clack_host::events::{Event, EventFlags};
use clack_host::prelude::{ClapId, Pckn};

fn parameters() -> Vec<ClapParameterDescriptor> {
    [(17, false, false), (3, true, false), (9, false, true)]
        .into_iter()
        .map(|(id, read_only, stepped)| ClapParameterDescriptor {
            id,
            name: format!("parameter {id}"),
            min_value: 0.0,
            max_value: if stepped { 4.0 } else { 1.0 },
            default_value: 0.0,
            current_value: 0.0,
            automatable: !read_only,
            stepped,
            enumeration: false,
            read_only,
            hidden: false,
        })
        .collect()
}
fn process_origin(start: i64) -> ClapOutputOrigin {
    ClapOutputOrigin::Process {
        start_sample: start,
        sample_rate: 48_000,
        frames: 4,
    }
}

#[test]
fn clap_output_owner_preserves_plain_values_flags_readonly_offsets_and_actual_boundaries() {
    let mut owner = ClapOutputEvents::new(
        &parameters(),
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new()),
    )
    .expect("admitted output storage");
    let before = owner.allocated_bytes().expect("exact capacity bytes");
    {
        let mut writer = owner.writer(process_origin(-7)).expect("exclusive callback publication");
        let mut output = writer.as_output();
        // A legal Value does not require a synthetic enclosing gesture.
        output
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(17),
                Pckn::match_all(),
                0.125,
            ))
            .expect("unwrapped value");
        output
            .try_push(ParamGestureBeginEvent::new(1, ClapId::new(17)))
            .expect("actual begin");
        output
            .try_push(
                ParamValueEvent::new(1, ClapId::new(17), Pckn::match_all(), 0.5)
                    .with_flags(EventFlags::IS_LIVE | EventFlags::DONT_RECORD),
            )
            .expect("exact flags");
        output
            .try_push(ParamGestureEndEvent::new(1, ClapId::new(17)))
            .expect("actual end");
        output
            .try_push(ParamValueEvent::new(
                3,
                ClapId::new(3),
                Pckn::match_all(),
                0.25,
            ))
            .expect("readonly native value");
    }
    let events = owner.events();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events.iter().map(|event| event.sequence).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(
        events.iter().map(|event| event.sample_offset).collect::<Vec<_>>(),
        [0, 1, 1, 1, 3]
    );
    assert_eq!(events[0].origin, process_origin(-7));
    assert_eq!(events[0].kind, ClapOutputParameterKind::Value(0.125));
    assert_eq!(events[1].kind, ClapOutputParameterKind::GestureBegin);
    assert_eq!(
        events[2].flags,
        (EventFlags::IS_LIVE | EventFlags::DONT_RECORD).bits()
    );
    assert_eq!(events[3].kind, ClapOutputParameterKind::GestureEnd);
    assert!(events[4].read_only);
    assert_eq!(owner.allocated_bytes().expect("same capacities"), before);
}

#[test]
fn clap_output_owner_retains_complete_capacity_and_refuses_without_clearing_previous_events() {
    let mut owner = ClapOutputEvents::new(
        &parameters(),
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new()),
    )
    .expect("storage");
    let bytes = owner.allocated_bytes().expect("capacity");
    for index in 0..CLAP_OUTPUT_EVENTS {
        let mut writer = owner
            .writer(process_origin(index as i64))
            .expect("exclusive callback publication");
        writer
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(17),
                Pckn::match_all(),
                0.5,
            ))
            .expect("every already admitted slot");
    }
    let before = owner.events().to_vec();
    {
        let mut writer = owner
            .writer(ClapOutputOrigin::InactiveFlush)
            .expect("exclusive callback publication");
        assert!(writer
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(17),
                Pckn::match_all(),
                0.75
            ))
            .is_err());
    }
    assert_eq!(owner.events(), before);
    assert!(owner.refused(ClapOutputRefusal::Capacity));
    assert_eq!(owner.allocated_bytes().expect("no growth"), bytes);
}

#[test]
fn clap_output_owner_rejects_unknown_pervoice_modulation_invalid_values_and_keeps_refusal_facts() {
    let mut owner = ClapOutputEvents::new(
        &parameters(),
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new()),
    )
    .expect("storage");
    {
        let mut writer = owner.writer(process_origin(0)).expect("exclusive callback publication");
        let mut output = writer.as_output();
        assert!(output
            .try_push(ParamModEvent::new(
                0,
                ClapId::new(17),
                Pckn::match_all(),
                0.25
            ))
            .is_err());
        assert!(output
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(17),
                Pckn::new(0u16, 0u16, 60u16, 4u32),
                0.25
            ))
            .is_err());
        assert!(output
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(404),
                Pckn::match_all(),
                0.25
            ))
            .is_err());
        for value in [f64::NAN, f64::INFINITY, -0.125, 1.125] {
            assert!(output
                .try_push(ParamValueEvent::new(
                    0,
                    ClapId::new(17),
                    Pckn::match_all(),
                    value
                ))
                .is_err());
        }
        assert!(output
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(9),
                Pckn::match_all(),
                1.5
            ))
            .is_err());
        output
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(9),
                Pckn::match_all(),
                2.0,
            ))
            .expect("actual integer plain value");
    }
    assert!(owner.refused(ClapOutputRefusal::UnsupportedEvent));
    assert!(owner.refused(ClapOutputRefusal::PerVoiceParameter));
    assert!(owner.refused(ClapOutputRefusal::UnknownParameter));
    assert!(owner.refused(ClapOutputRefusal::InvalidValue));
    assert_eq!(owner.events().len(), 1);
}

#[test]
fn clap_output_owner_observes_origin_order_and_signed_sample_overflow() {
    let mut owner = ClapOutputEvents::new(
        &parameters(),
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new()),
    )
    .expect("storage");
    {
        let mut writer = owner.writer(process_origin(10)).expect("exclusive callback publication");
        let mut output = writer.as_output();
        output
            .try_push(ParamValueEvent::new(
                2,
                ClapId::new(17),
                Pckn::match_all(),
                0.5,
            ))
            .expect("offset2");
        assert!(output
            .try_push(ParamValueEvent::new(
                1,
                ClapId::new(17),
                Pckn::match_all(),
                0.5
            ))
            .is_err());
        assert!(output
            .try_push(ParamValueEvent::new(
                4,
                ClapId::new(17),
                Pckn::match_all(),
                0.5
            ))
            .is_err());
    }
    {
        let mut writer =
            owner.writer(process_origin(i64::MAX)).expect("exclusive callback publication");
        assert!(writer
            .as_output()
            .try_push(ParamValueEvent::new(
                1,
                ClapId::new(17),
                Pckn::match_all(),
                0.5
            ))
            .is_err());
    }
    assert_eq!(owner.events().len(), 1);
    assert!(owner.refused(ClapOutputRefusal::InvalidOrder));
    assert!(owner.refused(ClapOutputRefusal::InvalidTime));
}

#[test]
fn clap_output_owner_flush_preserves_raw_offset_without_inventing_a_timeline_and_keeps_prior_process(
) {
    let mut owner = ClapOutputEvents::new(
        &parameters(),
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new()),
    )
    .expect("storage");
    for origin in [
        process_origin(-3),
        ClapOutputOrigin::InactiveFlush,
        ClapOutputOrigin::ActiveFlush,
    ] {
        let mut writer = owner.writer(origin).expect("exclusive callback publication");
        writer
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(17),
                Pckn::match_all(),
                0.5,
            ))
            .expect("actual call");
    }
    {
        let mut writer = owner
            .writer(ClapOutputOrigin::InactiveFlush)
            .expect("exclusive callback publication");
        writer
            .as_output()
            .try_push(ParamValueEvent::new(
                123,
                ClapId::new(17),
                Pckn::match_all(),
                0.5,
            ))
            .expect("flush timing is lost, raw offset remains observable");
    }
    assert_eq!(owner.events().len(), 4);
    assert_eq!(owner.events()[0].origin, process_origin(-3));
    assert_eq!(owner.events()[1].origin, ClapOutputOrigin::InactiveFlush);
    assert_eq!(owner.events()[2].origin, ClapOutputOrigin::ActiveFlush);
    assert_eq!(owner.events()[2].sequence, 3);
    assert_eq!(owner.events()[3].sample_offset, 123);
    assert_eq!(owner.events()[3].origin, ClapOutputOrigin::InactiveFlush);
}

#[test]
fn clap_output_owner_exact_empty_capacity_reservation_is_reported_and_schema_is_validated() {
    let owner = ClapOutputEvents::new(
        &parameters(),
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new()),
    )
    .expect("storage");
    assert!(owner.events().is_empty());
    assert_eq!(
        owner.allocated_bytes().expect("actual owners"),
        ClapOutputEvents::reservation_bytes(3).expect("grant")
    );
    assert!(
        owner.allocated_bytes().expect("not empty usage")
            >= CLAP_OUTPUT_EVENTS * size_of::<ClapOutputParameterEvent>()
    );
    let mut invalid = parameters();
    invalid[1].id = invalid[0].id;
    assert!(ClapOutputEvents::new(
        &invalid,
        std::sync::Arc::new(super::super::clap_parameter_source::ClapParameterSource::new())
    )
    .is_err());
}
