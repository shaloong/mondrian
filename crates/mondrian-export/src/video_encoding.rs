//! Typed video coding-structure admission.
//!
//! Presets describe editorial delivery intent. This module resolves that
//! intent against the exact output cadence before any frame is rendered, so
//! encoder adapters never inherit version-dependent GOP defaults.

use crate::preset::VideoCodecConfig;
use mondrian_core::Rational;
use serde::{Deserialize, Serialize};

/// Whether an H.26x encoder may insert additional keyframes at scene changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VideoSceneCutPolicy {
    /// Keep a fixed maximum GOP and permit earlier random-access points.
    Adaptive,
    /// Disable content-dependent scene-cut keyframes for deterministic GOPs.
    #[default]
    Disabled,
}

/// Codec-family-specific authored picture structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum VideoCodingStructure {
    /// Closed H.264/HEVC GOP with an explicit maximum interval and B-frame cap.
    H26xLongGop {
        /// Maximum keyframe interval in exact output-time seconds.
        keyframe_interval_seconds: u16,
        /// Maximum number of consecutive B pictures.
        max_b_frames: u8,
        /// Whether every GOP is independently decodable from its first IDR.
        closed_gop: bool,
        /// Content-dependent early keyframe policy.
        scene_cut: VideoSceneCutPolicy,
    },
    /// AV1 random-access structure. AV1 reference frames are not modeled as
    /// H.26x B pictures, so lookahead is expressed directly.
    Av1RandomAccess {
        /// Maximum keyframe interval in exact output-time seconds.
        keyframe_interval_seconds: u16,
        /// Encoder lookahead in frames.
        lookahead_frames: u16,
    },
    /// Every encoded picture is independently decodable.
    IntraOnly,
}

impl Default for VideoCodingStructure {
    fn default() -> Self {
        Self::h26x_delivery()
    }
}

impl VideoCodingStructure {
    /// Commercial H.264/HEVC delivery default: closed two-second GOP, up to
    /// three B pictures, and no content-dependent keyframe insertion.
    pub const fn h26x_delivery() -> Self {
        Self::H26xLongGop {
            keyframe_interval_seconds: 2,
            max_b_frames: 3,
            closed_gop: true,
            scene_cut: VideoSceneCutPolicy::Disabled,
        }
    }

    /// Balanced AV1 random-access delivery default.
    pub const fn av1_delivery() -> Self {
        Self::Av1RandomAccess { keyframe_interval_seconds: 2, lookahead_frames: 25 }
    }
}

/// Fully resolved picture structure consumed by one encoder invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedVideoCodingStructure {
    /// Exact H.264/HEVC GOP contract.
    H26xLongGop {
        /// Maximum/keyframe interval in encoded frames.
        keyframe_interval_frames: u32,
        /// Maximum number of consecutive B pictures.
        max_b_frames: u8,
        /// Whether open GOP references are forbidden.
        closed_gop: bool,
        /// Scene-cut keyframe policy.
        scene_cut: VideoSceneCutPolicy,
    },
    /// Exact AV1 random-access contract.
    Av1RandomAccess {
        /// Maximum keyframe interval in encoded frames.
        keyframe_interval_frames: u32,
        /// Encoder lookahead in frames.
        lookahead_frames: u16,
    },
    /// Intra-only encoded pictures.
    IntraOnly,
}

