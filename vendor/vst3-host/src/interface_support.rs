//! Immutable declarations of plug-in interfaces consumed by one host context.

/// Plug-in interface known by this adapter, rather than a host callback IID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PluginInterface {
    /// Core component lifecycle and state.
    Component,
    /// Core audio processing interface.
    AudioProcessor,
    /// Parameter controller lifecycle and values.
    EditController,
    /// Component/controller connection messages.
    ConnectionPoint,
    /// MIDI controller-to-parameter mapping.
    MidiMapping,
    /// Unit and program hierarchy.
    UnitInfo,
    /// Program state streams.
    ProgramListData,
    /// Unit state streams.
    UnitData,
    /// Host-origin editing brackets.
    EditControllerHostEditing,
    /// Live MIDI learn notifications.
    MidiLearn,
    /// Native automation recording/read state.
    AutomationState,
    /// Note expression metadata and events.
    NoteExpressionController,
    /// Native editor content scale.
    PlugViewContentScaleSupport,
    /// Native plug-in editor attachment and sizing.
    PlugView,
    /// Requested processing context validity.
    ProcessContextRequirements,
    /// Prefetch processing policy.
    PrefetchableSupport,
    /// Parameter remapping across class migration.
    RemapParamId,
    /// Processor-to-controller data exchange delivery.
    DataExchangeReceiver,
}

/// Instance-local declarations supplied before factory/component initialization.
///
/// A library API's existence does not establish that an embedding application
/// consumes it. Embedders can select the interfaces their complete execution
/// path actually supports. The value owns no heap storage and cannot change an
/// already initialized native host context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginInterfaceSupport {
    bits: u32,
}

impl PluginInterfaceSupport {
    /// Declare no plug-in interfaces consumed.
    pub const fn none() -> Self {
        Self { bits: 0 }
    }

    /// Add one consumed interface to this immutable value.
    pub const fn with(self, interface: PluginInterface) -> Self {
        Self {
            bits: self.bits | (1_u32 << interface as u8),
        }
    }

    /// Whether this value declares the given plug-in interface consumed.
    pub const fn supports(self, interface: PluginInterface) -> bool {
        self.bits & (1_u32 << interface as u8) != 0
    }
}

impl Default for PluginInterfaceSupport {
    fn default() -> Self {
        // Preserve the standalone library's original declarations exactly.
        use PluginInterface::*;
        Self::none()
            .with(ConnectionPoint)
            .with(MidiMapping)
            .with(UnitInfo)
            .with(ProgramListData)
            .with(UnitData)
            .with(EditControllerHostEditing)
            .with(MidiLearn)
            .with(AutomationState)
            .with(NoteExpressionController)
            .with(PlugViewContentScaleSupport)
            .with(ProcessContextRequirements)
            .with(PrefetchableSupport)
            .with(RemapParamId)
            .with(DataExchangeReceiver)
    }
}
