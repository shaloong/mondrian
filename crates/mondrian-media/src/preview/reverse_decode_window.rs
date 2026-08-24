//! Bounded session-local ownership for decoded reverse-playback candidates.
//!
//! FFmpeg decodes inter-frame GOPs forward even when editorial transport moves
//! backward. This window keeps a byte- and entry-bounded tail of that one
//! forward scan so adjacent reverse requests replay already decoded pictures
//! instead of seeking back to the same keyframe for every displayed frame.

use std::collections::VecDeque;

pub(super) struct ReverseDecodeWindow<T> {
    capacity: usize,
    byte_budget: usize,
    reserved_bytes: usize,
    entries: VecDeque<ReverseDecodeWindowEntry<T>>,
}

struct ReverseDecodeWindowEntry<T> {
    pts: i64,
    reserved_bytes: usize,
    value: T,
}

impl<T> ReverseDecodeWindow<T> {
    pub(super) fn new(capacity: usize, byte_budget: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            byte_budget: byte_budget.max(1),
            reserved_bytes: 0,
            entries: VecDeque::new(),
        }
    }

    pub(super) fn insert(&mut self, pts: i64, reserved_bytes: usize, value: T) -> bool {
        if reserved_bytes > self.byte_budget {
            return false;
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.pts == pts)
            && let Some(replaced) = self.entries.remove(index)
        {
            self.reserved_bytes = self.reserved_bytes.saturating_sub(replaced.reserved_bytes);
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(reserved_bytes);
        self.entries.push_back(ReverseDecodeWindowEntry { pts, reserved_bytes, value });
        while self.entries.len() > self.capacity || self.reserved_bytes > self.byte_budget {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.reserved_bytes = self.reserved_bytes.saturating_sub(evicted.reserved_bytes);
        }
        self.entries.back().is_some_and(|entry| entry.pts == pts)
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|entry| &entry.value)
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

#[cfg(test)]
mod tests {
    use super::ReverseDecodeWindow;

    #[test]
    fn window_evicts_oldest_entries_to_both_bounds() {
        let mut window = ReverseDecodeWindow::new(3, 8);
        assert!(window.insert(10, 3, "a"));
        assert!(window.insert(20, 3, "b"));
        assert!(window.insert(30, 3, "c"));
        assert_eq!(window.values().copied().collect::<Vec<_>>(), vec!["b", "c"]);
        assert_eq!(window.reserved_bytes(), 6);

        assert!(window.insert(40, 2, "d"));
        assert_eq!(
            window.values().copied().collect::<Vec<_>>(),
            vec!["b", "c", "d"]
        );
        assert_eq!(window.reserved_bytes(), 8);
    }

    #[test]
    fn oversized_entry_never_displaces_a_usable_window() {
        let mut window = ReverseDecodeWindow::new(2, 4);
        assert!(window.insert(10, 2, "a"));
        assert!(!window.insert(20, 5, "oversized"));
        assert_eq!(window.values().copied().collect::<Vec<_>>(), vec!["a"]);
        assert_eq!(window.reserved_bytes(), 2);
    }
}
