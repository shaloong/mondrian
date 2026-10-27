//! Sole-consumer ownership for primary offline audio delivery.
//!
//! DSP, PDC, state capture and installation remain in the shared Audio Runtime.
//! This consumer supplies the actual Source and a quiescent block boundary:
//! no device or encoder can read the temporary PCM while it is being prepared.

use mondrian_audio::{
    AudioContinuityEpoch, AudioDeliveryError, AudioExecutionError, AudioProcessorReprepareTicket,
    AudioProgramDeliveryRuntime, AudioRenderRequest,
};
use mondrian_core::{
    AudioOwnerInstallationClaim, AudioOwnerInstallationRegistration, AudioOwnerInstallationSource,
    ExecutionCancellationToken,
};

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
