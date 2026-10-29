//! Shared lifecycle of live value edits.

/// Explicit boundaries of a user edit; keyboard changes are independent edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueEdit {
    /// One keyboard or other independent value change.
    Single,
    /// First pointer value, including a pointer-down that leaves the value unchanged.
    Begin(mondrian_core::AuthoringGestureId),
    /// A changed value in the same pointer interaction.
    Update(mondrian_core::AuthoringGestureId),
    /// Accept the latest values in the pointer interaction.
    Finish(mondrian_core::AuthoringGestureId),
    /// Escape cancels the interaction; author adapters restore through their transaction owner.
    Cancel(mondrian_core::AuthoringGestureId),
}
