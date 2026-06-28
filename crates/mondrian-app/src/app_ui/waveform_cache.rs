//! Background waveform generation for timeline audio clips.
//!
//! ## Architecture
//!
//! **Source layer** — one ffmpeg decode per asset:
//!   key = `(asset_id, asset_revision)`
//!   value = full-file 4096-point envelope (`Vec<f32>`) + metadata
//!
//! **Render layer** — on-the-fly re-sample, no ffmpeg:
//!   Given a source envelope and a `(start_fraction, end_fraction, pixel_width)`,
//!   the UI thread slices the relevant portion and re-samples to pixel_width.
//!   This is O(pixel_width + source_width) < 8000 ops — cheap enough to
//!   run inline during paint without a separate render cache.
//!
//! Decodes run on a dedicated `std::thread`.  The UI polls results each
//! frame via [`poll_finished`] and signals a repaint when new data arrives.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use mondrian_assets::AssetLibrary;
use mondrian_core::AssetId;
use mondrian_media::audio::decode_audio_file_with_ffmpeg_cli;
use mondrian_media::waveform::{compute_waveform, MAX_WAVEFORM_WIDTH};

thread_local! {
    static CACHE: RefCell<*const AudioWaveformCache> = const { RefCell::new(std::ptr::null()) };
}

// ── Source cache ──────────────────────────────────────────────────────────

/// Decoded source data for one audio asset.
#[derive(Debug, Clone)]
struct WaveformSource {
    /// Peak envelope covering the entire file at MAX_WAVEFORM_WIDTH.
    envelope: Vec<f32>,
    /// Total audio samples in the decoded buffer.
    total_samples: u64,
    /// Sample rate (Hz).
    sample_rate: u32,
}

/// (asset_id, revision_stamp)
type SourceKey = (AssetId, u64);

// ── Jobs ───────────────────────────────────────────────────────────────────

struct SourceJob {
    key: SourceKey,
    file_path: PathBuf,
}

struct SourceResult {
    key: SourceKey,
    source: Option<WaveformSource>,
}

// ── Public API ────────────────────────────────────────────────────────────

/// Host-owned cache that produces waveform peak data for timeline painting.
///
/// One instance lives in [`AppUiHost`](crate::app_ui::host::AppUiHost) and
/// is registered via [`register`] for paint-time access from
/// [`TimelineView`](mondrian_ui_widgets::TimelineView).
pub struct AudioWaveformCache {
    // Source layer
    source_cache: RefCell<HashMap<SourceKey, WaveformSource>>,
    source_pending: RefCell<HashSet<SourceKey>>,
    source_errors: RefCell<HashSet<SourceKey>>,

    // Background decode
    job_sender: mpsc::Sender<SourceJob>,
    result_receiver: RefCell<mpsc::Receiver<SourceResult>>,

    // Library for path resolution (set once after construction)
    library: RefCell<Option<Arc<AssetLibrary>>>,
}

impl AudioWaveformCache {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = mpsc::channel::<SourceJob>();
        let (result_sender, result_receiver) = mpsc::channel::<SourceResult>();

        std::thread::Builder::new()
            .name("mondrian-audio-waveform".into())
            .spawn(move || {
                for job in job_receiver {
                    let source = Self::decode_source(&job.file_path);
                    let _ = result_sender.send(SourceResult { key: job.key, source });
                }
            })
            .expect("spawn waveform worker");

