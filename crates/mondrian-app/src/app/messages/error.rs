//! Product projection of typed failures; diagnostic Display output is never parsed.

use mondrian_core::MondrianError;

use super::AppMessage;

impl AppMessage {
    /// Retain a typed core failure as language-neutral product message data.
    /// Native error details remain opaque arguments; formatting happens in the UI.
    pub fn from_error(error: &MondrianError) -> Self {
        match error {
            MondrianError::MediaOpen { path, reason } => Self::new("error-media-open")
                .with_text("path", path.clone())
                .with_text("reason", reason.clone()),
            MondrianError::DecodeFailed { asset_id, reason } => Self::new("error-decode-failed")
                .with_text("asset_id", asset_id.clone())
                .with_text("reason", reason.clone()),
            MondrianError::UnsupportedFormat { format } => {
                Self::new("error-unsupported-format").with_text("format", format.clone())
            }
            MondrianError::ProxyGenerationFailed { reason } => {
                Self::new("error-proxy-generation").with_text("reason", reason.clone())
            }
            MondrianError::TrackNotFound { track_id } => {
                Self::new("error-track-not-found").with_text("track_id", track_id.clone())
            }
            MondrianError::ClipNotFound { clip_id } => {
                Self::new("error-clip-not-found").with_text("clip_id", clip_id.clone())
            }
            MondrianError::TimecodeOutOfRange { timecode } => {
                Self::new("error-time-out-of-range").with_text("timecode", timecode.clone())
            }
            MondrianError::TrackLocked { track_id } => {
                Self::new("error-track-locked").with_text("track_id", track_id.clone())
            }
            MondrianError::ActionNotExecuted { action, reason } => {
                Self::new("error-action-not-executed")
                    .with_text("action", action.clone())
                    .with_text("reason", reason.clone())
            }
            MondrianError::GpuInitFailed { reason } => {
                Self::new("error-gpu-init").with_text("reason", reason.clone())
            }
            MondrianError::ShaderCompileFailed { shader_name, reason } => {
                Self::new("error-shader-compile")
                    .with_text("shader_name", shader_name.clone())
                    .with_text("reason", reason.clone())
            }
            MondrianError::TextureUploadFailed { reason } => {
                Self::new("error-texture-upload").with_text("reason", reason.clone())
            }
            MondrianError::EffectGraphEvaluationFailed { reason } => {
                Self::new("error-effect-evaluation").with_text("reason", reason.clone())
            }
            MondrianError::AssetNotFound { asset_id } => {
                Self::new("error-asset-not-found").with_text("asset_id", asset_id.clone())
            }
            MondrianError::AssetFileMissing { path } => {
                Self::new("error-asset-file-missing").with_text("path", path.clone())
            }
            MondrianError::AssetDbError { reason } => {
                Self::new("error-asset-database").with_text("reason", reason.clone())
            }
            MondrianError::AiProviderFailed { provider, reason } => Self::new("error-ai-provider")
                .with_text("provider", provider.clone())
                .with_text("reason", reason.clone()),
            MondrianError::AiRateLimited { provider } => {
                Self::new("error-ai-rate-limit").with_text("provider", provider.clone())
            }
            MondrianError::WorkflowStepFailed { step_id, reason } => {
                Self::new("error-workflow-step")
                    .with_text("step_id", step_id.clone())
                    .with_text("reason", reason.clone())
            }
            MondrianError::WorkflowParseFailed { reason } => {
                Self::new("error-workflow-parse").with_text("reason", reason.clone())
            }
            MondrianError::ExportFailed { reason } => {
                Self::new("error-export").with_text("reason", reason.clone())
            }
            MondrianError::HwEncoderUnavailable { encoder } => {
                Self::new("error-hardware-encoder").with_text("encoder", encoder.clone())
            }
            MondrianError::MediaSourceRevisionUnavailable { path, reason } => {
                Self::new("error-source-revision-unavailable")
                    .with_text("path", path.clone())
                    .with_text("reason", reason.clone())
            }
            MondrianError::MediaSourceRevisionChanged { path, expected, actual } => {
                Self::new("error-source-revision-changed")
                    .with_text("path", path.clone())
                    .with_text("expected", format!("{expected:?}"))
                    .with_text("actual", format!("{actual:?}"))
            }
            MondrianError::MediaExecutionResourceUnavailable { operation, source, cleanup } => {
                let message = Self::new(if cleanup.is_some() {
                    "error-media-resource-cleanup"
                } else {
                    "error-media-resource"
                })
                .with_text("operation", *operation)
                .with_text("source", source.to_string());
                match cleanup {
                    Some(cleanup) => message.with_text("cleanup", cleanup.clone()),
                    None => message,
                }
            }
            MondrianError::DecodeTimeout { asset_id, access_mode, budget_ms, frame, secs } => {
                Self::new("error-decode-timeout")
                    .with_text("asset_id", asset_id.clone())
                    .with_text("access_mode", access_mode.clone())
                    .with_text("budget_ms", budget_ms.to_string())
                    .with_text("frame", frame.to_string())
                    .with_text("secs", format!("{secs:.3}"))
            }
            MondrianError::DecodeBudgetExhausted {
                asset_id,
                access_mode,
                decoded_frames,
                budget_frames,
                target_pts,
            } => Self::new("error-decode-budget")
                .with_text("asset_id", asset_id.clone())
                .with_text("access_mode", access_mode.clone())
                .with_text("decoded_frames", decoded_frames.to_string())
                .with_text("budget_frames", budget_frames.to_string())
                .with_text("target_pts", target_pts.to_string()),
            MondrianError::DecodeTemporalMismatch {
                asset_id,
                access_mode,
                requested_pts,
                selected_pts,
                selected_duration_pts,
            } => Self::new("error-decode-temporal")
                .with_text("asset_id", asset_id.clone())
                .with_text("access_mode", access_mode.clone())
                .with_message("requested_pts", optional_timestamp(*requested_pts))
                .with_message("selected_pts", optional_timestamp(*selected_pts))
                .with_message(
                    "selected_duration_pts",
                    optional_timestamp(*selected_duration_pts),
                ),
            MondrianError::InvalidTimelineTime(source) => {
                Self::new("error-timeline-time").with_message("reason", timeline_time_error(source))
            }
            MondrianError::Io(source) => {
                Self::new("error-io").with_text("reason", source.to_string())
            }
            MondrianError::Serialization(source) => {
                Self::new("error-serialization").with_text("reason", source.to_string())
            }
            MondrianError::Cancelled => Self::new("error-canceled"),
            MondrianError::Other(source) => Self::from_anyhow(source),
        }
    }

