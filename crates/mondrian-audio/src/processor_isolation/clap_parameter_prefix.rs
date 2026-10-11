//! One ordered prefix over the existing retained output and main control owners.
//! No event is consumed until its exact same-object consumer acknowledges it.

use super::clap_output_events::{ClapOutputEvents, ClapOutputParameterEvent};
use super::clap_parameter_source::{
    ClapParameterControl, ClapParameterControlSource, ClapParameterSource,
};
use crate::NativeAudioAuthorRefusal;
use std::alloc::Layout;
use std::sync::Arc;

/// One bounded response prefix; later callbacks stay in their original owners.
pub(super) const CLAP_PARAMETER_PREFIX_EVENTS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ClapParameterRecord {
    Output(ClapOutputParameterEvent),
    Control(ClapParameterControl),
}

impl ClapParameterRecord {
    pub(super) const fn sequence(self) -> u64 {
        match self {
            Self::Output(event) => event.sequence,
            Self::Control(event) => event.sequence,
        }
    }
}

/// Private source retirement key; copies cannot authorize a native or author edit.
#[derive(Clone, Copy)]
pub(super) struct ClapParameterPrefixKey {
    pub(super) output_through: Option<u64>,
    pub(super) output_count: usize,
    pub(super) control_through: Option<u64>,
    pub(super) control_count: usize,
    pub(super) revision: u64,
}

/// Actual prepared record allocation and the strong source lifetime it belongs to.
pub(super) struct PreparedClapParameterPrefix {
    pub(super) records: Vec<ClapParameterRecord>,
    pub(super) source: Arc<ClapParameterSource>,
    pub(super) key: ClapParameterPrefixKey,
    pub(super) covers_current_source: bool,
}

impl PreparedClapParameterPrefix {
    /// Called only on the full main owner after the same processing role joins.
    /// Each lane already preserves sequence order; their merge is linear in the
    /// selected response size, with no secondary callback interpretation.
    pub(super) fn prepare(
        source: Arc<ClapParameterSource>,
        output: &ClapOutputEvents,
        controls: &ClapParameterControlSource<'_>,
    ) -> Result<Option<Self>, NativeAudioAuthorRefusal> {
        if !std::ptr::eq(Arc::as_ptr(&source), output.source()) || !controls.belongs_to(&source) {
            return Err(NativeAudioAuthorRefusal::OwnerChanged);
        }
        let observed = source.snapshot();
        if observed.publishing_output || observed.incomplete != 0 {
            return Err(NativeAudioAuthorRefusal::InvalidSource);
        }
        let (main, main_count) = controls.snapshot();
        let events = output.events();
        if observed.pending_output as usize != events.len()
            || observed.pending_controls as usize != main_count
            || main[..main_count].iter().any(Option::is_none)
        {
            return Err(NativeAudioAuthorRefusal::InvalidSource);
        }
        let total = events
            .len()
            .checked_add(main_count)
            .ok_or(NativeAudioAuthorRefusal::ResourceLimit)?;
        if total == 0 {
            return Ok(None);
        }
        let selected = total.min(CLAP_PARAMETER_PREFIX_EVENTS);
        let mut records = Vec::new();
        records
            .try_reserve_exact(selected)
            .map_err(|_| NativeAudioAuthorRefusal::ResourceLimit)?;
        if records.capacity() > CLAP_PARAMETER_PREFIX_EVENTS {
            return Err(NativeAudioAuthorRefusal::ResourceLimit);
        }
        let mut key = ClapParameterPrefixKey {
            output_through: None,
            output_count: 0,
            control_through: None,
            control_count: 0,
            revision: 0,
        };
        for _ in 0..selected {
            let audio = events.get(key.output_count).copied();
            let main = main.get(key.control_count).copied().flatten();
            let record = match (audio, main) {
                (Some(audio), Some(main)) if audio.sequence == main.sequence => {
                    return Err(NativeAudioAuthorRefusal::InvalidSource);
                }
                (Some(audio), Some(main)) if main.sequence < audio.sequence => {
                    ClapParameterRecord::Control(main)
                }
                (Some(audio), _) => ClapParameterRecord::Output(audio),
                (None, Some(main)) => ClapParameterRecord::Control(main),
                (None, None) => return Err(NativeAudioAuthorRefusal::InvalidSource),
            };
            if record.sequence() <= key.revision || record.sequence() > observed.revision {
                return Err(NativeAudioAuthorRefusal::InvalidSource);
            }
            key.revision = record.sequence();
            match record {
                ClapParameterRecord::Output(event) => {
                    key.output_count += 1;
                    key.output_through = Some(event.sequence);
                }
                ClapParameterRecord::Control(event) => {
                    key.control_count += 1;
                    key.control_through = Some(event.sequence);
                }
            }
            records.push(record);
            if matches!(
                record,
                ClapParameterRecord::Control(ClapParameterControl {
                    kind: super::clap_parameter_source::ClapParameterControlKind::Rescan { .. },
                    ..
                })
            ) {
                break;
            }
        }
        if source.snapshot() != observed {
            return Err(NativeAudioAuthorRefusal::SourceChanged);
        }
        let covers_current_source = records.len() == total;
        Ok(Some(Self { records, source, key, covers_current_source }))
    }

