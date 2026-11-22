//! Immutable author-time payload shared by graph, Float32 and GPU execution.

use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

use mondrian_core::{ExecutionCancellationToken, ImageSampling};

use crate::{
    mask::MaskShape,
    mask_path_stroke::{
        ControlledMaskPathStrokeError, MaskPathStrokeControls, MaskPathStrokeError,
        MaskPathStrokeLayout,
    },
    EffectFrameExtent, StrokeGrade,
};

/// Resolved paths in Clip author order, with no dependency on host IDs or matte properties.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskPathStrokeGeometry {
    shapes: Arc<[MaskShape]>,
    grade: StrokeGrade,
    controls: MaskPathStrokeControls,
}

impl MaskPathStrokeGeometry {
    pub(crate) fn new(
        shapes: Vec<MaskShape>,
        grade: StrokeGrade,
        controls: MaskPathStrokeControls,
    ) -> Self {
        Self { shapes: shapes.into(), grade, controls }
    }

    /// Prepare analytic brush geometry on the actual input pixel grid.
    pub fn prepare(
        &self,
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<MaskPathStrokeLayout, MaskPathStrokeError> {
        MaskPathStrokeLayout::prepare(
            &self.shapes,
            extent,
            sampling,
            self.grade,
            self.controls,
            cancellation,
        )
    }

    pub(crate) fn prepare_controlled<E>(
        &self,
        extent: EffectFrameExtent,
        sampling: ImageSampling,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<MaskPathStrokeLayout, ControlledMaskPathStrokeError<E>> {
        MaskPathStrokeLayout::prepare_controlled(
            &self.shapes,
            extent,
            sampling,
            self.grade,
            self.controls,
            checkpoint,
        )
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.shapes.is_empty()
            || self.grade.is_identity()
            || self.controls.end_percent() <= self.controls.start_percent()
    }

    pub(crate) fn hash_signature(&self, state: &mut impl Hasher) {
        self.shapes.len().hash(state);
        for shape in &*self.shapes {
            crate::graph::shape_variant_hash(shape, state);
        }
        for channel in self.grade.color() {
            channel.to_bits().hash(state);
        }
        self.grade.brush_size().to_bits().hash(state);
        self.grade.brush_hardness().to_bits().hash(state);
        self.grade.opacity().to_bits().hash(state);
        self.grade.paint_style().hash(state);
        self.controls.sequential().hash(state);
        self.controls.start_percent().to_bits().hash(state);
        self.controls.end_percent().to_bits().hash(state);
        self.controls.spacing_percent().to_bits().hash(state);
    }
}
