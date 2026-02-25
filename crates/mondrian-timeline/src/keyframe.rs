//! 关键帧系统
//!
//! 支持多种插值类型：Hold / Linear / Bezier
//! 泛型设计，支持 f32、Vec2、Vec3、Color 等值类型。

use glam::Vec2;
use mondrian_core::types::TimeCode;
use serde::{Deserialize, Serialize};

// ─── 插值类型 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InterpolationType {
    /// 阶梯：保持前一关键帧值，直到下一关键帧
    Hold,
    /// 线性插值
    #[default]
    Linear,
    /// 贝塞尔曲线（控制点手动设置）
    Bezier,
    /// 缓入（贝塞尔预设）
    EaseIn,
    /// 缓出（贝塞尔预设）
    EaseOut,
    /// 缓入缓出（贝塞尔预设）
    EaseInOut,
}

// ─── 可插值 Trait ─────────────────────────────────────────────────────────────

/// 可在关键帧之间插值的值类型
pub trait Interpolatable: Clone + std::fmt::Debug + Send + Sync {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
}

impl Interpolatable for f32 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl Interpolatable for f64 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t as f64
    }
}

impl Interpolatable for Vec2 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        Vec2::lerp(*a, *b, t)
    }
}

impl Interpolatable for [f32; 4] {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ]
    }
}

// ─── 关键帧 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe<T> {
    pub time:         TimeCode,
    pub value:        T,
    pub interpolation: InterpolationType,
    /// 贝塞尔控制点（时间偏移, 值偏移）— 仅 Bezier 有效
    pub control_in:   Option<Vec2>,
    pub control_out:  Option<Vec2>,
}

// ─── 关键帧轨道 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyframeTrack<T: Interpolatable> {
    keyframes: Vec<Keyframe<T>>,
    /// 静态值（无关键帧时使用）
    static_value: T,
}

impl<T: Interpolatable + Serialize + for<'de> Deserialize<'de>> KeyframeTrack<T> {
    pub fn constant(value: T) -> Self {
        Self { keyframes: vec![], static_value: value }
    }

    /// 在指定时间码处求值
    pub fn evaluate(&self, time: TimeCode) -> T {
        if self.keyframes.is_empty() {
            return self.static_value.clone();
        }

        // 时间在第一个关键帧之前
        if time <= self.keyframes.first().unwrap().time {
            return self.keyframes.first().unwrap().value.clone();
        }
        // 时间在最后一个关键帧之后
        if time >= self.keyframes.last().unwrap().time {
            return self.keyframes.last().unwrap().value.clone();
        }

        // 二分查找相邻关键帧
        let idx = self.keyframes
            .partition_point(|kf| kf.time <= time)
            .saturating_sub(1);

        let kf_a = &self.keyframes[idx];
        let kf_b = &self.keyframes[idx + 1];

        let t = normalize_time(time, kf_a.time, kf_b.time);

        match kf_a.interpolation {
            InterpolationType::Hold => kf_a.value.clone(),
            InterpolationType::Linear => T::lerp(&kf_a.value, &kf_b.value, t),
            InterpolationType::Bezier => {
                let t_bezier = solve_bezier_t(t, kf_a.control_out, kf_b.control_in);
                T::lerp(&kf_a.value, &kf_b.value, t_bezier)
            }
            InterpolationType::EaseIn => {
                let t_eased = ease_in(t);
                T::lerp(&kf_a.value, &kf_b.value, t_eased)
            }
            InterpolationType::EaseOut => {
                let t_eased = ease_out(t);
                T::lerp(&kf_a.value, &kf_b.value, t_eased)
            }
            InterpolationType::EaseInOut => {
                let t_eased = ease_in_out(t);
                T::lerp(&kf_a.value, &kf_b.value, t_eased)
            }
        }
    }

    /// 添加或更新关键帧（按时间排序）
    pub fn set_keyframe(&mut self, kf: Keyframe<T>) {
        let pos = self.keyframes.partition_point(|k| k.time < kf.time);
        if pos < self.keyframes.len() && self.keyframes[pos].time == kf.time {
            self.keyframes[pos] = kf;
        } else {
            self.keyframes.insert(pos, kf);
        }
    }

    /// 删除关键帧
    pub fn remove_keyframe(&mut self, time: TimeCode) -> Option<Keyframe<T>> {
        if let Some(pos) = self.keyframes.iter().position(|k| k.time == time) {
            Some(self.keyframes.remove(pos))
        } else {
            None
        }
    }

    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }
}

// ─── 辅助函数 ─────────────────────────────────────────────────────────────────

/// 将时间码归一化为 [0, 1] 范围
fn normalize_time(t: TimeCode, start: TimeCode, end: TimeCode) -> f32 {
    let total = (end.frame - start.frame) as f32;
    if total <= 0.0 { return 0.0; }
    (t.frame - start.frame) as f32 / total
}

/// 缓入（三次方）
fn ease_in(t: f32) -> f32 {
    t * t * t
}

/// 缓出（三次方）
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// 缓入缓出（三次方 S 曲线）
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// 贝塞尔 t 参数求解（数值方法）
///
/// 给定水平方向归一化时间 x，求贝塞尔曲线在 y 方向的值。
/// 使用牛顿-拉弗森迭代法。
fn solve_bezier_t(x: f32, _cp_out: Option<Vec2>, _cp_in: Option<Vec2>) -> f32 {
    // TODO: 实现完整贝塞尔参数化求解
    // 当前简化为线性插值
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::Rational;

    fn tc(frame: i64) -> TimeCode {
        TimeCode::new(frame, Rational::new(1, 25))
    }

    #[test]
    fn linear_interpolation() {
        let mut track = KeyframeTrack::<f32>::constant(0.0);
        track.set_keyframe(Keyframe {
            time: tc(0), value: 0.0,
            interpolation: InterpolationType::Linear,
            control_in: None, control_out: None,
        });
        track.set_keyframe(Keyframe {
            time: tc(100), value: 100.0,
            interpolation: InterpolationType::Linear,
            control_in: None, control_out: None,
        });

        let mid = track.evaluate(tc(50));
        assert!((mid - 50.0).abs() < 0.01, "Expected 50.0, got {mid}");
    }

    #[test]
    fn hold_interpolation() {
        let mut track = KeyframeTrack::<f32>::constant(0.0);
        track.set_keyframe(Keyframe {
            time: tc(0), value: 10.0,
            interpolation: InterpolationType::Hold,
            control_in: None, control_out: None,
        });
        track.set_keyframe(Keyframe {
            time: tc(50), value: 50.0,
            interpolation: InterpolationType::Hold,
            control_in: None, control_out: None,
        });

        // Hold：在 tc(0)~tc(50) 之间，值应保持 10.0
        assert_eq!(track.evaluate(tc(25)), 10.0);
    }
}
