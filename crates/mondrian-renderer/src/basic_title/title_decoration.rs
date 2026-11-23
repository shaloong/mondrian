use std::collections::VecDeque;
use std::mem::size_of;

pub(super) const MAX_PIXELS: u64 = 4 * 1024 * 1024;
const MAX_JOB_BYTES: usize = 96 * 1024 * 1024;
const BACKGROUND_PAD: f64 = 8.0;
type Pixel = [f32; 4];

/// Already-validated evaluated property values in working-linear color.
#[derive(Clone, Copy, Debug)]
pub(super) struct Style {
    pub(super) fill: Pixel,
    pub(super) stroke_width: f32,
    pub(super) stroke_color: Pixel,
    pub(super) shadow_distance: f32,
    pub(super) shadow_softness: f32,
    pub(super) shadow_color: Pixel,
    pub(super) background_color: Pixel,
    pub(super) background_opacity: f32,
    pub(super) opacity: f32,
}

impl Style {
    fn stroke_active(self) -> bool {
        self.stroke_width > 0.0 && self.stroke_color[3] > 0.0
    }

    fn shadow_active(self) -> bool {
        self.shadow_distance > 0.0 && self.shadow_color[3] > 0.0
    }

    fn background_active(self) -> bool {
        self.background_opacity > 0.0 && self.background_color[3] > 0.0
    }

    pub(super) fn geometry_active(self) -> bool {
        self.stroke_active() || self.shadow_active() || self.background_active()
    }
}

/// Half-open sampled-pixel bounds, not clipped to the sampled canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) x0: i64,
    pub(super) y0: i64,
    pub(super) x1: i64,
    pub(super) y1: i64,
}

impl Rect {
    pub(super) fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn expanded(self, radius: i64) -> Result<Self, Error> {
        Ok(Self {
            x0: self.x0.checked_sub(radius).ok_or(Error::Overflow)?,
            y0: self.y0.checked_sub(radius).ok_or(Error::Overflow)?,
            x1: self.x1.checked_add(radius).ok_or(Error::Overflow)?,
            y1: self.y1.checked_add(radius).ok_or(Error::Overflow)?,
        })
    }

    fn shifted(self, distance: i64) -> Result<Self, Error> {
        Ok(Self {
            x0: self.x0.checked_add(distance).ok_or(Error::Overflow)?,
            y0: self.y0.checked_add(distance).ok_or(Error::Overflow)?,
            x1: self.x1.checked_add(distance).ok_or(Error::Overflow)?,
            y1: self.y1.checked_add(distance).ok_or(Error::Overflow)?,
        })
    }

