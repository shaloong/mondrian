//! App Adapter for the playback-owned Preview Frame Store.
//!
//! This Module maps Mondrian media and Viewer payloads to opaque playback keys,
//! exact byte reservations, and an exact presentation scope. Residency,
//! admission, eviction, failure memory, and pinning remain playback-owned.

use mondrian_core::types::SequenceId;
use mondrian_ui_widgets::ViewerFrameImage;

use super::preview::{MediaPreviewFrame, ScopedViewerFrame, ViewerPreviewCacheKey};
use super::preview_access_mode::MediaPreviewKey;

#[cfg(test)]
pub(crate) type PreviewCpuFrameStoreConfig = mondrian_playback::PreviewFrameStoreConfig;
pub(crate) type PreviewCpuFrameStoreDiagnostics = mondrian_playback::PreviewFrameStoreDiagnostics;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerFrameScope {
    sequence_id: SequenceId,
    width: u32,
    height: u32,
}

type PlaybackFrameStore = mondrian_playback::PreviewFrameStore<
    MediaPreviewKey,
    MediaPreviewFrame,
    ViewerPreviewCacheKey,
    ViewerFrameImage,
    ViewerFrameScope,
>;

/// Thin App Adapter over the playback-owned Preview Frame Store Interface.
#[derive(Default)]
pub(crate) struct PreviewCpuFrameStore {
    store: PlaybackFrameStore,
}

impl PreviewCpuFrameStore {
    /// Create a test Adapter with explicit budgets.
    #[cfg(test)]
    pub(crate) fn new(config: PreviewCpuFrameStoreConfig) -> Self {
        Self { store: PlaybackFrameStore::new(config) }
    }

    /// Return and touch one decoded-media frame.
    pub(crate) fn media_frame(&mut self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        self.store.media_frame(key)
    }

    /// Admit a decoded-media frame and optionally pin an oversize current frame.
    pub(crate) fn insert_media_frame(
        &mut self,
        key: MediaPreviewKey,
        frame: MediaPreviewFrame,
        pin_if_oversize: bool,
    ) -> bool {
        let reserved_bytes = frame.reserved_cpu_bytes();
        self.store
            .admit_media_frame(key, frame, reserved_bytes, pin_if_oversize)
            .is_resident()
    }

    /// Return and touch one final Viewer raster.
    pub(crate) fn viewer_frame(&mut self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        self.store.viewer_frame(key)
    }

    /// Admit one final Viewer raster under its encoded byte reservation.
    pub(crate) fn insert_viewer_frame(
        &mut self,
        key: ViewerPreviewCacheKey,
        frame: ViewerFrameImage,
    ) -> bool {
        let reserved_bytes = frame.rgba.len();
        self.store.admit_viewer_frame(key, frame, reserved_bytes).is_resident()
    }

    /// Remember one terminal media failure.
    pub(crate) fn remember_failure(&mut self, key: MediaPreviewKey) {
        self.store.remember_failure(key);
    }

    /// Remove failure memory after a successful completion.
    pub(crate) fn forget_failure(&mut self, key: &MediaPreviewKey) {
        self.store.forget_failure(key);
    }

    /// Return whether the key has a remembered terminal failure and touch it.
    pub(crate) fn contains_failure(&mut self, key: &MediaPreviewKey) -> bool {
        self.store.contains_failure(key)
    }

    /// Pin the current Viewer raster with its exact stale-reuse scope.
    pub(crate) fn pin_viewer_frame(&mut self, frame: ScopedViewerFrame) {
        let scope = ViewerFrameScope {
            sequence_id: frame.sequence_id,
            width: frame.width,
            height: frame.height,
        };
        let reserved_bytes = frame.frame.rgba.len();
        self.store.pin_viewer_frame(scope, frame.frame, reserved_bytes);
    }

    /// Return the pinned raster only when its presentation scope matches exactly.
    pub(crate) fn stale_viewer_frame(
        &self,
        sequence_id: SequenceId,
        width: u32,
        height: u32,
    ) -> Option<ViewerFrameImage> {
        self.store.stale_viewer_frame(&ViewerFrameScope { sequence_id, width, height })
    }

    /// Release the non-evictable current/stale Viewer pin.
    pub(crate) fn clear_pinned_viewer_frame(&mut self) {
        self.store.clear_pinned_viewer_frame();
    }

    /// Release the oversize current-media pin.
    pub(crate) fn clear_pinned_media_frame(&mut self) {
        self.store.clear_pinned_media_frame();
    }

    /// Clear final Viewer residency and its current/stale pin.
    pub(crate) fn clear_viewer_frames(&mut self) {
        self.store.clear_viewer_frames();
    }

    /// Clear every payload, failure key, and explicit pin.
    pub(crate) fn clear_all(&mut self) {
        self.store.clear_all();
    }

    /// Return playback-owned residency and admission evidence.
    pub(crate) fn diagnostics(&self) -> PreviewCpuFrameStoreDiagnostics {
        self.store.diagnostics()
    }
}
