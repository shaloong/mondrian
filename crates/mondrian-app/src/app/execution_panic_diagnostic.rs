//! Owner-free unwind diagnostics shared by execution Modules and their adapters.
//! Never run an opaque payload destructor on a construction or shutdown stack.

#[derive(Debug, thiserror::Error)]
#[error("{operation} panicked: {detail}; opaque_payload_abandoned={opaque_payload_abandoned}")]
struct ExecutionPanic {
    operation: &'static str,
    detail: String,
    opaque_payload_abandoned: bool,
}

/// Preserve a caught startup panic as a diagnostic, separately from live owners.
pub(crate) fn startup_panic_diagnostic(payload: Box<dyn std::any::Any + Send>) -> anyhow::Error {
    execution_panic_diagnostic(payload, "Headless startup")
}

/// Convert an unwind payload without executing an opaque user destructor.
pub(crate) fn execution_panic_diagnostic(
    payload: Box<dyn std::any::Any + Send>,
    operation: &'static str,
) -> anyhow::Error {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|value| (*value).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "opaque startup panic payload".to_owned());
    // Opaque payload destructors can themselves panic or carry foreign owners.
    // Match the product's explicit abandonment policy instead of unwinding here.
    let opaque_payload_abandoned = !(payload.is::<&'static str>() || payload.is::<String>());
    if opaque_payload_abandoned {
        std::mem::forget(payload);
    } else {
        drop(payload);
    }
    ExecutionPanic { operation, detail, opaque_payload_abandoned }.into()
}

/// Whether diagnostic conversion deliberately retained an opaque unwind payload.
pub(crate) fn opaque_panic_payload_abandoned(diagnostic: &anyhow::Error) -> bool {
    diagnostic
        .downcast_ref::<ExecutionPanic>()
        .is_some_and(|panic| panic.opaque_payload_abandoned)
}
