//! 硬件加速编码器检测

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    NvencH264,
    NvencH265,
    AmfH264,
    AmfH265,
    QsvH264,
    QsvH265,
    VideoToolboxH264,
    VideoToolboxH265,
    VideoToolboxProRes,
    SoftwareX264,
    SoftwareX265,
}

impl EncoderBackend {
    /// 自动检测最优可用编码器
    pub fn detect_best() -> Self {
        #[cfg(target_os = "macos")]
        return Self::VideoToolboxH264;

        #[cfg(not(target_os = "macos"))]
        Self::SoftwareX264
    }

    pub fn is_hardware(&self) -> bool {
        !matches!(self, Self::SoftwareX264 | Self::SoftwareX265)
    }

    pub fn ffmpeg_codec_name(&self) -> &'static str {
        match self {
            Self::NvencH264 => "h264_nvenc",
            Self::NvencH265 => "hevc_nvenc",
            Self::AmfH264 => "h264_amf",
            Self::AmfH265 => "hevc_amf",
            Self::QsvH264 => "h264_qsv",
            Self::QsvH265 => "hevc_qsv",
            Self::VideoToolboxH264 => "h264_videotoolbox",
            Self::VideoToolboxH265 => "hevc_videotoolbox",
            Self::VideoToolboxProRes => "prores_videotoolbox",
            Self::SoftwareX264 => "libx264",
            Self::SoftwareX265 => "libx265",
        }
    }
}
