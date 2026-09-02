use std::any::Any;
use std::io;

/// Dispose the two canonical Rust panic payloads and deliberately abandon any
/// opaque payload whose destructor is outside Mondrian's latency contract.
///
/// Returns `true` when an opaque owner was abandoned. Callers must retain that
/// fact in terminal evidence; forgetting the payload is never a clean result.
pub(crate) fn dispose_canonical_or_abandon_opaque_panic_payload(
    payload: Box<dyn Any + Send>,
) -> bool {
    if payload.is::<&'static str>() || payload.is::<String>() {
        drop(payload);
        false
    } else {
        // `panic_any` may carry a foreign value with a blocking or panicking
        // destructor. Never execute that destructor on a realtime, UI, or
        // qualification-coordinator thread.
        std::mem::forget(payload);
        true
    }
}

/// Retain the stable operating-system error kind while abandoning a possibly
/// foreign `io::Error::other` payload.
///
/// Plain OS/simple-kind errors are dropped normally, so repeated ordinary
/// spawn failures do not leak. Only errors carrying a type-erased source are
/// deliberately retained because its destructor is outside Mondrian's
/// latency contract. The returned kind is safe to format or reconstruct.
pub(crate) fn abandon_io_error(error: io::Error) -> (io::ErrorKind, bool) {
    let kind = error.kind();
    let owner_abandoned = error.get_ref().is_some();
    if owner_abandoned {
        std::mem::forget(error);
    } else {
        drop(error);
    }
    (kind, owner_abandoned)
}
