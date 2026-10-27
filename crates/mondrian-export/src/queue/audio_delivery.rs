//! Sole-consumer ownership and admission for offline Program Outputs.
//!
//! DSP, PDC, state capture and installation remain in the shared Audio Runtime.
//! This consumer supplies the actual Source and a quiescent block boundary:
//! no device or encoder can read the temporary PCM while it is being prepared.

use mondrian_audio::{
    AudioContinuityEpoch, AudioDeliveryError, AudioExecutionError, AudioProcessorReprepareTicket,
    AudioProgramDeliveryRuntime, AudioRenderRequest, AudioRuntimeResourceFootprint,
    AudioRuntimeResourceGrant,
};
use mondrian_core::{
    AudioOwnerInstallationClaim, AudioOwnerInstallationRegistration, AudioOwnerInstallationSource,
    ExecutionCancellationToken,
};

/// Give each simultaneously-live stem a disjoint hard grant before native work.
/// Cold requirements are retained first; equal shares of each remaining resource
/// go to the outputs in authored order. Each Runtime admits its own retained
/// reprepare overlap, so their peaks cannot multiply the original job grant.
pub(super) fn partition_stem_grants(
    cold: &[AudioRuntimeResourceFootprint],
    job: AudioRuntimeResourceGrant,
) -> Result<Vec<AudioRuntimeResourceGrant>, String> {
    if cold.is_empty() {
        return Err("audio-stem grant partition requires at least one output".to_owned());
    }
    let total = cold
        .iter()
        .try_fold(AudioRuntimeResourceFootprint::default(), |total, next| {
            super::add_audio_runtime_footprint(total, *next)
        })
        .ok_or_else(|| {
            "audio-stem aggregate Runtime footprint exceeds addressable capacity".to_owned()
        })?;
    super::validate_audio_runtime_footprint_grant(total, job)?;
    let count = cold.len();
    let share = |base: usize, used: usize, granted: usize, index: usize| {
        let slack = granted - used;
        // base <= used <= granted; this share is at most granted-used.
        base + slack / count + usize::from(index < slack % count)
    };
    Ok(cold
        .iter()
        .enumerate()
        .map(|(index, base)| {
            AudioRuntimeResourceGrant::new(
                share(
                    base.runtime_occurrences,
                    total.runtime_occurrences,
                    job.max_runtime_occurrences,
                    index,
                ),
                share(
                    base.fixed_resident_bytes,
                    total.fixed_resident_bytes,
                    job.max_fixed_resident_bytes,
                    index,
                ),
                share(
                    base.prepared_logical_bytes,
                    total.prepared_logical_bytes,
                    job.max_prepared_logical_bytes,
                    index,
                ),
            )
        })
        .collect())
}

pub(super) struct ExportAudioDelivery {
    // Drop the Runtime/native children before closing their actual Source.
    delivery: AudioProgramDeliveryRuntime,
    native: Option<NativeInstallation>,
}

struct NativeInstallation {
    source: AudioOwnerInstallationSource,
    registration: AudioOwnerInstallationRegistration,
    claim: AudioOwnerInstallationClaim,
}