    pub(super) fn dimensions(self) -> Result<(u32, u32), Error> {
        fn dimension(lo: i64, hi: i64) -> Result<u32, Error> {
            hi.checked_sub(lo)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|&n| n != 0)
                .ok_or(Error::Overflow)
        }
        Ok((dimension(self.x0, self.x1)?, dimension(self.y0, self.y1)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Error {
    Overflow,
    Pixels { width: u32, height: u32 },
    Bytes { required: usize, limit: usize },
    Allocation,
}

#[derive(Debug)]
pub(super) struct Output {
    pub(super) crop_x: i64,
    pub(super) crop_y: i64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pixels: Vec<Pixel>,
}

#[derive(Debug)]
pub(super) struct Raster {
    ink: Rect,
    crop: Rect,
    // The background rectangle is converted to crop-local coordinates once.
    background: Option<Rect>,
    dimensions: (u32, u32),
    width: usize,
    height: usize,
    style: Style,
    stroke_radius: usize,
    shadow_shift: usize,
    shadow_radius: usize,
    // Pre-admitted denominator for the full square, including exterior zeros.
    shadow_divisor: f64,
    ink_mask: Vec<u8>,
    stroke: Vec<u8>,
    scratch: Vec<u8>,
    queue: VecDeque<u32>,
    pixels: Vec<Pixel>,
}

impl Raster {
    /// Only for validated active geometry with nonempty first-pass ink bounds.
    /// Admit signed extents, dimensions and all job allocation sizes first;
    /// reserve everything before accepting any coverage.
    pub(super) fn prepare(ink: Rect, style: Style, uniform_scale: f32) -> Result<Self, Error> {
        ink.dimensions()?;
        if !uniform_scale.is_finite() || uniform_scale <= 0.0 {
            return Err(Error::Overflow);
        }
        let scale = f64::from(uniform_scale);
        let stroke = if style.stroke_active() {
            checked_sample((f64::from(style.stroke_width) * scale).ceil())?
        } else {
            0
        };
        let outline = ink.expanded(stroke)?;
        let (shift, blur, mut crop) = if style.shadow_active() {
            let shift = checked_sample(
                (f64::from(style.shadow_distance) * scale / std::f64::consts::SQRT_2).round(),
            )?;
            let blur = checked_sample((f64::from(style.shadow_softness) * scale).ceil())?;
            let shadow = outline.shifted(shift)?.expanded(blur)?;
            (shift, blur, outline.union(shadow))
        } else {
            (0, 0, outline)
        };
        let background = if style.background_active() {
            let background = ink.expanded(checked_sample((BACKGROUND_PAD * scale).ceil())?)?;
            crop = crop.union(background);
            Some(Rect {
                x0: background.x0.checked_sub(crop.x0).ok_or(Error::Overflow)?,
                y0: background.y0.checked_sub(crop.y0).ok_or(Error::Overflow)?,
                x1: background.x1.checked_sub(crop.x0).ok_or(Error::Overflow)?,
                y1: background.y1.checked_sub(crop.y0).ok_or(Error::Overflow)?,
            })
        } else {
            None
        };

        let dimensions @ (w, h) = crop.dimensions()?;
        if u64::from(w) * u64::from(h) > MAX_PIXELS {
            return Err(Error::Pixels { width: w, height: h });
        }
        let width = as_usize(i64::from(w))?;
        let height = as_usize(i64::from(h))?;
        let len = width.checked_mul(height).ok_or(Error::Overflow)?;
        let stroke_radius = as_usize(stroke)?;
        let shadow_shift = as_usize(shift)?;
        let shadow_radius = as_usize(blur)?;
        let window =
            |r: usize| r.checked_mul(2).and_then(|n| n.checked_add(1)).ok_or(Error::Overflow);
        let queue_len = if stroke_radius != 0 {
            window(stroke_radius)?.min(width.max(height))
        } else {
            0
        };
        let shadow_width = window(shadow_radius)? as u64;
        let shadow_divisor = shadow_width
            .checked_mul(shadow_width)
            .and_then(|n| n.checked_mul(255))
            .ok_or(Error::Overflow)? as f64;
        // Only three u8 planes at peak; the output buffer also stores blur
        // intermediates. Queue indexes fit in four bytes (each axis <= MAX_PIXELS).
        let stroke_len = if stroke_radius != 0 { len } else { 0 };
        let ink_len = if stroke_radius != 0 || style.shadow_active() {
            len
        } else {
            0
        };
        let plane_bytes = stroke_len
            .checked_mul(2)
            .and_then(|extra| ink_len.checked_add(extra))
            .ok_or(Error::Overflow)?;
        let required = len
            .checked_mul(size_of::<Pixel>())
            .and_then(|output_bytes| output_bytes.checked_add(plane_bytes))
            .and_then(|bytes| bytes.checked_add(queue_len.checked_mul(size_of::<u32>())?))
            .ok_or(Error::Overflow)?;
        if required > MAX_JOB_BYTES {
            return Err(Error::Bytes { required, limit: MAX_JOB_BYTES });
        }

        let mut queue = VecDeque::new();
        queue.try_reserve_exact(queue_len).map_err(|_| Error::Allocation)?;
        Ok(Self {
            ink,
            crop,
            background,
            dimensions,
            width,
            height,
            style,
            stroke_radius,
            shadow_shift,
            shadow_radius,
            shadow_divisor,
            ink_mask: zeroed(ink_len, 0)?,
            stroke: zeroed(stroke_len, 0)?,
            scratch: zeroed(stroke_len, 0)?,
            queue,
            pixels: zeroed(len, [0.0; 4])?,
        })
    }

    /// Coverage-over union of one Swash callback rectangle. Positions are
    /// the same signed sampled coordinates used in the first pass.
    pub(super) fn add_coverage(
        &mut self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        coverage: u8,
    ) -> Result<(), Error> {
        if coverage == 0 || width == 0 || height == 0 {
            return Ok(());
        }
        let x1 = x.checked_add(i64::from(width)).ok_or(Error::Overflow)?;
        let y1 = y.checked_add(i64::from(height)).ok_or(Error::Overflow)?;
        if x < self.ink.x0 || x1 > self.ink.x1 || y < self.ink.y0 || y1 > self.ink.y1 {
            return Err(Error::Overflow);
        }
        if self.style.opacity == 0.0 {
            // Keep checking the callback contract, but leave output zeroed.
            return Ok(());
        }
        // The checked ink membership and crop admission above bound these
        // differences and every row stride/pixel index by the admitted region.
        let left = (x - self.crop.x0) as usize;
        let right = (x1 - self.crop.x0) as usize;
        let top = (y - self.crop.y0) as usize;
        let bottom = (y1 - self.crop.y0) as usize;
        let alpha = f32::from(coverage) / 255.0;
        let retain = 1.0 - alpha;
        for row in top..bottom {
            let start = row * self.width;
            for pixel in &mut self.pixels[start + left..start + right] {
                // Union into the idle output alpha lane. Round only once
                // for the u8 geometry plane, not on every overlap.
                pixel[3] = alpha + pixel[3] * retain;
            }
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Output {
        if self.style.opacity != 0.0 {
            self.compose();
        }
        // Opacity zero preserves the admitted crop (and the caller has still
        // drawn/validated the face); the already zeroed output is transparent.
        Output {
            crop_x: self.crop.x0,
            crop_y: self.crop.y0,
            width: self.dimensions.0,
            height: self.dimensions.1,
            pixels: self.pixels,
        }
    }

    fn compose(&mut self) {
        let horizontal = Axis::horizontal(self.width, self.height);
        let vertical = Axis::vertical(self.width, self.height);
        let shadow_active = self.style.shadow_active();
        for (mask, pixel) in self.ink_mask.iter_mut().zip(&self.pixels) {
            *mask = (pixel[3] * 255.0).round() as u8;
        }
        if self.stroke_radius != 0 {
            max_axis(
                &self.ink_mask,
                &mut self.scratch,
                horizontal,
                self.stroke_radius,
                &mut self.queue,
            );
            max_axis(
                &self.scratch,
                &mut self.stroke,
                vertical,
                self.stroke_radius,
                &mut self.queue,
            );
        }
        let caster = if self.stroke_radius != 0 {
            &self.stroke
        } else {
            &self.ink_mask
        };
        let shifted = |x, y| shifted_sample(caster, self.width, x, y, self.shadow_shift);
        if shadow_active && self.shadow_radius != 0 {
            box_axis(
                &mut self.pixels,
                horizontal,
                self.shadow_radius,
                |_, row, x| u32::from(shifted(x, row)),
                |pixels, i, sum| {
                    // Store the exact integer word in an otherwise unused
                    // output lane. Sum <= 255 * MAX_PIXELS = 0x3fc00000.
                    pixels[i][0] = f32::from_bits(sum as u32);
                },
            );
            box_axis(
                &mut self.pixels,
                vertical,
                self.shadow_radius,
                |pixels, column, y| pixels[vertical.index(column, y)][0].to_bits(),
                |pixels, i, sum| pixels[i][1] = (sum as f64 / self.shadow_divisor) as f32,
            );
        }

        for (y, row) in self.pixels.chunks_exact_mut(self.width).enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                let i = y * self.width + x;
                let mut associated = [0.0; 4];
                if let Some(bg) = self.background {
                    let (x, y) = (x as i64, y as i64);
                    if (bg.x0..bg.x1).contains(&x) && (bg.y0..bg.y1).contains(&y) {
                        over(
                            &mut associated,
                            self.style.background_color,
                            self.style.background_opacity,
                        );
                    }
                }
                if shadow_active {
                    let coverage = if self.shadow_radius != 0 {
                        pixel[1]
                    } else {
                        f32::from(shifted(x, y)) / 255.0
                    };
                    over(&mut associated, self.style.shadow_color, coverage);
                }
                if self.stroke_radius != 0 {
                    over(
                        &mut associated,
                        self.style.stroke_color,
                        f32::from(self.stroke[i]) / 255.0,
                    );
                }
                over(&mut associated, self.style.fill, pixel[3]);
                let alpha = associated[3] * self.style.opacity;
                *pixel = if alpha > 0.0 {
                    // Uniform group opacity cancels in RGB when unassociating.
                    [
                        associated[0] / associated[3],
                        associated[1] / associated[3],
                        associated[2] / associated[3],
                        alpha,
                    ]
                } else {
                    [0.0; 4]
                };
            }
        }
    }
}

fn as_usize(value: i64) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| Error::Overflow)
}

fn checked_sample(value: f64) -> Result<i64, Error> {
    // i64::MAX itself rounds up to 2^63 in f64, so the upper bound is strict.
    if (0.0..i64::MAX as f64).contains(&value) {
        Ok(value as i64)
    } else {
        Err(Error::Overflow)
    }
}

fn zeroed<T: Copy>(len: usize, value: T) -> Result<Vec<T>, Error> {
    let mut result = Vec::new();
    result.try_reserve_exact(len).map_err(|_| Error::Allocation)?;
    result.resize(len, value);
    Ok(result)
}

pub(super) fn over(dst: &mut Pixel, straight: Pixel, coverage: f32) {
    let alpha = straight[3] * coverage;
    if alpha > 0.0 {
        let retain = 1.0 - alpha;
        for channel in 0..3 {
            dst[channel] = straight[channel] * alpha + dst[channel] * retain;
        }
        dst[3] = alpha + dst[3] * retain;
    }
}

fn shifted_sample(mask: &[u8], width: usize, x: usize, y: usize, shift: usize) -> u8 {
    if x >= shift && y >= shift {
        mask[(y - shift) * width + (x - shift)]
    } else {
        0
    }
}

#[derive(Clone, Copy)]
struct Axis {
    line_count: usize,
    line_len: usize,
    line_stride: usize,
    sample_stride: usize,
}

impl Axis {
    fn horizontal(width: usize, height: usize) -> Self {
        Self {
            line_count: height,
            line_len: width,
            line_stride: width,
            sample_stride: 1,
        }
    }

