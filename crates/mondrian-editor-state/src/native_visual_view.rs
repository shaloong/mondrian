//! Current author-address evidence for a native visual effect GUI.
//!
//! A view does not own an instance, edit parameters, open history or suspend a
//! render deadline. The product must additionally prove a real visible effect
//! GUI and an actual Effects-bank invocation on its current owning worker.

use crate::{AuthorGeneration, AuthoringSession, AuthoringSessionId};
use mondrian_core::effect_data::EffectType;
use mondrian_core::{ClipId, EffectId, MondrianError, Result, SequenceId};

/// An issued, read-only address into one current open authoring lifetime.
///
/// Private fields prevent rebuilding an earlier view against a replacement
/// Session. This is not native execution or author-write authority. Native
/// parameter gestures must use the existing pending-state/history boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVisualAuthorView {
    session: AuthoringSessionId,
    generation: AuthorGeneration,
    sequence: SequenceId,
    clip: ClipId,
    effect: EffectId,
    definition: EffectType,
}

impl NativeVisualAuthorView {
    /// Specific open Session that issued this view.
    pub const fn session_id(&self) -> AuthoringSessionId {
        self.session
    }
    /// Exact author generation, invalidated by any subsequent author commit.
    pub const fn author_generation(&self) -> AuthorGeneration {
        self.generation
    }
    /// Sequence whose actual Clip occurrence supplies the effect GUI.
    pub const fn sequence_id(&self) -> SequenceId {
        self.sequence
    }
    /// Concrete Clip; a shared definition or display label is insufficient.
    pub const fn clip_id(&self) -> ClipId {
        self.clip
    }
    /// Exact authored effect occurrence, not a registry definition identity.
    pub const fn effect_id(&self) -> EffectId {
        self.effect
    }
    /// Definition selected at issue; runtime provider revision is independent.
    pub fn definition(&self) -> &EffectType {
        &self.definition
    }
    /// Known logical owner bytes, excluding allocator/Arc administration.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match &self.definition {
                EffectType::External(key) => key.capacity(),
                _ => 0,
            }
    }
}

impl AuthoringSession {
    /// Issue current author-address evidence without changing history or state.
    ///
    /// The caller separately proves that the corresponding effect GUI is
    /// actually active. Foreground mode, selection metadata or this handle
    /// alone must not be used to mark a native instance interactive.
    pub fn native_visual_author_view(
        &self,
        sequence: SequenceId,
        clip: ClipId,
        effect: EffectId,
    ) -> Result<NativeVisualAuthorView> {
        let sequence_data = self
            .active_sequence()
            .filter(|value| value.id == sequence)
            .ok_or_else(|| view_error("effect GUI Sequence is not active"))?;
        let location = sequence_data
            .clip_track_location(clip)
            .ok_or_else(|| view_error("effect GUI Clip is absent"))?;
        if !location.is_video_track || location.is_locked {
            return Err(view_error("effect GUI requires an unlocked visual Clip"));
        }
        let effect_data = sequence_data
            .find_clip(clip)
            .and_then(|value| value.effects.iter().find(|value| value.id == effect))
            .ok_or_else(|| view_error("effect GUI occurrence is absent"))?;
        Ok(NativeVisualAuthorView {
            session: self.session_id(),
            generation: self.author_generation(),
            sequence,
            clip,
            effect,
            definition: effect_data.effect_type.clone(),
        })
    }

    /// Whether an issued author view still addresses the same editable owner.
    ///
    /// Current GUI presence and processor-bank ownership must be admitted
    /// separately at publication and again before any actual user reply.
    pub fn native_visual_author_view_is_current(&self, view: &NativeVisualAuthorView) -> bool {
        if self.session_id() != view.session || self.author_generation() != view.generation {
            return false;
        }
        let Some(sequence) = self.active_sequence().filter(|value| value.id == view.sequence)
        else {
            return false;
        };
        if sequence
            .clip_track_location(view.clip)
            .is_none_or(|location| !location.is_video_track || location.is_locked)
        {
            return false;
        }
        sequence
            .find_clip(view.clip)
            .and_then(|clip| clip.effects.iter().find(|effect| effect.id == view.effect))
            .is_some_and(|effect| effect.effect_type == view.definition)
    }
}

fn view_error(reason: &str) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "native_visual_author_view".to_owned(),
        reason: reason.to_owned(),
    }
}
