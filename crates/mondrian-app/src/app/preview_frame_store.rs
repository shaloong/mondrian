//! App Adapter for the playback-owned Preview Frame Store.
//!
//! This Module maps Mondrian media and Preview raster payloads to opaque playback keys,
//! exact byte reservations, and an exact presentation scope. Residency,
//! admission, eviction, failure memory, and continuity residency remain playback-owned.

use mondrian_core::types::SequenceId;

use super::preview_access_mode::MediaPreviewKey;
use super::preview_execution::PreviewOutputKey as ViewerPreviewCacheKey;
use super::preview_media_frame::MediaPreviewFrame;
use super::preview_raster_frame::PreviewRasterFrame;

pub(crate) type PreviewFrameStoreAdapterConfig = mondrian_playback::PreviewFrameStoreConfig;
pub(crate) type PreviewFrameStoreAdapterDiagnostics =
    mondrian_playback::PreviewFrameStoreDiagnostics;
pub(crate) type MediaWorkReservationAdmission = mondrian_playback::MediaWorkReservationAdmission;
pub(crate) type MediaFrameStoreAdmission = mondrian_playback::FrameStoreAdmission;
pub(crate) type MediaFrameProtectionError = mondrian_playback::MediaFrameProtectionError;

/// App contract error detected before playback-owned physical admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaWorkReservationAdapterError {
    /// The physical media source lacks immutable identity evidence required for reuse.
    UnstableMediaIdentity,
}

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
    PreviewRasterFrame,
    ViewerFrameScope,
>;

/// One final raster pinned for exact-scope stale reuse.
#[derive(Debug, Clone)]
pub(crate) struct ScopedPreviewRasterFrame {
    pub(crate) sequence_id: SequenceId,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frame: PreviewRasterFrame,
}

/// Thin App Adapter over the playback-owned Preview Frame Store Interface.
pub(crate) struct PreviewFrameStoreAdapter {
    store: PlaybackFrameStore,
}

impl Default for PreviewFrameStoreAdapter {
    fn default() -> Self {
        Self {
            store: PlaybackFrameStore::new(mondrian_playback::PreviewFrameStoreConfig::default()),
        }
    }
}

impl PreviewFrameStoreAdapter {
    /// Create a test Adapter with explicit budgets.
    #[cfg(test)]
    pub(crate) fn new(config: PreviewFrameStoreAdapterConfig) -> Self {
        Self { store: PlaybackFrameStore::new(config) }
    }

    /// Return and touch one decoded-media frame.
    pub(crate) fn media_frame(&mut self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        if !media_key_authorizes_residency(key) {
            return None;
        }
        self.store
            .media_frame(key)
            .map(|(frame, resource)| frame.with_residency_resource(resource))
    }

    /// Return one decoded frame protected for a current Viewer candidate.
    pub(crate) fn protected_media_frame(
        &mut self,
        key: &MediaPreviewKey,
        demand_id: mondrian_playback::MediaWorkDemandId,
    ) -> Result<Option<MediaPreviewFrame>, MediaFrameProtectionError> {
        if !media_key_authorizes_residency(key) {
            return Ok(None);
        }
        self.store.protected_media_frame(key, demand_id).map(|frame| {
            frame.map(|(frame, resource, protection)| {
                frame.with_residency_resource(resource).with_residency_protection(protection)
            })
        })
    }

    /// Reserve bounded decoded-media residency before Broker admission.
    pub(crate) fn reserve_media_work(
        &mut self,
        key: &MediaPreviewKey,
        intent: mondrian_playback::MediaWorkReservationIntent,
        reserved_bytes: usize,
        resource_units: usize,
    ) -> Result<MediaWorkReservationAdmission, MediaWorkReservationAdapterError> {
        if !media_key_authorizes_residency(key) {
            return Err(MediaWorkReservationAdapterError::UnstableMediaIdentity);
        }
        Ok(self.store.reserve_media_work(key, intent, reserved_bytes, resource_units))
    }

    /// Admit a decoded-media frame under its attempt's physical resource lease.
    pub(crate) fn insert_media_frame(
        &mut self,
        key: MediaPreviewKey,
        frame: MediaPreviewFrame,
        work: mondrian_playback::MediaWorkResourceLease,
    ) -> MediaFrameStoreAdmission {
        if !media_key_authorizes_residency(&key) {
            return MediaFrameStoreAdmission::RejectedCapacity;
        }
        let reserved_bytes = frame.reserved_cpu_bytes();
        let resource_units = frame.decoder_resource_units();
        let frame = frame.into_unbound_store_payload();
        self.store.admit_media_frame(key, frame, work, reserved_bytes, resource_units)
    }

