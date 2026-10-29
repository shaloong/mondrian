//! The one actual native view attachment, including unresolved teardown.

use crate::error::{EditorDetachStage, Error, Result};
use vst3::{
    ComPtr,
    Steinberg::{kResultOk, kResultTrue, IPlugView, IPlugViewTrait},
};

#[derive(Default)]
pub(super) enum EditorAttachment {
    #[default]
    Empty,
    Attached(ComPtr<IPlugView>),
    /// Native removed failed, so its platform attachment is unknown. Keep the
    /// exact view/frame alive and refuse another remove or any live UI call.
    RemovalFailed {
        view: ComPtr<IPlugView>,
        result: i32,
    },
    /// removed succeeded (or attach never succeeded), but frame cleanup did not.
    /// A retry may clear the frame only; removed must not be called twice.
    FrameClearPending(ComPtr<IPlugView>),
}

impl EditorAttachment {
    pub(super) fn pending_detach(&self) -> Option<EditorDetachStage> {
        match self {
            Self::RemovalFailed { .. } => Some(EditorDetachStage::Removal),
            Self::FrameClearPending(_) => Some(EditorDetachStage::FrameClear),
            Self::Empty | Self::Attached(_) => None,
        }
    }

    pub(super) fn is_present(&self) -> bool {
        !matches!(self, Self::Empty)
    }

    pub(super) fn live_view(&self) -> Result<Option<&ComPtr<IPlugView>>> {
        match self {
            Self::Empty => Ok(None),
            Self::Attached(view) => Ok(Some(view)),
            Self::RemovalFailed { .. } | Self::FrameClearPending(_) => {
                Err(Error::EditorLifecycleBlocked)
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn removal_complete(&self) -> bool {
        matches!(self, Self::Empty | Self::FrameClearPending(_))
    }

    pub(super) fn detach(&mut self) -> Result<()> {
        // Moving the current state does not drop the owned view. Every failed
        // native path puts the same object back before returning its error.
        let view = match std::mem::take(self) {
            Self::Empty => return Ok(()),
            Self::RemovalFailed { view, result } => {
                *self = Self::RemovalFailed { view, result };
                return Err(Error::EditorDetachFailed {
                    stage: EditorDetachStage::Removal,
                    result,
                });
            }
            Self::FrameClearPending(view) => view,
            Self::Attached(view) => {
                // SAFETY: only the same main owner invokes this method. This
                // exact view previously accepted attached and remains retained.
                let result = unsafe { view.removed() };
                if result != kResultOk && result != kResultTrue {
                    *self = Self::RemovalFailed { view, result };
                    return Err(Error::EditorDetachFailed {
                        stage: EditorDetachStage::Removal,
                        result,
                    });
                }
                view
            }
        };
        // SAFETY: removed accepted, or this view never accepted attached.
        // Clearing its frame can now be retried without unmatched removed.
        let result = unsafe { view.setFrame(std::ptr::null_mut()) };
        if result != kResultOk && result != kResultTrue {
            *self = Self::FrameClearPending(view);
            return Err(Error::EditorDetachFailed {
                stage: EditorDetachStage::FrameClear,
                result,
            });
        }
        Ok(())
    }

    pub(super) fn clear_unattached_frame(&mut self, view: ComPtr<IPlugView>) -> Result<()> {
        // Open already reports its original failure. Retain this frame-only
        // cleanup obligation if the native view does not accept clearing it.
        // It must never be treated as a successful platform attachment.
        let result = unsafe { view.setFrame(std::ptr::null_mut()) };
        if result != kResultOk && result != kResultTrue {
            *self = Self::FrameClearPending(view);
            return Err(Error::EditorDetachFailed {
                stage: EditorDetachStage::FrameClear,
                result,
            });
        }
        Ok(())
    }
}