    pub(super) fn allocated_bytes(&self) -> Result<usize, NativeAudioAuthorRefusal> {
        Layout::array::<ClapParameterRecord>(self.records.capacity())
            .map(|layout| layout.size())
            .map_err(|_| NativeAudioAuthorRefusal::ResourceLimit)
    }

    /// Native caller must first check its retained binding and revision. This
    /// private method then validates both actual local prefixes before retiring
    /// either one. New callbacks, open boundaries and sticky loss are preserved.
    pub(super) fn acknowledge(
        self,
        output: &mut ClapOutputEvents,
        controls: &ClapParameterControlSource<'_>,
    ) -> Result<(), Self> {
        if !std::ptr::eq(Arc::as_ptr(&self.source), output.source())
            || !controls.belongs_to(&self.source)
            || !super::clap_output_events::acknowledge_parameter_prefix(
                output,
                controls,
                self.key.output_through,
                self.key.output_count,
                self.key.control_through,
                self.key.control_count,
            )
        {
            return Err(self);
        }
        Ok(())
    }
}

#[cfg(test)]
mod ordered_prefix_contracts {
    use super::super::clap_parameter_source::ClapParameterControlKind as Control;
    use super::*;
    use clack_host::events::event_types::ParamValueEvent;
    use clack_host::prelude::{ClapId, Pckn};
    fn descriptor() -> super::super::ClapParameterDescriptor {
        super::super::ClapParameterDescriptor {
            id: 17,
            name: "gain".into(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 0.25,
            current_value: 0.25,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        }
    }
    fn value(owner: &mut ClapOutputEvents, value: f64) {
        owner
            .writer(super::super::clap_output_events::ClapOutputOrigin::ActiveFlush)
            .expect("actual exclusive output owner")
            .as_output()
            .try_push(ParamValueEvent::new(
                0,
                ClapId::new(17),
                Pckn::match_all(),
                value,
            ))
            .expect("native output callback accepts an owned value");
    }
    #[test]
    fn clap_parameter_prefix_keeps_clear_value_clear_order_and_new_suffix() {
        let source = Arc::new(ClapParameterSource::new());
        let controls = ClapParameterControlSource::new(&source);
        let mut output =
            ClapOutputEvents::new(&[descriptor()], Arc::clone(&source)).expect("owner");
        assert!(controls.record(Control::Clear { parameter: 17, flags: 2 }));
        value(&mut output, 0.75);
        assert!(controls.record(Control::Clear { parameter: 17, flags: 1 }));
        let prefix = PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &controls)
            .expect("coherent prefix")
            .expect("records");
        assert!(prefix.covers_current_source);
        assert_eq!(
            prefix.records.iter().map(|record| record.sequence()).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(matches!(
            prefix.records[0],
            ClapParameterRecord::Control(ClapParameterControl {
                kind: Control::Clear { flags: 2, .. },
                ..
            })
        ));
        assert!(matches!(prefix.records[1], ClapParameterRecord::Output(_)));
        assert!(matches!(
            prefix.records[2],
            ClapParameterRecord::Control(ClapParameterControl {
                kind: Control::Clear { flags: 1, .. },
                ..
            })
        ));
        value(&mut output, 0.125);
        assert!(prefix.acknowledge(&mut output, &controls).is_ok());
        assert_eq!(source.snapshot().revision, 4);
        assert_eq!(source.snapshot().pending_output, 1);
        assert_eq!(source.snapshot().pending_controls, 0);
        assert_eq!(output.events()[0].sequence, 4);
    }
    #[test]
    fn clap_parameter_prefix_rescan_is_an_ordered_query_boundary() {
        let source = Arc::new(ClapParameterSource::new());
        let controls = ClapParameterControlSource::new(&source);
        let mut output =
            ClapOutputEvents::new(&[descriptor()], Arc::clone(&source)).expect("owner");
        value(&mut output, 0.5);
        assert!(controls.record(Control::Rescan { flags: 1 }));
        assert!(controls.record(Control::Clear { parameter: 17, flags: 2 }));
        value(&mut output, 0.75);
        let prefix = PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &controls)
            .expect("query prefix")
            .expect("records");
        assert_eq!(prefix.records.len(), 2);
        assert!(!prefix.covers_current_source);
        assert!(prefix.acknowledge(&mut output, &controls).is_ok());
        let suffix = PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &controls)
            .expect("suffix")
            .expect("records");
        assert_eq!(
            suffix.records.iter().map(|record| record.sequence()).collect::<Vec<_>>(),
            [3, 4]
        );
        assert!(suffix.acknowledge(&mut output, &controls).is_ok());
        let status = source.snapshot();
        assert_eq!(status.revision, 4);
        assert_eq!((status.pending_output, status.pending_controls), (0, 0));
    }
    #[test]
    fn clap_parameter_prefix_wrong_owner_never_retires_either_lane() {
        let source = Arc::new(ClapParameterSource::new());
        let other = Arc::new(ClapParameterSource::new());
        let controls = ClapParameterControlSource::new(&source);
        let wrong = ClapParameterControlSource::new(&other);
        let mut output =
            ClapOutputEvents::new(&[descriptor()], Arc::clone(&source)).expect("owner");
        value(&mut output, 0.5);
        assert!(controls.record(Control::StateDirty));
        assert!(matches!(
            PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &wrong),
            Err(NativeAudioAuthorRefusal::OwnerChanged)
        ));
        let prefix = PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &controls)
            .expect("coherent")
            .expect("records");
        let prefix = match prefix.acknowledge(&mut output, &wrong) {
            Err(prefix) => prefix,
            Ok(()) => panic!("wrong source retired records"),
        };
        assert_eq!(
            (
                source.snapshot().pending_output,
                source.snapshot().pending_controls
            ),
            (1, 1)
        );
        assert_eq!(other.snapshot().revision, 0);
        assert!(prefix.acknowledge(&mut output, &controls).is_ok());
    }
    #[test]
    fn clap_parameter_prefix_publication_in_progress_is_not_empty_or_consumed() {
        let source = Arc::new(ClapParameterSource::new());
        let controls = ClapParameterControlSource::new(&source);
        let output = ClapOutputEvents::new(&[descriptor()], Arc::clone(&source)).expect("owner");
        let publication = source.begin_output().expect("actual publication");
        assert!(matches!(
            PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &controls),
            Err(NativeAudioAuthorRefusal::InvalidSource)
        ));
        assert_eq!(source.snapshot().revision, 0);
        drop(publication);
        assert!(
            PreparedClapParameterPrefix::prepare(Arc::clone(&source), &output, &controls)
                .expect("settled")
                .is_none()
        );
    }
}