/// Resolve and validate one authored coding structure for an exact codec and cadence.
pub fn resolve_video_coding_structure(
    codec: &VideoCodecConfig,
    authored: VideoCodingStructure,
    frame_rate: Rational,
) -> Result<ResolvedVideoCodingStructure, String> {
    match (codec, authored) {
        (
            VideoCodecConfig::H264 { .. } | VideoCodecConfig::Hevc { .. },
            VideoCodingStructure::H26xLongGop {
                keyframe_interval_seconds,
                max_b_frames,
                closed_gop,
                scene_cut,
            },
        ) => {
            if max_b_frames > 4 {
                return Err("H.264/HEVC B 帧上限必须位于 0..=4".to_owned());
            }
            Ok(ResolvedVideoCodingStructure::H26xLongGop {
                keyframe_interval_frames: resolve_interval_frames(
                    keyframe_interval_seconds,
                    frame_rate,
                )?,
                max_b_frames,
                closed_gop,
                scene_cut,
            })
        }
        (
            VideoCodecConfig::Av1 { .. },
            VideoCodingStructure::Av1RandomAccess { keyframe_interval_seconds, lookahead_frames },
        ) => {
            if lookahead_frames > 120 {
                return Err("AV1 lookahead 必须位于 0..=120 帧".to_owned());
            }
            Ok(ResolvedVideoCodingStructure::Av1RandomAccess {
                keyframe_interval_frames: resolve_interval_frames(
                    keyframe_interval_seconds,
                    frame_rate,
                )?,
                lookahead_frames,
            })
        }
        (
            VideoCodecConfig::ProRes { .. } | VideoCodecConfig::Gif { .. },
            VideoCodingStructure::IntraOnly,
        ) => Ok(ResolvedVideoCodingStructure::IntraOnly),
        (VideoCodecConfig::H264 { .. } | VideoCodecConfig::Hevc { .. }, _) => {
            Err("H.264/HEVC 导出需要显式 H26x Long-GOP 编码结构".to_owned())
        }
        (VideoCodecConfig::Av1 { .. }, _) => {
            Err("AV1 导出需要显式 AV1 Random Access 编码结构".to_owned())
        }
        (VideoCodecConfig::ProRes { .. } | VideoCodecConfig::Gif { .. }, _) => {
            Err("ProRes/GIF 导出必须使用 Intra-only 编码结构".to_owned())
        }
    }
}

fn resolve_interval_frames(seconds: u16, frame_rate: Rational) -> Result<u32, String> {
    if !(1..=10).contains(&seconds) {
        return Err("关键帧间隔必须位于 1..=10 秒".to_owned());
    }
    if frame_rate.num <= 0 || frame_rate.den <= 0 {
        return Err("输出帧率必须为正有理数".to_owned());
    }
    let numerator = i128::from(frame_rate.num)
        .checked_mul(i128::from(seconds))
        .ok_or_else(|| "关键帧间隔计算溢出".to_owned())?;
    let denominator = i128::from(frame_rate.den);
    let frames = numerator
        .checked_add(denominator - 1)
        .ok_or_else(|| "关键帧间隔计算溢出".to_owned())?
        / denominator;
    u32::try_from(frames).map_err(|_| "关键帧间隔超出编码器容量".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{H264Profile, VideoRateControl};

    #[test]
    fn fractional_cadence_resolves_interval_without_float_rounding() {
        let codec = VideoCodecConfig::H264 {
            profile: H264Profile::High,
            rate_control: VideoRateControl::constant_quality(18),
        };

        let resolved = resolve_video_coding_structure(
            &codec,
            VideoCodingStructure::h26x_delivery(),
            Rational::FPS_2997,
        )
        .expect("two-second GOP");

        assert_eq!(
            resolved,
            ResolvedVideoCodingStructure::H26xLongGop {
                keyframe_interval_frames: 60,
                max_b_frames: 3,
                closed_gop: true,
                scene_cut: VideoSceneCutPolicy::Disabled,
            }
        );
    }

    #[test]
    fn codec_family_mismatch_fails_before_execution() {
        let codec = VideoCodecConfig::H264 {
            profile: H264Profile::High,
            rate_control: VideoRateControl::constant_quality(18),
        };

        let error = resolve_video_coding_structure(
            &codec,
            VideoCodingStructure::IntraOnly,
            Rational::FPS_25,
        )
        .expect_err("invalid family contract");

        assert!(error.contains("H26x Long-GOP"));
    }
}