impl ExportAudioDelivery {
    pub(super) fn new(
        delivery: AudioProgramDeliveryRuntime,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Self, String> {
        if cancellation.is_canceled() {
            return Err("export audio preparation cancelled".to_owned());
        }
        let mut owned = Self { delivery, native: None };
        if owned.delivery.native_observations_supported() {
            let source = AudioOwnerInstallationSource::new();
            let (registration, verifier) = source.register().map_err(|e| e.to_string())?;
            owned
                .delivery
                .bind_native_owner_installation(verifier)
                .map_err(|e| e.to_string())?;
            let claim = source
                .reserve_install(&registration, None)
                .and_then(|reservation| source.begin_commit(reservation))
                .map_err(|e| e.to_string())?
                .commit()
                .ok_or_else(|| "native export initial Source commit produced no claim".to_owned())?;
            // Adopt before any foreign call or fallible native acknowledgement.
            owned.native = Some(NativeInstallation { source, registration, claim });
            owned
                .delivery
                .install_native_owner_bindings(&owned.native.as_ref().unwrap().claim, cancellation)
                .map_err(|e| format!("native export owner binding failed: {e}"))?;
        }
        Ok(owned)
    }

    pub(super) fn enter_state(
        &mut self,
        start_sample: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if !self.delivery.requires_state_entry() {
            return Ok(());
        }
        let epoch = self.native.as_ref().map_or(1, |native| native.claim.generation().get());
        match self.delivery.enter_state_cancellable(
            AudioContinuityEpoch::new(epoch),
            start_sample,
            cancellation,
        ) {
            Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::ReprepareRequired(
                ticket,
            ))) => self.reprepare(ticket, start_sample, cancellation),
            result => result.map_err(|e| e.to_string()),
        }
    }

    pub(super) fn render_into_cancellable(
        &mut self,
        request: AudioRenderRequest,
        pcm: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        match self.delivery.render_into_cancellable(request, pcm, cancellation) {
            Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::ReprepareRequired(
                ticket,
            ))) => {
                self.reprepare(ticket, request.start_sample, cancellation)?;
                // One explicit Fresh entry, then one execution attempt. A new
                // hold during entry or retry is returned to the job, never looped.
                self.delivery
                    .render_into_cancellable(request, pcm, cancellation)
                    .map_err(|e| format!("native export render after Fresh entry failed: {e}"))
            }
            result => result.map_err(|e| e.to_string()),
        }
    }

    fn reprepare(
        &mut self,
        ticket: AudioProcessorReprepareTicket,
        start_sample: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        let native = self
            .native
            .as_mut()
            .ok_or_else(|| "native export restart has no installed Source".to_owned())?;
        let destination = self
            .delivery
            .native_owner_installation_verifier()
            .ok_or_else(|| "native export restart lost its Runtime registration".to_owned())?;
        let operation = self
            .delivery
            .service_pending_native_reprepare(ticket.product_notice(), cancellation)
            .map_err(|e| format!("native export reprepare capture failed: {e}"))?;
        self.delivery
            .prepare_native_reprepare_plan(&operation, destination, cancellation)
            .map_err(|e| format!("native export reprepare plan failed: {e}"))?;
        let reservation = native
            .source
            .reserve_install(&native.registration, Some(&native.claim))
            .map_err(|e| e.to_string())?;
        let mut authorization = native
            .source
            .authorize_runtime_installation(reservation)
            .map_err(|e| e.to_string())?;
        let receipt = self
            .delivery
            .install_native_reprepare_plan(&operation, &mut authorization, cancellation)
            .map_err(|e| format!("native export reprepare installation failed: {e}"))?;
        native.claim = receipt.claim().clone();
        // No Runtime/Source lock is held while the old owners are retired.
        self.delivery
            .finish_native_reprepare_retirement(receipt.retire())
            .map_err(|e| format!("native export reprepare retirement failed: {e}"))?;
        self.delivery
            .enter_state_cancellable(
                AudioContinuityEpoch::new(native.claim.generation().get()),
                start_sample,
                cancellation,
            )
            .map_err(|e| format!("native export Fresh entry failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cold(occurrences: usize, fixed: usize, logical: usize) -> AudioRuntimeResourceFootprint {
        AudioRuntimeResourceFootprint {
            runtime_occurrences: occurrences,
            fixed_resident_bytes: fixed,
            prepared_logical_bytes: logical,
            ..Default::default()
        }
    }

    #[test]
    fn stem_grants_reserve_cold_requirements_and_conserve_all_three_job_limits() {
        let inputs = [cold(1, 7, 9), cold(3, 2, 4)];
        let job = AudioRuntimeResourceGrant::new(7, 22, 21);
        let quotas = partition_stem_grants(&inputs, job).unwrap();
        assert_eq!(
            quotas,
            [
                AudioRuntimeResourceGrant::new(3, 14, 13),
                AudioRuntimeResourceGrant::new(4, 8, 8)
            ]
        );
        assert_eq!(
            quotas.iter().map(|q| q.max_runtime_occurrences).sum::<usize>(),
            7
        );
        assert_eq!(
            quotas.iter().map(|q| q.max_fixed_resident_bytes).sum::<usize>(),
            22
        );
        assert_eq!(
            quotas.iter().map(|q| q.max_prepared_logical_bytes).sum::<usize>(),
            21
        );
        for (required, quota) in inputs.iter().zip(quotas) {
            assert!(required.runtime_occurrences <= quota.max_runtime_occurrences);
            assert!(required.fixed_resident_bytes <= quota.max_fixed_resident_bytes);
            assert!(required.prepared_logical_bytes <= quota.max_prepared_logical_bytes);
        }
    }

    #[test]
    fn stem_grants_accept_exact_cold_fit_and_refuse_each_insufficient_dimension() {
        let inputs = [cold(1, 7, 9), cold(3, 2, 4)];
        assert_eq!(
            partition_stem_grants(&inputs, AudioRuntimeResourceGrant::new(4, 9, 13)).unwrap(),
            [
                AudioRuntimeResourceGrant::new(1, 7, 9),
                AudioRuntimeResourceGrant::new(3, 2, 4)
            ]
        );
        for job in [
            AudioRuntimeResourceGrant::new(3, 9, 13),
            AudioRuntimeResourceGrant::new(4, 8, 13),
            AudioRuntimeResourceGrant::new(4, 9, 12),
        ] {
            assert!(partition_stem_grants(&inputs, job).is_err());
        }
    }

    #[test]
    fn stem_grants_refuse_empty_and_overflow_but_accept_one_maximum_grant() {
        let job = AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX);
        assert!(partition_stem_grants(&[], job).is_err());
        assert!(partition_stem_grants(&[cold(1, usize::MAX, 0), cold(1, 1, 0)], job).is_err());
        assert_eq!(
            partition_stem_grants(&[cold(usize::MAX, usize::MAX, usize::MAX)], job).unwrap(),
            [job]
        );
    }
}
