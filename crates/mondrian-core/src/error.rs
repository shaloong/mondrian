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

    #[error("轨道锁定，无法修改: {track_id}")]
    TrackLocked { track_id: String },

    // ── 渲染器 ────────────────────────────────────────────────────────────────
    #[error("GPU 设备初始化失败: {reason}")]
    GpuInitFailed { reason: String },

    #[error("Shader 编译失败: {shader_name} — {reason}")]
    ShaderCompileFailed { shader_name: String, reason: String },

    #[error("纹理上传失败: {reason}")]
    TextureUploadFailed { reason: String },

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