    /// Project a contextual error chain, retaining typed core failures and context.
    pub fn from_anyhow(error: &anyhow::Error) -> Self {
        if let Some(failure) = error.chain().find_map(|source| {
            source
                .downcast_ref::<crate::app::curve_clipboard::CurveClipboardError>()
                .or_else(|| match source.downcast_ref::<MondrianError>() {
                    Some(MondrianError::Other(inner)) => {
                        inner.downcast_ref::<crate::app::curve_clipboard::CurveClipboardError>()
                    }
                    _ => None,
                })
        }) {
            return Self::new(failure.message_id());
        }
        let chain = error.chain().collect::<Vec<_>>();
        let typed = chain.iter().enumerate().find_map(|(index, source)| {
            source.downcast_ref::<MondrianError>().map(|source| (index, source))
        });
        let Some((index, source)) = typed else {
            return Self::new("error-other").with_text("reason", format!("{error:#}"));
        };
        let mut message = Self::from_error(source);
        for context in chain[..index].iter().rev() {
            message = Self::new("error-context")
                .with_text("context", context.to_string())
                .with_message("source", message);
        }
        message
    }
}

fn optional_timestamp(value: Option<i64>) -> AppMessage {
    match value {
        Some(value) => {
            AppMessage::new("status-opaque-detail").with_text("detail", value.to_string())
        }
        None => AppMessage::new("error-timestamp-unknown"),
    }
}

fn timeline_time_error(error: &mondrian_core::timeline_time::TimelineTimeError) -> AppMessage {
    use mondrian_core::timeline_time::TimelineTimeError;
    match error {
        TimelineTimeError::ZeroDenominator => AppMessage::new("reason-time-zero-denominator"),
        TimelineTimeError::Overflow => AppMessage::new("reason-time-overflow"),
        TimelineTimeError::InvalidLegacyTimeBase { numerator, denominator } => {
            AppMessage::new("reason-time-invalid-base")
                .with_text("numerator", numerator.to_string())
                .with_text("denominator", denominator.to_string())
        }
        TimelineTimeError::InvalidFrameRate { numerator, denominator } => {
            AppMessage::new("reason-time-invalid-rate")
                .with_text("numerator", numerator.to_string())
                .with_text("denominator", denominator.to_string())
        }
        TimelineTimeError::DomainMismatch => AppMessage::new("reason-time-domain-mismatch"),
        TimelineTimeError::NonInvertibleTransform => AppMessage::new("reason-time-non-invertible"),
        TimelineTimeError::NegativeDuration => AppMessage::new("reason-time-negative-duration"),
        TimelineTimeError::NonFiniteInput => AppMessage::new("reason-time-non-finite"),
    }
}
