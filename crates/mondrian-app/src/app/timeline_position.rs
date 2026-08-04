//! Exact App-boundary lowering from explicit input grids to Sequence frames.
//!
//! Product Adapters retain `FramePosition` as one value until this Module has
//! converted it to exact author time. Callers then choose either semantic
//! nearest-grid lowering or exact-grid admission; no caller may discard the
//! input time base and reinterpret the bare frame on another Sequence.

use mondrian_core::{FramePosition, FrameRounding, MondrianError, TimelineTime};
use mondrian_timeline::Sequence;

/// Lower a nonnegative semantic position once onto the Sequence evaluation grid.
pub(super) fn lower_nearest_sequence_frame(
    sequence: &Sequence,
    position: FramePosition,
    step_id: &'static str,
) -> mondrian_core::Result<i64> {
    let time = nonnegative_time(position, step_id)?;
    Ok(time
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
        .frame)
}

/// Admit a position only when it lies exactly on the Sequence evaluation grid.
pub(super) fn lower_exact_sequence_frame(
    sequence: &Sequence,
    position: FramePosition,
    step_id: &'static str,
) -> mondrian_core::Result<i64> {
    let time = nonnegative_time(position, step_id)?;
    let resolved = time.to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?;
    if TimelineTime::from_frame_position(resolved)? != time {
        return Err(position_error(
            step_id,
            "Timeline position is not aligned to the active Sequence frame grid",
        ));
    }
    Ok(resolved.frame)
}

fn nonnegative_time(
    position: FramePosition,
    step_id: &'static str,
) -> mondrian_core::Result<TimelineTime> {
    let time = TimelineTime::from_frame_position(position)?;
    if time.is_negative() {
        return Err(position_error(
            step_id,
            "Timeline position must be non-negative",
        ));
    }
    Ok(time)
}

fn position_error(step_id: &'static str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}
