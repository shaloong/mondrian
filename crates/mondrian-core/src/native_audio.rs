//! Typed native audio lifecycle notices shared across execution adapters.

use crate::AudioProcessorInstanceId;

/// Coalesced standard lifecycle request, never an execution fault or author edit.
///
/// Raw flags retain their normative meaning. They are not interpreted by media
/// playback and are not an arbitrary control RPC or persisted project state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioReprepareRequest {
    /// Actual VST3 IComponentHandler::restartComponent flags.
    Vst3Restart {
        /// Exact nonzero coalesced standard bitmask, including unknown bits.
        flags: u32,
    },
    /// CLAP core host request_restart; extension flags are distinct contracts.
    ClapRestart,
}

/// Diagnostic identity of a native request retained by the canonical audio owner.
///
/// This identifies an author instance for product evidence. The audio owner
/// retains the exact generated nested occurrence; this notice is not authority
/// to mutate a selected instance or to choose another playback materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioReprepareNotice {
    /// Stable accepted author instance requiring nonRT owner service.
    pub instance_id: AudioProcessorInstanceId,
    /// Actual coalesced native request.
    pub request: NativeAudioReprepareRequest,
}

impl std::fmt::Display for NativeAudioReprepareNotice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "native audio instance {} requires reprepare ({:?})",
            self.instance_id, self.request
        )
    }
}
