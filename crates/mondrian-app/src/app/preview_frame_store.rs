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
use super::preview_scheduler_policy::MediaPreviewResidencyReservation;

pub(crate) type PreviewFrameStoreAdapterConfig = mondrian_playback::PreviewFrameStoreConfig;
pub(crate) type PreviewFrameStoreAdapterDiagnostics =
    mondrian_playback::PreviewFrameStoreDiagnostics;
pub(crate) type MediaWorkReservationAdmission = mondrian_playback::MediaWorkReservationAdmission;
pub(crate) type MediaPrefetchBatchReservationAdmission =
    mondrian_playback::MediaPrefetchBatchReservationAdmission;
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
    PreviewViewerCpuFrame,
    ViewerFrameScope,
    super::preview_unavailability::PreviewUnavailability,
>;

/// One immutable CPU result admitted under the sole playback residency budget.
/// Working pixels retain their exact linear/alpha contract for GPU presentation.
#[derive(Debug, Clone)]
pub(crate) enum PreviewViewerCpuFrame {
    Raster(PreviewRasterFrame),
    Working(mondrian_renderer::CpuColorFrame),
}

/// Immutable working pixels and their one playback-issued physical charge.
#[derive(Debug, Clone)]
pub(crate) struct PreviewOwnedWorkingFrame {
    frame: mondrian_renderer::CpuColorFrame,
    resource: mondrian_playback::ViewerFrameResourceLease,
}

impl std::ops::Deref for PreviewOwnedWorkingFrame {
    type Target = mondrian_renderer::CpuColorFrame;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}

impl PreviewOwnedWorkingFrame {
    /// Retain the admitted allocation and charge as one ordered owner.
    pub(crate) fn new(
        frame: mondrian_renderer::CpuColorFrame,
        resource: mondrian_playback::ViewerFrameResourceLease,
    ) -> Self {
        Self { frame, resource }
    }

