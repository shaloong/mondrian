//! Ordered owner facts for a Window device replacement, including failed candidates.
//!
//! Append before evaluating qualification so a dirty retired generation cannot
//! disappear behind the final active generation's otherwise clean shutdown.

use serde::{Deserialize, Serialize};

use super::window::{AppUiActiveWindowGpuShutdownEvidence, AppUiPreActiveWindowShutdownEvidence};

const MAX_EVENTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum WindowGenerationEvent {
    Began {
        surface_generation: u64,
        device_generation: u64,
    },
    Activated {
        surface_generation: u64,
        device_generation: u64,
    },
    CandidateFailed {
        shutdown: AppUiPreActiveWindowShutdownEvidence,
    },
    Retired {
        shutdown: AppUiActiveWindowGpuShutdownEvidence,
    },
    Final {
        shutdown: AppUiActiveWindowGpuShutdownEvidence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowGenerationHistory {
    schema_version: u32,
    overflowed: bool,
    events: Vec<WindowGenerationEvent>,
}

impl Default for WindowGenerationHistory {
    fn default() -> Self {
        Self {
            schema_version: 1,
            overflowed: false,
            events: Vec::new(),
        }
    }
}

impl WindowGenerationHistory {
    pub(super) fn push(&mut self, event: WindowGenerationEvent) {
        if self.events.len() == MAX_EVENTS {
            self.overflowed = true;
        } else {
            self.events.push(event);
        }
    }

    pub(super) fn has_valid_shape(&self) -> bool {
        self.schema_version == 1 && self.events.len() <= MAX_EVENTS
    }

    /// Closure is distinct from successful recovery: a failed candidate can
    /// release its partial inventory while the operation still fails.
    pub(super) fn all_created_resources_released(&self) -> bool {
        use WindowGenerationEvent::*;
        if !self.has_valid_shape() || self.overflowed {
            return false;
        }
        match self.events.as_slice() {
            [] => true, // No replacement attempted; outer receipt owns startup facts.
            [Final { shutdown }] => shutdown.qualifies_normal_runtime(),
            [Began { surface_generation, device_generation }, CandidateFailed { shutdown }, Final { shutdown: final_shutdown }] => {
                *surface_generation != 0
                    && *device_generation != 0
                    && shutdown.all_created_resources_released()
                    && final_shutdown.qualifies_normal_runtime()
                    && final_shutdown.generation_identity()
                        == Some((*surface_generation, *device_generation))
            }
            [Began { surface_generation, device_generation }, Activated {
                surface_generation: next_surface,
                device_generation: next_device,
            }, Retired { shutdown }, Final { shutdown: final_shutdown }] => {
                *surface_generation != 0
                    && *device_generation != 0
                    && *next_surface != 0
                    && *next_device != 0
                    && next_surface != surface_generation
                    && next_device != device_generation
                    && shutdown.qualifies_normal_runtime()
                    && shutdown.generation_identity()
                        == Some((*surface_generation, *device_generation))
                    && final_shutdown.qualifies_normal_runtime()
                    && final_shutdown.generation_identity() == Some((*next_surface, *next_device))
            }
            _ => false,
        }
    }

    pub(super) fn matches_old_retirement(&self, old_json: &str) -> bool {
        let Some(WindowGenerationEvent::Retired { shutdown }) = self.events.get(2) else {
            return false;
        };
        let Ok(mut expected) = serde_json::from_str::<serde_json::Value>(old_json) else {
            return false;
        };
        let Some(object) = expected.as_object_mut() else {
            return false;
        };
        object.remove("schema_version");
        object.remove("surface_generation");
        object.remove("device_generation");
        let Ok(actual) = serde_json::to_value(shutdown) else {
            return false;
        };
        actual["retirement"]["retired"] == expected
    }

    pub(super) fn qualifies_recovery(
        &self,
        generations: (u64, u64, u64, u64),
        final_gpu: &AppUiActiveWindowGpuShutdownEvidence,
    ) -> bool {
        let (before_surface, after_surface, before_device, after_device) = generations;
        matches!(self.events.as_slice(), [
            WindowGenerationEvent::Began { surface_generation, device_generation },
            WindowGenerationEvent::Activated { surface_generation: next_surface, device_generation: next_device },
            WindowGenerationEvent::Retired { .. },
            WindowGenerationEvent::Final { shutdown },
        ] if (*surface_generation, *next_surface, *device_generation, *next_device)
            == (before_surface, after_surface, before_device, after_device) && shutdown == final_gpu)
            && self.all_created_resources_released()
    }
}

#[cfg(test)]
pub(super) fn test_history(
    before_surface: u64,
    after_surface: u64,
    before_device: u64,
    after_device: u64,
    old: AppUiActiveWindowGpuShutdownEvidence,
    final_gpu: AppUiActiveWindowGpuShutdownEvidence,
) -> WindowGenerationHistory {
    let mut history = WindowGenerationHistory::default();
    history.push(WindowGenerationEvent::Began {
        surface_generation: before_surface,
        device_generation: before_device,
    });
    history.push(WindowGenerationEvent::Activated {
        surface_generation: after_surface,
        device_generation: after_device,
    });
    history.push(WindowGenerationEvent::Retired { shutdown: old });
    history.push(WindowGenerationEvent::Final { shutdown: final_gpu });
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_attempt_and_overflow_never_prove_closed_inventory() {
        let mut history = WindowGenerationHistory::default();
        assert!(history.all_created_resources_released());
        for _ in 0..=MAX_EVENTS {
            history
                .push(WindowGenerationEvent::Began { surface_generation: 1, device_generation: 2 });
            assert!(!history.all_created_resources_released());
        }
        assert_eq!(history.events.len(), MAX_EVENTS);
        assert!(history.overflowed);
        let json = serde_json::to_string(&history).expect("bounded history");
        let decoded: WindowGenerationHistory =
            serde_json::from_str(&json).expect("raw diagnostic replay");
        assert!(!decoded.all_created_resources_released());
    }
}
