//! Native adapter -> foundation diagnostic projection, never owner admission.

use mondrian_core::{
    NativeAudioDiagnosticBatch, NativeAudioDiagnosticInsertion as Insertion,
    NativeAudioDiagnosticLocation as Location, NativeAudioDiagnosticLoss,
    NativeAudioDiagnosticMessageFacts, NativeAudioDiagnosticRecord,
    NativeAudioDiagnosticSeverity as Severity, NativeAudioDiagnosticSource,
};

pub(crate) fn project(
    root: crate::AudioSessionCursorIdentity,
    local: crate::AudioSessionCursorIdentity,
    occurrence: crate::AudioProcessorOccurrence,
    observations: crate::NativeAudioObservations,
) -> Result<Option<NativeAudioDiagnosticBatch>, crate::AudioProcessorHostError> {
    if observations.logs.records().is_empty() && !observations.logs.incomplete() {
        return Ok(None);
    }
    let process_id = observations.owner_process_id().ok_or_else(|| {
        crate::AudioProcessorHostError::WorkerFailed {
            operation: "native diagnostic observation",
            detail: "native diagnostic source has no authenticated producer PID".to_owned(),
        }
    })?;
    let location = match occurrence.owner {
        crate::AudioProcessorOccurrenceOwner::Contribution { edit_id, scope_id } => {
            Location::Contribution { edit_id, scope_id }
        }
        crate::AudioProcessorOccurrenceOwner::Track(id) => Location::Track(id),
        crate::AudioProcessorOccurrenceOwner::Bus(id) => Location::Bus(id),
        crate::AudioProcessorOccurrenceOwner::Output(id) => Location::Output(id),
    };
    let insertion = match occurrence.insertion {
        crate::AudioProcessorInsertionPoint::Scope => Insertion::Scope,
        crate::AudioProcessorInsertionPoint::PreFader => Insertion::PreFader,
        crate::AudioProcessorInsertionPoint::PostFader => Insertion::PostFader,
    };
    let mut batch = NativeAudioDiagnosticBatch::new(
        NativeAudioDiagnosticSource {
            root_plan: root.prepared_plan_id,
            root_session: root.session_id,
            local_plan: local.prepared_plan_id,
            local_session: local.session_id,
            instance: occurrence.instance_id,
            location,
            insertion,
            process_id,
        },
        NativeAudioDiagnosticLoss {
            capacity: observations.logs.capacity_loss(),
            contention: observations.logs.contention_loss(),
            sequence_exhausted: observations.logs.sequence_exhausted(),
        },
    );
    for native in observations.logs.records() {
        let severity = match native.severity() {
            Some(crate::NativeAudioLogSeverity::Debug) => Severity::Debug,
            Some(crate::NativeAudioLogSeverity::Info) => Severity::Info,
            Some(crate::NativeAudioLogSeverity::Warning) => Severity::Warning,
            Some(crate::NativeAudioLogSeverity::Error) => Severity::Error,
            Some(crate::NativeAudioLogSeverity::Fatal) => Severity::Fatal,
            Some(crate::NativeAudioLogSeverity::HostMisbehaving) => Severity::HostMisbehaving,
            Some(crate::NativeAudioLogSeverity::PluginMisbehaving) => Severity::PluginMisbehaving,
            None => Severity::Unknown,
        };
        let record = NativeAudioDiagnosticRecord::project(
            native.sequence(),
            native.raw_severity(),
            severity,
            NativeAudioDiagnosticMessageFacts {
                truncated: native.truncated(),
                null_message: native.null_message(),
                invalid_utf8: native.invalid_utf8(),
                incomplete_utf8_suffix: native.incomplete_utf8_suffix(),
            },
            native.bytes(),
        )
        .map_err(|error| crate::AudioProcessorHostError::WorkerFailed {
            operation: "native diagnostic projection",
            detail: error.to_string(),
        })?;
        batch
            .push(record)
            .map_err(|error| crate::AudioProcessorHostError::WorkerFailed {
                operation: "native diagnostic projection",
                detail: error.to_string(),
            })?;
    }
    Ok(Some(batch))
}

const _: () = assert!(
    crate::NATIVE_AUDIO_LOG_MESSAGE_BYTES == mondrian_core::NATIVE_AUDIO_DIAGNOSTIC_MESSAGE_BYTES
);
const _: () =
    assert!(crate::NATIVE_AUDIO_LOG_RECORDS == mondrian_core::NATIVE_AUDIO_DIAGNOSTIC_RECORDS);
