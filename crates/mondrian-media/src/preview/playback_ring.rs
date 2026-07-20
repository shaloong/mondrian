//! Small session-local forward frame ring for continuous Preview playback.
//!
//! The ring owns nearest-PTS lookup, LRU promotion, and fixed-capacity
//! eviction. It is deliberately session-local and cannot serve scrub/still
//! requests or become a second process-wide frame cache.

use super::*;

struct PreviewPlaybackRingEntry {
    pts: i64,
    frame: PreviewDecodedFramePayload,
}

pub(super) struct PreviewPlaybackRing {
    capacity: usize,
    entries: VecDeque<PreviewPlaybackRingEntry>,
}

impl PreviewPlaybackRing {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: VecDeque::new(),
        }
    }

    pub(super) fn get(
        &mut self,
        target_pts: i64,
        tolerance_pts: i64,
    ) -> Option<PreviewDecodedFramePayload> {
        let mut best_index = None;
        let mut best_distance = i64::MAX;
        for (index, entry) in self.entries.iter().enumerate() {
            let distance = (entry.pts - target_pts).abs();
            if distance < best_distance {
                best_distance = distance;
                best_index = Some(index);
            }
        }
        let index = best_index?;
        if best_distance > tolerance_pts.max(1) {
            return None;
        }

        let entry = self.entries.remove(index)?;
        let frame = entry.frame.clone();
        self.entries.push_front(entry);
        Some(frame)
    }

    pub(super) fn put(&mut self, pts: i64, frame: impl Into<PreviewDecodedFramePayload>) {
        let frame = frame.into();
        if let Some(index) = self.entries.iter().position(|entry| entry.pts == pts) {
            self.entries.remove(index);
        }
        self.entries.push_front(PreviewPlaybackRingEntry { pts, frame });
        while self.entries.len() > self.capacity {
            self.entries.pop_back();
        }
    }
}
