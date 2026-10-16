//! Explicit state-entry preparation on the sole retained VST child.

use super::*;
use crate::{
    NativeAudioAuthorStateEntryOutcome as Outcome, NativeAudioAuthorStateEntryReceipt as Receipt,
    NativeAudioAuthorStateEntryRefusal as Refusal, NativeAudioAuthorStateEntryRequest as Request,
    NativeAudioAuthorStateEntrySource as Source,
};

/// Constant-size private incarnation link. The real accepted state is owned
/// once by payload.state; the original delivered configuration remains owned
/// by the sole pending delivery until explicit reentered acknowledgement.
pub(super) struct PreparedAuthorEntry {
    pub(super) receipt: Receipt,
    first_signal_pending: bool,
    acknowledgement_pending: bool,
}

impl PreparedAuthorEntry {
    pub(super) fn awaiting_ack(&self) -> bool {
        self.acknowledgement_pending
    }
}

impl Vst3Worker {
    fn author_entry_source_closed(
        &self,
        request: &Request<'_>,
    ) -> Result<bool, AudioProcessorHostError> {
        let Some(plugin) = self.plugin.as_ref() else {
            return Ok(false);
        };
        let source =
            native_author::status(plugin.editor_event_source_status().map_err(|error| {
                unavailable(format!(
                    "VST3 state-entry source observation failed: {error}"
                ))
            })?)?;
        if plugin.pending_processor_parameter_input().map_err(|error| {
            unavailable(format!(
                "VST3 state-entry input observation failed: {error}"
            ))
        })? || !source.closed_empty()
            || self.pending_reprepare().is_some()
        {
            return Ok(false);
        }
        Ok(match request.source {
            Source::SelectedTerminal { revision } => {
                source.revision() == revision.get()
                    && matches!(&self.native_author_delivery,
                        Some(native_author::PendingDelivery::Delivered {
                            binding, revision: retained_revision,
                            configuration: Some(configuration),
                        }) if *binding == request.binding
                            && *retained_revision == revision.get()
                            && configuration.bytes() == request.configuration)
            }
            Source::CanonicalPeer { revision } => {
                source.revision() == revision && self.native_author_delivery.is_none()
            }
        })
    }

