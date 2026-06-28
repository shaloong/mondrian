//! Background waveform peak computation for timeline audio clips.
//!
//! Follows the same [`std::thread`] + [`mpsc::channel`] pattern as
//! [`AssetThumbnailCache`](crate::app_ui::asset_thumbnails::AssetThumbnailCache):
//! one dedicated OS thread decodes audio and computes peaks, then sends
//! results back via the result channel.  The UI polls the channel every
//! frame and inserts the data into [`TimelineClip`] view models.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use mondrian_core::AssetId;
use mondrian_media::audio::decode_audio_file_with_ffmpeg_cli;
use mondrian_media::waveform::{compute_waveform, WaveformData};

thread_local! {
    static CACHE: RefCell<*const AudioWaveformCache> = const { RefCell::new(std::ptr::null()) };
}

/// Maximum number of queued or in-flight waveform jobs.
const MAX_QUEUED_JOBS: usize = 32;

struct WaveformJob {
    asset_id: AssetId,
    file_path: std::path::PathBuf,
    pixel_width: u32,
}

struct WaveformResult {
    asset_id: AssetId,
    pixel_width: u32,
    data: Option<WaveformData>,
}

/// Host-owned cache that asynchronously produces waveform peak data for
/// timeline audio clips.
pub struct AudioWaveformCache {
    job_sender: mpsc::Sender<WaveformJob>,
    result_receiver: RefCell<mpsc::Receiver<WaveformResult>>,
    data: RefCell<HashMap<(AssetId, u32), WaveformData>>,
    pending: RefCell<HashSet<(AssetId, u32)>>,
    queued: RefCell<usize>,
}

impl AudioWaveformCache {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = mpsc::channel::<WaveformJob>();
        let (result_sender, result_receiver) = mpsc::channel::<WaveformResult>();

        std::thread::Builder::new()
            .name("mondrian-ui-audio-waveform".into())
            .spawn(move || {
                for job in job_receiver {
                    let data = match decode_audio_file_with_ffmpeg_cli(
                        &job.file_path,
                        48000, // mono decode is fast and sufficient for peaks
                        1,
                    ) {
                        Ok(buffer) => Some(compute_waveform(&buffer, job.pixel_width)),
                        Err(_) => None,
                    };
                    let _ = result_sender.send(WaveformResult {
                        asset_id: job.asset_id,
                        pixel_width: job.pixel_width,
                        data,
                    });
                }
            })
            .expect("spawn waveform worker thread");

        Self {
            job_sender,
            result_receiver: RefCell::new(result_receiver),
            data: RefCell::new(HashMap::new()),
            pending: RefCell::new(HashSet::new()),
            queued: RefCell::new(0),
        }
    }

    pub fn poll_finished(&self) -> bool {
        let mut had_results = false;
        let receiver = &mut *self.result_receiver.borrow_mut();
        while let Ok(result) = receiver.try_recv() {
            had_results = true;
            let key = (result.asset_id, result.pixel_width);
            self.pending.borrow_mut().remove(&key);
            *self.queued.borrow_mut() = self.queued.borrow().saturating_sub(1);
            if let Some(data) = result.data {
                self.data.borrow_mut().insert(key, data);
            }
        }
        had_results
    }

    pub fn get(
        &self,
        asset_id: AssetId,
        file_path: &std::path::Path,
        pixel_width: u32,
    ) -> Option<WaveformData> {
        let pixel_width = pixel_width.clamp(1, 4096).max(1);
        let key = (asset_id, pixel_width);

        if let Some(data) = self.data.borrow().get(&key) {
            return Some(data.clone());
        }
        if self.pending.borrow().contains(&key) {
            return None;
        }
        if *self.queued.borrow() >= MAX_QUEUED_JOBS {
            return None;
        }

        if self
            .job_sender
            .send(WaveformJob {
                asset_id,
                file_path: file_path.to_path_buf(),
                pixel_width,
            })
            .is_ok()
        {
            self.pending.borrow_mut().insert(key);
            *self.queued.borrow_mut() = self.queued.borrow().saturating_add(1);
        }

        None
    }

    /// Register this instance so that [`Self::try_with`] can access it
    /// from the current thread.
    pub fn register(&self) {
        CACHE.with(|c| *c.borrow_mut() = self as *const _);
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
}
