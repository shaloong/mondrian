//! 统一错误类型体系

use thiserror::Error;

/// Mondrian 全局错误枚举
#[derive(Debug, Error)]
pub enum MondrianError {
    // ── 媒体处理 ──────────────────────────────────────────────────────────────
    #[error("媒体文件无法打开: {path} — {reason}")]
    MediaOpen { path: String, reason: String },

    #[error("解码失败 (asset={asset_id}): {reason}")]
    DecodeFailed { asset_id: String, reason: String },

    /// A caller supplied an exact source revision, but the physical path no
    /// longer identifies that revision at the execution Adapter.
    #[error("媒体源修订在执行前已变化: {path} (expected={expected:?}, actual={actual:?})")]
    MediaSourceRevisionChanged {
        /// Physical source path checked immediately before execution.
        path: String,
        /// Revision that authorized the request's probe and execution contract.
        expected: Box<crate::MediaFileFingerprint>,
        /// Revision observed at the final execution check. An incomplete value
        /// means the path could no longer be observed.
        actual: Box<crate::MediaFileFingerprint>,
    },

    /// The filesystem Adapter could not prove a source revision strongly
    /// enough to authorize media interpretation or execution reuse.
    #[error("媒体源缺少完整的文件修订证据: {path} — {reason}")]
    MediaSourceRevisionUnavailable {
        /// Physical source path whose revision could not be proven.
        path: String,
        /// Platform or filesystem evidence that was unavailable.
        reason: String,
    },

    #[error(
        "解码超时 (asset={asset_id}, access_mode={access_mode}, budget_ms={budget_ms}, frame={frame}, secs={secs:.3})"
    )]
    DecodeTimeout {
        asset_id: String,
        access_mode: String,
        budget_ms: u64,
        frame: u64,
        secs: f64,
    },

    #[error(
        "解码前向扫描预算耗尽 (asset={asset_id}, access_mode={access_mode}, decoded_frames={decoded_frames}, budget_frames={budget_frames}, target_pts={target_pts})"
    )]
    DecodeBudgetExhausted {
        asset_id: String,
        access_mode: String,
        decoded_frames: u64,
        budget_frames: u64,
        target_pts: i64,
    },

    /// A successful decoder payload did not prove that its half-open
    /// presentation interval contains the requested stream timestamp.
    #[error(
        "解码帧时间区间不匹配 (asset={asset_id}, access_mode={access_mode}, requested_pts={requested_pts:?}, selected_pts={selected_pts:?}, selected_duration_pts={selected_duration_pts:?})"
    )]
    DecodeTemporalMismatch {
        /// Physical media identity used for diagnostics.
        asset_id: String,
        /// Preview access-mode contract that rejected the payload.
        access_mode: String,
        /// Requested absolute stream timestamp, when the Adapter supplied it.
        requested_pts: Option<i64>,
        /// Selected decoded-frame start timestamp, when proven.
        selected_pts: Option<i64>,
        /// Positive duration of the selected presentation interval, when proven.
        selected_duration_pts: Option<i64>,
    },

    #[error("不支持的媒体格式: {format}")]
    UnsupportedFormat { format: String },

    #[error("代理文件生成失败: {reason}")]
    ProxyGenerationFailed { reason: String },

    // ── 时间线 ────────────────────────────────────────────────────────────────
    #[error("轨道不存在: {track_id}")]
    TrackNotFound { track_id: String },

    #[error("片段不存在: {clip_id}")]
    ClipNotFound { clip_id: String },

    #[error("时间码超出范围: {timecode}")]
    TimecodeOutOfRange { timecode: String },

    #[error("invalid exact timeline time: {0}")]
    InvalidTimelineTime(#[from] crate::timeline_time::TimelineTimeError),

    #[error("轨道锁定，无法修改: {track_id}")]
    TrackLocked { track_id: String },

    // ── 应用操作 ──────────────────────────────────────────────────────────────
    #[error("操作未执行 (action={action}): {reason}")]
    ActionNotExecuted { action: String, reason: String },

    // ── 渲染器 ────────────────────────────────────────────────────────────────
    #[error("GPU 设备初始化失败: {reason}")]
    GpuInitFailed { reason: String },

    #[error("Shader 编译失败: {shader_name} — {reason}")]
    ShaderCompileFailed { shader_name: String, reason: String },

    #[error("纹理上传失败: {reason}")]
    TextureUploadFailed { reason: String },

    #[error("效果图求值失败: {reason}")]
    EffectGraphEvaluationFailed { reason: String },

    // ── 素材库 ────────────────────────────────────────────────────────────────
    #[error("素材不存在: {asset_id}")]
    AssetNotFound { asset_id: String },

    #[error("素材文件已移动或删除: {path}")]
    AssetFileMissing { path: String },

    #[error("素材库数据库错误: {reason}")]
    AssetDbError { reason: String },

    // ── AI 工作流 ─────────────────────────────────────────────────────────────
    #[error("AI Provider '{provider}' 调用失败: {reason}")]
    AiProviderFailed { provider: String, reason: String },

    #[error("AI API 限流，请稍后重试 (provider={provider})")]
    AiRateLimited { provider: String },

    #[error("工作流步骤失败 (step={step_id}): {reason}")]
    WorkflowStepFailed { step_id: String, reason: String },

    #[error("工作流 YAML 解析失败: {reason}")]
    WorkflowParseFailed { reason: String },

    // ── 导出 ──────────────────────────────────────────────────────────────────
    #[error("导出编码失败: {reason}")]
    ExportFailed { reason: String },

    #[error("硬件编码器不可用: {encoder}")]
    HwEncoderUnavailable { encoder: String },

    // ── IO / 通用 ─────────────────────────────────────────────────────────────
    #[error("文件 IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("操作已取消")]
    Cancelled,

    #[error("未知错误: {0}")]
    Other(#[from] anyhow::Error),
}

/// 全局 Result 别名
pub type Result<T, E = MondrianError> = std::result::Result<T, E>;