    pub(super) fn prepare_author_state_entry(
        &mut self,
        request: Request<'_>,
    ) -> Result<Outcome, AudioProcessorHostError> {
        if request.format != super::super::NativeConfigurationFormat::Vst3ComponentAndController
            || request.configuration.is_empty()
            || request.configuration.len() > MAX_STATE_BYTES
        {
            return Ok(Outcome::Refused(Refusal::ConfigurationRejected));
        }
        let digest: [u8; 32] = Sha256::digest(request.configuration).into();
        if let Some(prepared) = &self.prepared_author_entry {
            let receipt = prepared.receipt;
            if prepared.first_signal_pending
                && receipt.source_binding == request.binding
                && receipt.source == request.source
                && receipt.configuration_sha256 == digest
                && receipt.configuration_bytes == request.configuration.len()
                && self.native_audio_object_observation()? == Some(receipt.object)
                && self.payload.state.as_deref() == Some(request.configuration)
            {
                let plugin =
                    self.plugin.as_ref().ok_or_else(|| unavailable("prepared object closed"))?;
                let source = native_author::status(plugin.editor_event_source_status().map_err(
                    |error| unavailable(format!("prepared native source check failed: {error}")),
                )?)?;
                if source.closed_empty() && self.pending_reprepare().is_none() {
                    return Ok(Outcome::Prepared(receipt));
                }
                return Ok(Outcome::Refused(Refusal::SourcePending));
            }
            // Never repeat a destructive restore on a different retained prefix.
            return Ok(Outcome::Refused(Refusal::DeliveryChanged));
        }
        if !self.author_binding_current(request.binding)? {
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        if !self.author_entry_source_closed(&request)? {
            return Ok(Outcome::Refused(Refusal::SourcePending));
        }
        // One real owner allocation is admitted before closing/retiring native
        // state. The decoder observes its actual capacity independently too.
        let mut accepted = Vec::new();
        accepted
            .try_reserve_exact(request.configuration.len())
            .map_err(|_| unavailable("VST3 accepted author-state allocation failed"))?;
        accepted.extend_from_slice(request.configuration);
        if accepted.capacity() > MAX_STATE_BYTES {
            return Ok(Outcome::Refused(Refusal::ConfigurationRejected));
        }
        // The admitted lane already reserves one additional seed owner beside
        // the actual decoded request and the existing payload. Observe the real
        // new Vec capacity; do not turn byte length into an allocation receipt.
        // An existing staged candidate is included in the same owner ceiling.
        if let Some(profile) = self.native_lane {
            let staged_bytes = self
                .native_staged_seed_layout()
                .bytes
                .checked_add(accepted.capacity())
                .ok_or_else(|| invalid("native staged author-state owner extent overflow"))?;
            profile
                .admit_observed_lane_owners(
                    profile.owning_layouts(),
                    crate::NativeWorkerControlOverlapLayout {
                        decoded_input: crate::NativeHostBufferLayout { bytes: 0, alignment: 1 },
                        reply_storage: crate::NativeHostBufferLayout { bytes: 0, alignment: 1 },
                        staged_seed: crate::NativeHostBufferLayout {
                            bytes: staged_bytes,
                            alignment: 1,
                        },
                    },
                )
                .map_err(AudioProcessorHostError::from)?;
        } else {
            // This first state-entry adapter is qualified only with the actual
            // Windows scoped-lane resource profile, never an ungranted lane.
            return Ok(Outcome::Refused(Refusal::UnsupportedAdapter));
        }
        // The canonical close call owns removal/frame-clear retry. A failed
        // removal is fatal; no second parent/view is constructed over it.
        match self.close_native_audio_editor(request.binding)? {
            crate::NativeAudioEditorOutcome::Closed { .. } => {}
            crate::NativeAudioEditorOutcome::ClosePending { .. } => {
                return Ok(Outcome::Refused(Refusal::EditorCleanupPending));
            }
            crate::NativeAudioEditorOutcome::Refused(_) => {
                return Ok(Outcome::Refused(Refusal::OwnerChanged));
            }
            _ => return Err(invalid("state-entry close returned a non-close result")),
        }
        // Removal callbacks are real author source data. They cannot be erased
        // by retirement merely because the earlier snapshot was closed.
        if !self.author_entry_source_closed(&request)? {
            return Ok(Outcome::Refused(Refusal::SourcePending));
        }
        self.native_binding = None;
        self.last_end = None;
        let retired_seed = self.payload.state.replace(accepted);
        // This is the SAME ordinary real state-entry loader, after DSP join.
        // Old component/controller/module are retired before fresh construction.
        self.rebuild_native_state()?;
        drop(retired_seed);
        let object = self
            .native_audio_object_observation()?
            .ok_or_else(|| unavailable("real prepared native object has no incarnation"))?;
        if object.process_id() != request.binding.expected_object.process_id()
            || object.object_serial() <= request.binding.expected_object.object_serial()
        {
            return Err(invalid(
                "real state-entry incarnation did not change on same child",
            ));
        }
        let receipt = Receipt {
            source_binding: request.binding,
            source: request.source,
            object,
            configuration_sha256: digest,
            configuration_bytes: request.configuration.len(),
            contract: self.contract,
        };
        self.prepared_author_entry = Some(PreparedAuthorEntry {
            receipt,
            first_signal_pending: true,
            acknowledgement_pending: matches!(request.source, Source::SelectedTerminal { .. }),
        });
        let plugin = self
            .plugin
            .as_ref()
            .ok_or_else(|| unavailable("prepared native object closed"))?;
        let source =
            native_author::status(plugin.editor_event_source_status().map_err(|error| {
                unavailable(format!("fresh native source check failed: {error}"))
            })?)?;
        if !source.closed_empty() || self.pending_reprepare().is_some() {
            // Keep the private prepared object/prefix. Never label initialization
            // callbacks closed and never rebuild it to clear those callbacks.
            return Ok(Outcome::Refused(Refusal::SourcePending));
        }
        Ok(Outcome::Prepared(receipt))
    }

    pub(super) fn enter_prepared_author_state(
        &mut self,
        start_sample: i64,
    ) -> Result<bool, AudioProcessorHostError> {
        let Some(prepared) = &self.prepared_author_entry else {
            return Ok(false);
        };
        if !prepared.first_signal_pending {
            return Ok(false);
        }
        if prepared.acknowledgement_pending
            || self.native_audio_object_observation()? != Some(prepared.receipt.object)
            || self.pending_reprepare().is_some()
        {
            return Err(unavailable(
                "prepared native entry has unaccepted author/native work",
            ));
        }
        let plugin = self.plugin.as_ref().ok_or_else(|| unavailable("prepared object closed"))?;
        let source =
            native_author::status(plugin.editor_event_source_status().map_err(|error| {
                unavailable(format!("prepared first-entry source check failed: {error}"))
            })?)?;
        if !source.closed_empty() {
            return Err(unavailable("prepared first-entry source is not closed"));
        }
        // The original fresh object has processed no samples. The sole Session
        // supplies the actual signal coordinate now, including lazy Scope entry.
        self.prepared_author_entry
            .as_mut()
            .ok_or_else(|| unavailable("prepared entry lost"))?
            .first_signal_pending = false;
        self.last_end = Some(start_sample);
        Ok(true)
    }

    pub(super) fn acknowledge_reentered_author(
        &mut self,
        ack: crate::NativeAudioReenteredAuthorAcknowledgement,
        source: crate::NativeAudioAuthorSourceStatus,
    ) -> Result<crate::NativeAudioAuthorObservation, AudioProcessorHostError> {
        use crate::NativeAudioAuthorDeliveryState as Delivery;
        let refusal = |source| crate::NativeAudioAuthorObservation {
            source,
            delivery: Delivery::Refused(crate::NativeAudioAuthorRefusal::StaleDelivery),
        };
        let Some(prepared) = &self.prepared_author_entry else {
            return Ok(refusal(source));
        };
        let Source::SelectedTerminal { revision } = ack.preparation.source else {
            return Ok(refusal(source));
        };
        if prepared.receipt != ack.preparation
            || !prepared.acknowledgement_pending
            || !prepared.first_signal_pending
            || ack.installed_binding.expected_object != prepared.receipt.object
            || !self.author_binding_current(ack.installed_binding)?
            || !source.closed_empty()
            || !matches!(&self.native_author_delivery,
                Some(native_author::PendingDelivery::Delivered { binding, revision: retained, .. })
                if *binding == ack.preparation.source_binding && *retained == revision.get())
        {
            return Ok(refusal(source));
        }
        // Actual bytes were already restored once. Ack retires only the old
        // retained delivery; it does not restore again or overwrite fresh data.
        self.native_author_delivery = None;
        self.prepared_author_entry
            .as_mut()
            .ok_or_else(|| unavailable("prepared Ack lost"))?
            .acknowledgement_pending = false;
        Ok(crate::NativeAudioAuthorObservation {
            source,
            delivery: Delivery::ReenteredAcknowledged(ack),
        })
    }
}
