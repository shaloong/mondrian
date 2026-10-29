//! Test-only snapshots of the existing Runtime subtree footprint caches.

use super::*;

/// Read the actual cache and the layout of its independent inline capture
/// field at each exclusive Runtime materialization. Copied test facts are not
/// resource/author/native authority and create no Runtime owner.
pub(crate) fn subtree_footprints(
    runtime: &AudioProgramRuntime,
) -> Vec<(
    crate::AudioSessionCursorIdentity,
    AudioRuntimeResourceFootprint,
    usize,
)> {
    fn visit(
        runtime: &AudioProgramRuntime,
        snapshots: &mut Vec<(
            crate::AudioSessionCursorIdentity,
            AudioRuntimeResourceFootprint,
            usize,
        )>,
    ) {
        snapshots.push((
            runtime.session.native_owner_identity(),
            runtime.resource_footprint,
            std::mem::size_of_val(&runtime.native_reprepare),
        ));
        for source in runtime.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                visit(&nested.runtime, snapshots);
            }
        }
    }
    let mut snapshots = Vec::new();
    visit(runtime, &mut snapshots);
    snapshots
}