    fn vertical(width: usize, height: usize) -> Self {
        Self {
            line_count: width,
            line_len: height,
            line_stride: 1,
            sample_stride: width,
        }
    }

    fn index(self, line: usize, sample: usize) -> usize {
        line * self.line_stride + sample * self.sample_stride
    }

    fn window(self, center: usize, radius: usize) -> (usize, usize) {
        (
            center.saturating_sub(radius),
            center.saturating_add(radius).saturating_add(1).min(self.line_len),
        )
    }
}

// A monotone queue of relative indexes (max at the front); each sample is
// inserted and evicted at most once. O(pixels) even for kernels wider than a line.
fn max_axis(src: &[u8], dst: &mut [u8], axis: Axis, radius: usize, queue: &mut VecDeque<u32>) {
    for line in 0..axis.line_count {
        queue.clear();
        let mut end = 0;
        for center in 0..axis.line_len {
            let (lo, hi) = axis.window(center, radius);
            while queue.front().is_some_and(|&j| (j as usize) < lo) {
                queue.pop_front();
            }
            while end < hi {
                let value = src[axis.index(line, end)];
                while queue.back().is_some_and(|&j| src[axis.index(line, j as usize)] <= value) {
                    queue.pop_back();
                }
                queue.push_back(end as u32);
                end += 1;
            }
            // The centered window always includes the center, so nonempty.
            let index = *queue.front().expect("nonempty max window") as usize;
            dst[axis.index(line, center)] = src[axis.index(line, index)];
        }
    }
}

// Unnormalized sum. Skip exterior zero samples; the caller divides by the
// full square size, never by the clipped window size. Each sample is read
// at most twice per axis. Read/write lanes must be disjoint within a pass.
fn box_axis(
    pixels: &mut [Pixel],
    axis: Axis,
    radius: usize,
    read: impl Fn(&[Pixel], usize, usize) -> u32,
    write: impl Fn(&mut [Pixel], usize, u64),
) {
    for line in 0..axis.line_count {
        let (mut begin, mut end, mut sum) = (0, 0, 0u64);
        for center in 0..axis.line_len {
            let (lo, hi) = axis.window(center, radius);
            while begin < lo {
                sum -= u64::from(read(pixels, line, begin));
                begin += 1;
            }
            while end < hi {
                sum += u64::from(read(pixels, line, end));
                end += 1;
            }
            write(pixels, axis.index(line, center), sum);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: Pixel = [0.0, 0.0, 0.0, 1.0];
    const WHITE: Pixel = [1.0; 4];

    fn style() -> Style {
        Style {
            fill: WHITE,
            stroke_width: 0.0,
            stroke_color: BLACK,
            shadow_distance: 0.0,
            shadow_softness: 10.0,
            shadow_color: BLACK,
            background_color: BLACK,
            background_opacity: 0.0,
            opacity: 1.0,
        }
    }

    fn dot(x: i64, y: i64) -> Rect {
        Rect { x0: x, y0: y, x1: x + 1, y1: y + 1 }
    }

    fn at(out: &Output, x: i64, y: i64) -> Pixel {
        let (x, y) = (x - out.crop_x, y - out.crop_y);
        assert!((0..i64::from(out.width)).contains(&x));
        assert!((0..i64::from(out.height)).contains(&y));
        out.pixels[(y * i64::from(out.width) + x) as usize]
    }

    fn close(actual: Pixel, expected: Pixel) {
        for (a, b) in actual.into_iter().zip(expected) {
            assert!((a - b).abs() <= 2e-7, "{actual:?} != {expected:?}");
        }
    }

    #[test]
    fn stroke_is_square_with_coverage_over_and_ceiled_radius() {
        let style = Style { stroke_width: 1.1, ..style() };
        let mut raster = Raster::prepare(dot(0, 0), style, 1.0).unwrap();
        raster.add_coverage(0, 0, 1, 1, 128).unwrap();
        raster.add_coverage(0, 0, 1, 1, 128).unwrap();
        let out = raster.finish();
        assert_eq!(
            (out.crop_x, out.crop_y, out.width, out.height),
            (-2, -2, 5, 5)
        );
        let outline = 192.0 / 255.0;
        close(at(&out, -2, -2), [0.0, 0.0, 0.0, outline]);
        close(at(&out, 2, 2), [0.0, 0.0, 0.0, outline]);
        let fill = 1.0 - (127.0f32 / 255.0).powi(2);
        let combined = fill + outline * (1.0 - fill);
        close(
            at(&out, 0, 0),
            [fill / combined, fill / combined, fill / combined, combined],
        );
    }

    #[test]
    fn low_coverage_overlaps_accumulate_before_quantizing() {
        let style = Style { stroke_width: 1.0, ..style() };
        let mut raster = Raster::prepare(dot(0, 0), style, 1.0).unwrap();
        for _ in 0..1024 {
            raster.add_coverage(0, 0, 1, 1, 1).unwrap();
        }
        let alpha = 1.0 - (254.0f64 / 255.0).powi(1024);
        let outline = ((alpha * 255.0).round() / 255.0) as f32;
        close(at(&raster.finish(), -1, -1), [0.0, 0.0, 0.0, outline]);
    }

    #[test]
    fn hard_shadow_uses_dilated_caster_and_rounded_diagonal() {
        let style = Style {
            stroke_width: 1.0,
            shadow_distance: 4.0,
            shadow_softness: 0.0,
            shadow_color: [1.0, 0.0, 0.0, 0.5],
            ..style()
        };
        let mut raster = Raster::prepare(dot(-2, 3), style, 1.0).unwrap();
        raster.add_coverage(-2, 3, 1, 1, 255).unwrap();
        let out = raster.finish();
        assert_eq!(
            (out.crop_x, out.crop_y, out.width, out.height),
            (-3, 2, 6, 6)
        );
        assert_eq!(at(&out, -2, 3), WHITE);
        assert_eq!(at(&out, 2, 7), [1.0, 0.0, 0.0, 0.5]);
        assert_eq!(at(&out, 2, 2), [0.0; 4]);
    }

    #[test]
    fn soft_shadow_normalizes_against_full_kernel_and_preserves_tail() {
        // A one-code-value dot and 3x3 kernel: each shadow tap is <1 u8 code,
        // but none vanishes. Shift 3 > radius, so blurring before translating
        // within the final crop would lose the top/left shadow support.
        let style = Style {
            shadow_distance: 4.0,
            shadow_softness: 0.1,
            ..style()
        };
        let mut raster = Raster::prepare(dot(0, 0), style, 1.0).unwrap();
        raster.add_coverage(0, 0, 1, 1, 1).unwrap();
        let out = raster.finish();
        assert_eq!(
            (out.crop_x, out.crop_y, out.width, out.height),
            (0, 0, 5, 5)
        );
        for y in 0..5 {
            for x in 0..5 {
                let alpha = if (2..5).contains(&x) && (2..5).contains(&y) {
                    1.0 / 2295.0
                } else {
                    0.0
                };
                if (x, y) != (0, 0) {
                    close(at(&out, x, y), [0.0, 0.0, 0.0, alpha]);
                }
            }
        }
    }

    #[test]
    fn rectangular_multisample_filter_matches_direct_kernel() {
        let style = Style {
            fill: [0.0; 4],
            stroke_width: 1.0,
            stroke_color: [0.0, 0.0, 1.0, 1.0],
            shadow_distance: 3.0,
            shadow_softness: 2.0,
            shadow_color: [1.0, 0.0, 0.0, 1.0],
            ..style()
        };
        let ink = Rect { x0: -4, y0: 2, x1: 3, y1: 5 };
        let mut raster = Raster::prepare(ink, style, 1.0).unwrap();
        raster.add_coverage(-4, 2, 3, 1, 64).unwrap();
        raster.add_coverage(1, 3, 2, 2, 200).unwrap();
        raster.add_coverage(-3, 2, 1, 2, 160).unwrap();
        let out = raster.finish();
        assert_eq!(
            (out.crop_x, out.crop_y, out.width, out.height),
            (-5, 1, 13, 9)
        );
        let coverage = |x, y| match (x, y) {
            (-3, 2) => 184,
            (-4 | -2, 2) => 64,
            (-3, 3) => 160,
            (1 | 2, 3 | 4) => 200,
            _ => 0,
        };
        let dilated = |x, y| {
            let mut max = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    max = max.max(coverage(x + dx, y + dy));
                }
            }
            max
        };
        for y in 1..10 {
            for x in -5..8 {
                let mut sum = 0;
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        sum += dilated(x + dx - 2, y + dy - 2);
                    }
                }
                let stroke = dilated(x, y) as f32 / 255.0;
                let shadow_under = (sum as f32 / (255.0 * 25.0)) * (1.0 - stroke);
                let alpha = stroke + shadow_under;
                let pixel = if alpha > 0.0 {
                    [shadow_under / alpha, 0.0, stroke / alpha, alpha]
                } else {
                    [0.0; 4]
                };
                close(at(&out, x, y), pixel);
            }
        }
    }

    #[test]
    fn ordered_linear_alpha_composition_applies_opacity_once() {
        let style = Style {
            fill: [1.0, 1.0, 1.0, 0.5],
            stroke_width: 0.5,
            stroke_color: [0.0, 0.0, 1.0, 0.5],
            shadow_distance: 0.5, // active, but rounds to 0 pixels per component
            shadow_softness: 0.0,
            shadow_color: [0.0, 1.0, 0.0, 0.5],
            background_color: [1.0, 0.0, 0.0, 0.5],
            background_opacity: 0.5,
            opacity: 0.5,
        };
        let mut raster = Raster::prepare(dot(1, 2), style, 1.0).unwrap();
        raster.add_coverage(1, 2, 1, 1, 255).unwrap();
        let out = raster.finish();
        let alpha = 29.0 / 32.0;
        close(
            at(&out, 1, 2),
            [17.0 / 29.0, 20.0 / 29.0, 24.0 / 29.0, alpha * 0.5],
        );
        close(at(&out, -7, -6), [1.0, 0.0, 0.0, 0.125]);
    }

    #[test]
    fn background_wraps_ink_union_and_zero_opacity_keeps_crop() {
        let style = Style { background_opacity: 1.0, ..style() };
        let ink = dot(-3, -1).union(dot(2, 5));
        let run = |style| {
            let mut raster = Raster::prepare(ink, style, 0.2).unwrap();
            raster.add_coverage(-3, -1, 1, 1, 255).unwrap();
            raster.add_coverage(2, 5, 1, 1, 255).unwrap();
            raster.finish()
        };
        let out = run(style);
        assert_eq!(
            (out.crop_x, out.crop_y, out.width, out.height),
            (-5, -3, 10, 11)
        );
        assert_eq!(at(&out, 0, 2), BLACK);
        let zero = run(Style { opacity: 0.0, ..style });
        assert_eq!(
            (zero.crop_x, zero.crop_y, zero.width, zero.height),
            (-5, -3, 10, 11)
        );
        assert!(zero.pixels.iter().all(|&pixel| pixel == [0.0; 4]));
    }

    #[test]
    fn admission_checks_caps_signed_overflow_and_callback_contract() {
        let style = Style { stroke_width: 1.0, ..style() };
        let large = Rect { x0: 0, y0: 0, x1: 2048, y1: 2048 };
        assert_eq!(
            Raster::prepare(large, style, 1.0).unwrap_err(),
            Error::Pixels { width: 2050, height: 2050 },
        );
        assert_eq!(
            Raster::prepare(dot(i64::MIN, 0), style, 1.0).unwrap_err(),
            Error::Overflow,
        );
        assert_eq!(
            Raster::prepare(dot(0, 0), style, f32::MAX).unwrap_err(),
            Error::Overflow,
        );
        let mut raster = Raster::prepare(dot(0, 0), style, 1.0).unwrap();
        assert_eq!(raster.add_coverage(-1, 0, 1, 1, 1), Err(Error::Overflow));
    }

    #[test]
    fn inactive_gates_are_independent_of_hidden_values_and_title_opacity() {
        let style = Style {
            stroke_width: 5.0,
            stroke_color: [1.0, 0.0, 0.0, 0.0],
            shadow_distance: 10.0,
            shadow_color: [0.0; 4],
            background_opacity: 1.0,
            background_color: [0.0; 4],
            opacity: 0.0,
            ..style()
        };
        assert!(!style.geometry_active());
    }
}