    /// Share only the existing charge after a complete GPU candidate owns the pixels.
    pub(crate) fn resource(&self) -> &mondrian_playback::ViewerFrameResourceLease {
        &self.resource
    }
}

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
        if let Some((frame, resource)) = self.store.media_frame(key) {
            return Some(frame.with_residency_resource(resource));
        }
        let (frame, resource) = self.store.media_frame_matching(|resident_key, frame| {
            if !resident_key.decode.same_configuration(&key.decode) {
                return false;
            }
            let mut retargeted = resident_key.clone();
            retargeted.decode = key.decode.clone();
            retargeted == *key && frame.native_target_selection(key.source_sample()).is_some()
        })?;
        frame
            .for_native_target(key)
            .map(|frame| frame.with_residency_resource(resource))
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
        let Some(frame) = self.media_frame(key) else {
            return Ok(None);
        };
        frame.protect_for_demand(demand_id).map(Some)
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

    /// Atomically reserve one complete speculative frame dependency closure.
    pub(crate) fn reserve_media_prefetch_work_batch(
        &mut self,
        requests: &[(MediaPreviewKey, MediaPreviewResidencyReservation)],
    ) -> Result<MediaPrefetchBatchReservationAdmission, MediaWorkReservationAdapterError> {
        if requests.iter().any(|(key, _)| !media_key_authorizes_residency(key)) {
            return Err(MediaWorkReservationAdapterError::UnstableMediaIdentity);
        }
        let requests = requests
            .iter()
            .map(|(key, reservation)| {
                (
                    key,
                    reservation.cpu_bytes,
                    reservation.decoder_resource_units,
                )
            })
            .collect::<Vec<_>>();
        Ok(self.store.reserve_media_prefetch_work_batch(&requests))
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
        match self.store.viewer_frame(key)? {
            PreviewViewerCpuFrame::Raster(frame) => Some(frame),
            PreviewViewerCpuFrame::Working(_) => None,
        }
    }

    /// Admit one final Preview raster under its encoded byte reservation.
    pub(crate) fn insert_viewer_frame(
        &mut self,
        key: ViewerPreviewCacheKey,
        frame: PreviewRasterFrame,
    ) -> bool {
        let reserved_bytes = frame.reserved_bytes();
        self.store
            .admit_viewer_frame(key, PreviewViewerCpuFrame::Raster(frame), reserved_bytes)
            .is_resident()
    }

    /// Borrow an exact result with its charge retained by the returned owner.
    pub(crate) fn viewer_working_frame(
        &mut self,
        key: &ViewerPreviewCacheKey,
    ) -> Option<PreviewOwnedWorkingFrame> {
        let (frame, resource) = self.store.owned_viewer_frame(key)?;
        match frame {
            PreviewViewerCpuFrame::Working(frame) => {
                Some(PreviewOwnedWorkingFrame { frame, resource })
            }
            PreviewViewerCpuFrame::Raster(frame) => {
                drop(frame);
                drop(resource);
                None
            }
        }
    }

    /// Detach one exact single-use completion; external ownership stays charged.
    pub(crate) fn detach_viewer_working_frame(
        &mut self,
        key: &ViewerPreviewCacheKey,
    ) -> Option<PreviewOwnedWorkingFrame> {
        let (frame, resource) = self.store.detach_owned_viewer_frame(key)?;
        match frame {
            PreviewViewerCpuFrame::Working(frame) => {
                Some(PreviewOwnedWorkingFrame { frame, resource })
            }
            PreviewViewerCpuFrame::Raster(frame) => {
                drop(frame);
                drop(resource);
                None
            }
        }
    }

    /// Reserve the known actual pixel extent before queueing CPU allocation.
    pub(crate) fn reserve_working_result(
        &mut self,
        bytes: usize,
    ) -> Option<mondrian_playback::ViewerFrameWorkLease> {
        self.store.reserve_viewer_work(bytes)
    }

    /// Publish completed pixels using the original pre-allocation admission.
    pub(crate) fn insert_admitted_working_frame(
        &mut self,
        key: ViewerPreviewCacheKey,
        frame: mondrian_renderer::CpuColorFrame,
        resource: mondrian_playback::ViewerFrameResourceLease,
    ) -> bool {
        if frame.rgba_f32().data.capacity().checked_mul(std::mem::size_of::<[f32; 4]>())
            != Some(resource.reserved_bytes())
        {
            drop(frame);
            drop(resource);
            return false;
        }
        self.store
            .admit_owned_viewer_frame(key, PreviewViewerCpuFrame::Working(frame), resource)
            .is_resident()
    }

    /// Admit a completed test-owned allocation through the same reservation seam.
    #[cfg(test)]
    pub(crate) fn insert_viewer_working_frame(
        &mut self,
        key: ViewerPreviewCacheKey,
        frame: mondrian_renderer::CpuColorFrame,
    ) -> bool {
        let Some(bytes) =
            frame.rgba_f32().data.capacity().checked_mul(std::mem::size_of::<[f32; 4]>())
        else {
            return false;
        };
        let Some(work) = self.store.reserve_viewer_work(bytes) else {
            return false;
        };
        let Ok(resource) = work.finish(bytes) else {
            return false;
        };
        self.insert_admitted_working_frame(key, frame, resource)
    }

    /// Remember one terminal media failure.
    pub(crate) fn remember_failure(
        &mut self,
        key: MediaPreviewKey,
        failure: super::preview_unavailability::PreviewUnavailability,
    ) {
        if !media_key_authorizes_residency(&key) {
            return;
        }
        self.store.remember_failure(key, failure);
    }

    /// Remove failure memory after a successful completion.
    pub(crate) fn forget_failure(&mut self, key: &MediaPreviewKey) {
        if !media_key_authorizes_residency(key) {
            return;
        }
        self.store.forget_failure(key);
    }

    /// Return whether the key has a remembered terminal failure and touch it.
    #[cfg(test)]
    pub(crate) fn contains_failure(&mut self, key: &MediaPreviewKey) -> bool {
        media_key_authorizes_residency(key) && self.store.contains_failure(key)
    }

    /// Project the retained decoder failure without replacing its explanation.
    pub(crate) fn failure(
        &mut self,
        key: &MediaPreviewKey,
    ) -> Option<super::preview_unavailability::PreviewUnavailability> {
        if !media_key_authorizes_residency(key) {
            return None;
        }
        self.store.failure(key)
    }

    /// Pin the current Preview raster with its exact stale-reuse scope.
    pub(crate) fn pin_viewer_frame(&mut self, frame: ScopedPreviewRasterFrame) {
        let scope = ViewerFrameScope {
            sequence_id: frame.sequence_id,
            width: frame.width,
            height: frame.height,
        };
        let reserved_bytes = frame.frame.reserved_bytes();
        self.store.pin_viewer_frame(
            scope,
            PreviewViewerCpuFrame::Raster(frame.frame),
            reserved_bytes,
        );
    }

    /// Return the pinned raster only when its presentation scope matches exactly.
    pub(crate) fn stale_viewer_frame(
        &self,
        sequence_id: SequenceId,
        width: u32,
        height: u32,
    ) -> Option<PreviewRasterFrame> {
        match self
            .store
            .stale_viewer_frame(&ViewerFrameScope { sequence_id, width, height })?
        {
            PreviewViewerCpuFrame::Raster(frame) => Some(frame),
            PreviewViewerCpuFrame::Working(_) => None,
        }
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

    /// A near-keyframe scrub payload proves no exact source-sample residency
    /// in the next generation. Preserve exact CPU frames and final output.
    pub(crate) fn retire_approximate_media_frames(&mut self) {
        self.store.clear_media_frames_matching(|_, frame| {
            frame.presentation_quality() == mondrian_playback::FramePresentationQuality::Degraded
        });
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

#[cfg(test)]
mod working_tests {
    use super::*;

    #[test]
    fn cpu_working_residency_counts_capacity_and_preserves_shared_linear_pixels() {
        let mut data = Vec::with_capacity(5);
        data.push([-0.5, 2.0, 0.25, 0.5]);
        let bytes = data.capacity() * std::mem::size_of::<[f32; 4]>();
        let frame = mondrian_renderer::CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            width: 1,
            height: 1,
            data,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            color_space: mondrian_core::WorkingColorSpace::LinearRec2020,
        });
        let key = ViewerPreviewCacheKey::new(
            SequenceId::new(),
            1,
            1,
            super::super::preview_execution::PreviewSemanticIdentityBuilder::new(
                b"working-capacity",
            )
            .finish_identity(),
        );
        let mut refused = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
            viewer_byte_budget: bytes - 1,
            ..Default::default()
        });
        assert!(!refused.insert_viewer_working_frame(key.clone(), frame.clone()));
        assert_eq!(refused.diagnostics().viewer_reserved_bytes, 0);
        let mut admitted = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
            viewer_byte_budget: bytes,
            ..Default::default()
        });
        assert!(admitted.insert_viewer_working_frame(key.clone(), frame.clone()));
        let retained = admitted.viewer_working_frame(&key).expect("exact working result");
        assert!(retained.shares_storage_with(&frame));
        assert_eq!(retained.rgba_f32().data, [[-0.5, 2.0, 0.25, 0.5]]);
        assert_eq!(retained.descriptor(), frame.descriptor());
        assert!(admitted.viewer_frame(&key).is_none());
        assert_eq!(admitted.diagnostics().viewer_reserved_bytes, bytes);
        admitted.clear_viewer_frames();
        assert!(admitted.viewer_working_frame(&key).is_none());
        assert_eq!(admitted.diagnostics().viewer_reserved_bytes, 0);
    }

    #[test]
    fn cpu_working_external_owner_remains_charged_after_store_clear() {
        let frame = mondrian_renderer::CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            width: 1,
            height: 1,
            data: vec![[0.5, 0.25, 0.75, 1.0]],
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        });
        let bytes = frame.rgba_f32().data.capacity() * std::mem::size_of::<[f32; 4]>();
        let key = ViewerPreviewCacheKey::new(
            SequenceId::new(),
            1,
            1,
            super::super::preview_execution::PreviewSemanticIdentityBuilder::new(
                b"live-working-owner",
            )
            .finish_identity(),
        );
        let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
            viewer_byte_budget: bytes,
            ..Default::default()
        });
        assert!(store.insert_viewer_working_frame(key.clone(), frame));
        let retained = store.viewer_working_frame(&key).expect("actual external working owner");
        store.clear_viewer_frames();
        assert!(store.viewer_working_frame(&key).is_none());
        let another =
            mondrian_renderer::CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                width: 1,
                height: 1,
                data: vec![[0.1, 0.2, 0.3, 1.0]],
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            });
        assert!(
            !store.insert_viewer_working_frame(key.clone(), another.clone()),
            "live detached working pixels must retain their Viewer residency charge"
        );
        assert_eq!(retained.rgba_f32().data, [[0.5, 0.25, 0.75, 1.0]]);
        drop(retained);
        assert!(
            store.insert_viewer_working_frame(key, another),
            "last external retirement restores actual headroom"
        );
    }
}