    /// Return and touch one final Preview raster.
    pub(crate) fn viewer_frame(
        &mut self,
        key: &ViewerPreviewCacheKey,
    ) -> Option<PreviewRasterFrame> {
        self.store.viewer_frame(key)
    }

    /// Admit one final Preview raster under its encoded byte reservation.
    pub(crate) fn insert_viewer_frame(
        &mut self,
        key: ViewerPreviewCacheKey,
        frame: PreviewRasterFrame,
    ) -> bool {
        let reserved_bytes = frame.reserved_bytes();
        self.store.admit_viewer_frame(key, frame, reserved_bytes).is_resident()
    }

    /// Remember one terminal media failure.
    pub(crate) fn remember_failure(&mut self, key: MediaPreviewKey) {
        if !media_key_authorizes_residency(&key) {
            return;
        }
        self.store.remember_failure(key);
    }

    /// Remove failure memory after a successful completion.
    pub(crate) fn forget_failure(&mut self, key: &MediaPreviewKey) {
        if !media_key_authorizes_residency(key) {
            return;
        }
        self.store.forget_failure(key);
    }

    /// Return whether the key has a remembered terminal failure and touch it.
    pub(crate) fn contains_failure(&mut self, key: &MediaPreviewKey) -> bool {
        media_key_authorizes_residency(key) && self.store.contains_failure(key)
    }

    /// Pin the current Preview raster with its exact stale-reuse scope.
    pub(crate) fn pin_viewer_frame(&mut self, frame: ScopedPreviewRasterFrame) {
        let scope = ViewerFrameScope {
            sequence_id: frame.sequence_id,
            width: frame.width,
            height: frame.height,
        };
        let reserved_bytes = frame.frame.reserved_bytes();
        self.store.pin_viewer_frame(scope, frame.frame, reserved_bytes);
    }

    /// Return the pinned raster only when its presentation scope matches exactly.
    pub(crate) fn stale_viewer_frame(
        &self,
        sequence_id: SequenceId,
        width: u32,
        height: u32,
    ) -> Option<PreviewRasterFrame> {
        self.store.stale_viewer_frame(&ViewerFrameScope { sequence_id, width, height })
    }

    /// Release the non-evictable current/stale Viewer pin.
    pub(crate) fn clear_pinned_viewer_frame(&mut self) {
        self.store.clear_pinned_viewer_frame();
    }

    /// Release Store-owned current working-set overflow residency.
    #[cfg(test)]
    pub(crate) fn clear_current_media_overflow(&mut self) {
        self.store.clear_current_media_overflow();
    }

    /// Clear final Viewer residency and its current/stale pin.
    pub(crate) fn clear_viewer_frames(&mut self) {
        self.store.clear_viewer_frames();
    }

    /// Release decoded-media payloads without invalidating a usable final Viewer output.
    pub(crate) fn clear_media_frames(&mut self) {
        self.store.clear_media_frames();
    }

    /// Release native decoder resources while retaining ordinary CPU frames.
    pub(crate) fn clear_decoder_resource_media_frames(&mut self) {
        self.store.clear_decoder_resource_media_frames();
    }

    /// Apply persistent product residency limits and immediately evict
    /// ordinary entries that exceed them.
    pub(crate) fn reconfigure(&mut self, config: PreviewFrameStoreAdapterConfig) {
        self.store.reconfigure(config);
    }

    /// Clear every payload, failure key, and explicit pin.
    pub(crate) fn clear_all(&mut self) {
        self.store.clear_all();
    }

    /// Return playback-owned residency and admission evidence.
    pub(crate) fn diagnostics(&self) -> PreviewFrameStoreAdapterDiagnostics {
        self.store.diagnostics()
    }

    /// Exact speculative headroom after Store-exclusive media LRU release.
    pub(crate) fn media_prefetch_headroom(&self) -> mondrian_playback::MediaPrefetchHeadroom {
        self.store.media_prefetch_headroom()
    }
}

fn media_key_authorizes_residency(key: &MediaPreviewKey) -> bool {
    key.decode.source().fingerprint().authorizes_reuse()
}
