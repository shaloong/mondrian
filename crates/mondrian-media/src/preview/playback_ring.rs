//! Small session-local forward frame ring for continuous Preview playback.
//!
//! The ring owns exact presentation-interval lookup, LRU promotion, and
//! fixed-capacity eviction. It is deliberately session-local, byte-bounded,
//! and cannot serve scrub/still requests or become a second process-wide frame
//! cache.

use super::*;

struct PreviewPlaybackRingEntry {
    extent: DecodedTemporalExtent,
    frame: PreviewDecodedFramePayload,
    reserved_bytes: usize,
}

pub(super) struct PreviewPlaybackRing {
    capacity: usize,
    byte_budget: usize,
    reserved_bytes: usize,
    entries: VecDeque<PreviewPlaybackRingEntry>,
}

impl PreviewPlaybackRing {
    pub(super) fn new(capacity: usize, byte_budget: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            byte_budget: byte_budget.max(1),
            reserved_bytes: 0,
            entries: VecDeque::new(),
        }
    }

    pub(super) fn get(
        &mut self,
        target_pts: i64,
    ) -> Option<(DecodedTemporalExtent, PreviewDecodedFramePayload)> {
        let mut best_index = None;
        let mut best_start = i64::MIN;
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.extent.covers(target_pts) && entry.extent.start_pts >= best_start {
                best_start = entry.extent.start_pts;
                best_index = Some(index);
            }
        }
        let index = best_index?;

        let entry = self.entries.remove(index)?;
        let frame = entry.frame.clone();
        let extent = entry.extent;
        self.entries.push_front(entry);
        Some((extent, frame))
    }

    pub(super) fn put(
        &mut self,
        extent: DecodedTemporalExtent,
        frame: impl Into<PreviewDecodedFramePayload>,
    ) -> bool {
        let frame = frame.into();
        let reserved_bytes = frame.reserved_cpu_bytes();
        if let Some(index) =
            self.entries.iter().position(|entry| entry.extent.start_pts == extent.start_pts)
        {
            if let Some(replaced) = self.entries.remove(index) {
                self.reserved_bytes = self.reserved_bytes.saturating_sub(replaced.reserved_bytes);
            }
        }
        if reserved_bytes > self.byte_budget {
            return false;
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(reserved_bytes);
        self.entries
            .push_front(PreviewPlaybackRingEntry { extent, frame, reserved_bytes });
        while self.entries.len() > self.capacity || self.reserved_bytes > self.byte_budget {
            let Some(evicted) = self.entries.pop_back() else {
                break;
            };
            self.reserved_bytes = self.reserved_bytes.saturating_sub(evicted.reserved_bytes);
        }
        self.entries
            .front()
            .is_some_and(|entry| entry.extent.start_pts == extent.start_pts)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.reserved_bytes = 0;
    }

    #[cfg(test)]
    pub(super) fn reserved_bytes(&self) -> usize {
        self.reserved_bytes
    }
}
