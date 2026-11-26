use crate::EffectPixelRoi;
use mondrian_core::ImageSampling;
use std::hash::{Hash, Hasher};

/// A source-pixel rectangle for comparing a processed picture to its input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectPreviewWindow {
    center: [f32; 2],
    size: [f32; 2],
    border: [f32; 4],
}

impl EffectPreviewWindow {
    /// Validate the authored geometry and an optional straight working-RGB border.
    pub fn new(center: [f32; 2], size: [f32; 2], border: [f32; 4]) -> Option<Self> {
        (center.into_iter().chain(size).chain(border).all(f32::is_finite)
            && size.into_iter().all(|value| value > 0.0)
            && (0.0..=1.0).contains(&border[3]))
        .then_some(Self { center, size, border })
    }

    /// Outer and inner bounds on the sampled grid, followed by the border color.
    /// Both axes are authored in source pixels, so only render density enters the mapping.
    pub fn gpu_controls(self, sampling: ImageSampling) -> [[f32; 4]; 3] {
        let [sx, sy] = sampling.render_scale().map(|scale| scale.to_f64() as f32);
        let [x, y] = self.center;
        let [w, h] = self.size.map(|size| size * 0.5);
        let outer = [(x - w) * sx, (y - h) * sy, (x + w) * sx, (y + h) * sy];
        let inner_w = (w - 1.0).max(0.0);
        let inner_h = (h - 1.0).max(0.0);
        let inner = [
            (x - inner_w) * sx,
            (y - inner_h) * sy,
            (x + inner_w) * sx,
            (y + inner_h) * sy,
        ];
        [outer, inner, self.border]
    }

    pub(crate) fn hash_controls(self, state: &mut impl Hasher) {
        self.center.map(f32::to_bits).hash(state);
        self.size.map(f32::to_bits).hash(state);
        self.border.map(f32::to_bits).hash(state);
    }

    pub(crate) fn apply<E>(
        self,
        original: &mut [[f32; 4]],
        processed: &[[f32; 4]],
        region: EffectPixelRoi,
        sampling: ImageSampling,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        let [outer, inner, border] = self.gpu_controls(sampling);
        let width = region.width() as usize;
        if width == 0 {
            return checkpoint();
        }
        for (row, (original, processed)) in
            original.chunks_mut(width).zip(processed.chunks(width)).enumerate()
        {
            checkpoint()?;
            let y = region.y() as f32 + row as f32;
            for (column, (original, processed)) in original.iter_mut().zip(processed).enumerate() {
                let x = region.x() as f32 + column as f32;
                let weight = rectangle_coverage(outer, x, y);
                if weight == 0.0 {
                    continue;
                }
                let border_weight = (weight - rectangle_coverage(inner, x, y)) * border[3];
                for channel in 0..3 {
                    let picture = if weight == 1.0 {
                        processed[channel]
                    } else {
                        original[channel] * (1.0 - weight) + processed[channel] * weight
                    };
                    original[channel] = if border_weight == 0.0 {
                        picture
                    } else {
                        picture * (1.0 - border_weight) + border[channel] * border_weight
                    };
                }
            }
        }
        checkpoint()
    }
}

fn rectangle_coverage(bounds: [f32; 4], x: f32, y: f32) -> f32 {
    let [left, top, right, bottom] = bounds;
    ((x + 1.0).min(right) - x.max(left)).clamp(0.0, 1.0)
        * ((y + 1.0).min(bottom) - y.max(top)).clamp(0.0, 1.0)
}
