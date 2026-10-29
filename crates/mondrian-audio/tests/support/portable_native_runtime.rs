//! Shared deterministic input and original-owner census for finite native Runtime tests.

use mondrian_audio::{AudioDecodedSource, AudioMediaResolver, ResolvedAudioSource};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioSourceComponentId, ExecutionCancellationToken,
    NativeChildSettlement, NativeChildSettlementError, NativeChildSettlementReporter,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct Signal {
    pub(super) asset: AssetId,
    pub(super) layout: AudioChannelLayout,
    pub(super) rate: u32,
}

pub(super) fn sample(frame: i64, channel: usize) -> f32 {
    if frame < 0 {
        return 0.0;
    }
    (((frame as usize * 29 + channel * 11) % 113) as f32 - 56.0) / 256.0
}

impl AudioMediaResolver for Signal {
    fn resolve(
        &self,
        asset: AssetId,
        component: AudioSourceComponentId,
        rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        assert_eq!(
            (asset, component, rate),
            (self.asset, AudioSourceComponentId::primary(), self.rate)
        );
        Ok(ResolvedAudioSource::new(
            self.layout,
            Arc::new(self.clone()),
        ))
    }
}

impl AudioDecodedSource for Signal {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        output: &mut [f32],
        cancel: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        assert!(!cancel.is_canceled());
        let channels = self.layout.channel_count();
        assert_eq!(output.len(), frames * channels);
        for (index, frame) in output.chunks_exact_mut(channels).enumerate() {
            for (channel, value) in frame.iter_mut().enumerate() {
                *value = sample(start + index as i64, channel);
            }
        }
        Ok(())
    }
}

// Observe the original cross-platform child owners. No PID polling or second
// process owner; this evidence does not certify graceful-shutdown timing.
#[derive(Default)]
pub(super) struct Counts {
    pub(super) scopes: usize,
    pub(super) children: usize,
    pub(super) settled: usize,
    pub(super) unavailable: bool,
}
#[derive(Default)]
pub(super) struct Census(Mutex<Counts>);
impl Census {
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Counts> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.unavailable = true;
                state
            }
        }
    }
    fn begin(&self, child: bool) -> Result<(), NativeChildSettlementError> {
        let mut state = self.lock();
        if state.unavailable {
            return Err(NativeChildSettlementError::Unavailable);
        }
        let count = if child {
            &mut state.children
        } else {
            &mut state.scopes
        };
        match count.checked_add(1) {
            Some(next) => {
                *count = next;
                Ok(())
            }
            None => {
                state.unavailable = true;
                Err(NativeChildSettlementError::Overflow)
            }
        }
    }
    fn end(&self, child: bool) {
        let mut state = self.lock();
        let count = if child {
            &mut state.children
        } else {
            &mut state.scopes
        };
        match count.checked_sub(1) {
            Some(next) => *count = next,
            None => state.unavailable = true,
        }
    }
}
impl NativeChildSettlementReporter for Census {
    fn begin_scope(&self) -> Result<(), NativeChildSettlementError> {
        self.begin(false)
    }
    fn end_scope(&self) {
        self.end(false);
    }
    fn begin_child(&self) -> Result<(), NativeChildSettlementError> {
        self.begin(true)
    }
    fn end_child(&self, outcome: NativeChildSettlement) {
        self.end(true);
        let mut state = self.lock();
        match (outcome, state.settled.checked_add(1)) {
            (NativeChildSettlement::Settled, Some(next)) => state.settled = next,
            _ => state.unavailable = true,
        }
    }
    fn unavailable(&self) {
        self.lock().unavailable = true;
    }
}