        Self {
            source_cache: RefCell::new(HashMap::new()),
            source_pending: RefCell::new(HashSet::new()),
            source_errors: RefCell::new(HashSet::new()),
            job_sender,
            result_receiver: RefCell::new(result_receiver),
            library: RefCell::new(None),
        }
    }

    // ── library binding ──────────────────────────────────────────────────

    /// Bind the asset library so [`Self::lookup`] can resolve paths
    /// automatically.
    pub fn set_library(&self, library: Arc<AssetLibrary>) {
        *self.library.borrow_mut() = Some(library);
    }

    // ── poll ─────────────────────────────────────────────────────────────

    /// Drain completed source decodes.  Returns `true` if any new envelope
    /// data arrived (caller should request a repaint).
    ///
    /// On the very first call the cache self-registers via the thread-local
    /// pointer so paint-time lookups can find it.
    pub fn poll_finished(&self) -> bool {
        // Lazy registration — must happen AFTER the cache has been moved
        // into its final memory location (the host struct field), not
        // before the constructor returns.
        self.ensure_registered();
        let mut changed = false;
        let receiver = &mut *self.result_receiver.borrow_mut();
        while let Ok(result) = receiver.try_recv() {
            self.source_pending.borrow_mut().remove(&result.key);
            match result.source {
                Some(source) => {
                    self.source_cache.borrow_mut().insert(result.key, source);
                    changed = true;
                }
                None => {
                    self.source_errors.borrow_mut().insert(result.key);
                }
            }
        }
        changed
    }

    // ── lookup (paint-time, no blocking) ─────────────────────────────────

    /// Try to get waveform peaks for a visible region of an audio clip.
    ///
    /// * `asset_id` / `revision` — identity for invalidation
    /// * `start_secs` / `end_secs` — source time range of the visible clip portion
    /// * `pixel_width` — target column count for painting
    ///
    /// Returns `None` if the source is still decoding (retry next frame) or
    /// the decode failed.  Returns `Some(peaks)` when ready.
    pub fn lookup(
        &self,
        asset_id: AssetId,
        revision: u64,
        start_secs: f64,
        end_secs: f64,
        pixel_width: u32,
    ) -> Option<Vec<f32>> {
        let key = (asset_id, revision);

        // Check error cache.
        if self.source_errors.borrow().contains(&key) {
            return None;
        }

        // Check source cache.
        let source = {
            let cache = self.source_cache.borrow();
            cache.get(&key).cloned()
        };
        match source {
            Some(source) => {
                let total_secs = source.total_samples as f64 / source.sample_rate as f64;
                let start = (start_secs.max(0.0) / total_secs.max(0.001)).clamp(0.0, 1.0);
                let end = (end_secs.min(total_secs) / total_secs.max(0.001)).clamp(start, 1.0);
                let env_len = source.envelope.len();
                let start_idx = ((start * env_len as f64) as usize).min(env_len);
                let end_idx =
                    ((end * env_len as f64).ceil() as usize).min(env_len).max(start_idx + 1);
                let slice = &source.envelope[start_idx..end_idx];
                Some(resample_peaks(slice, pixel_width))
            }
            None => {
                // Auto-request decode if not already pending/errored.
                if !self.source_pending.borrow().contains(&key) {
                    drop(self.source_pending.borrow()); // release before request_source re-borrows
                    self.request_source(asset_id, revision);
                }
                None
            }
        }
    }

    /// Schedule a source decode job if not already pending/errored.
    ///
    /// Safe to call once per frame; duplicates are silently ignored.
    pub fn request_source(&self, asset_id: AssetId, revision: u64) {
        let key = (asset_id, revision);

        // Already cached, pending, or errored — skip.
        if self.source_cache.borrow().contains_key(&key)
            || self.source_pending.borrow().contains(&key)
            || self.source_errors.borrow().contains(&key)
        {
            return;
        }

        // Resolve file path.
        let lib = self.library.borrow();
        let Some(lib) = lib.as_ref() else {
            return;
        };
        let Ok(Some(record)) = lib.get_asset(asset_id) else {
            self.source_errors.borrow_mut().insert(key);
            return;
        };

        let path = record.path.clone();
        if self.job_sender.send(SourceJob { key, file_path: path }).is_ok() {
            self.source_pending.borrow_mut().insert(key);
        }
    }

    /// Invalidate all source data for an asset (relink, replace, delete).
    pub fn evict_asset(&self, asset_id: AssetId) {
        // We can't efficiently remove by partial key, so collect keys first.
        let keys: Vec<SourceKey> =
            self.source_cache.borrow().keys().filter(|k| k.0 == asset_id).copied().collect();
        for key in keys {
            self.source_cache.borrow_mut().remove(&key);
        }
        self.source_pending.borrow_mut().retain(|k| k.0 != asset_id);
        self.source_errors.borrow_mut().retain(|k| k.0 != asset_id);
    }

    // ── register ─────────────────────────────────────────────────────────

    /// Register this instance so that [`Self::try_with`] can access it.
    fn ensure_registered(&self) {
        CACHE.with(|c| {
            if c.borrow().is_null() {
                *c.borrow_mut() = self as *const _;
            }
        });
    }

    /// Access the registered instance from the current thread.
    pub fn try_with<F, R>(f: F) -> Option<R>
    where
        F: FnOnce(&AudioWaveformCache) -> R,
    {
        CACHE.with(|c| {
            let ptr = *c.borrow();
            if ptr.is_null() {
                None
            } else {
                Some(f(unsafe { &*ptr }))
            }
        })
    }

    // ── internal ─────────────────────────────────────────────────────────

    fn decode_source(file_path: &std::path::Path) -> Option<WaveformSource> {
        let buffer = decode_audio_file_with_ffmpeg_cli(file_path, 48000, 1).ok()?;
        let total_samples = buffer.frame_count() as u64;
        let envelope = compute_waveform(&buffer, MAX_WAVEFORM_WIDTH).peaks;
        // Release the raw PCM — only the envelope is kept.
        Some(WaveformSource {
            envelope,
            total_samples,
            sample_rate: buffer.sample_rate,
        })
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Re-sample `peaks` to `target_width` columns using nearest-neighbour
/// interpolation.  The caller is responsible for slicing the source
/// envelope to the visible range before calling.
fn resample_peaks(peaks: &[f32], target_width: u32) -> Vec<f32> {
    let target = target_width.clamp(1, MAX_WAVEFORM_WIDTH) as usize;
    if peaks.len() == target {
        return peaks.to_vec();
    }
    let mut out = vec![0.0f32; target];
    for (i, slot) in out.iter_mut().enumerate() {
        let src_idx = (i as f64 * peaks.len() as f64 / target as f64) as usize;
        *slot = peaks.get(src_idx).copied().unwrap_or(0.0);
    }
    out
}

// ── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_downscales() {
        let src: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let out = resample_peaks(&src, 10);
        assert_eq!(out.len(), 10);
        assert!(out.iter().all(|v| *v >= 0.0 && *v <= 1.0));
    }

    #[test]
    fn resample_upscales() {
        let src = vec![0.2, 0.8];
        let out = resample_peaks(&src, 5);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn resample_same_size_is_identity() {
        let src = vec![0.1, 0.5, 0.9];
        let out = resample_peaks(&src, 3);
        assert_eq!(out, src);
    }
}
