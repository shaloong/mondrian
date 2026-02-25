//! 代理文件生成器（Proxy）
//!
//! 后台将高码率原始素材转码为低码率代理文件，用于编辑时的流畅预览。
//! 导出时自动切换回原始文件。

use mondrian_core::{types::AssetId, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::sync::mpsc;

/// 代理分辨率预设
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyResolution {
    P360,
    P480,
    P720,
    P1080,
}

impl ProxyResolution {
    pub fn height(self) -> u32 {
        match self {
            Self::P360 => 360,
            Self::P480 => 480,
            Self::P720 => 720,
            Self::P1080 => 1080,
        }
    }
}

/// 代理编码格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyCodec {
    H264,  // 低码率，兼容性最好
    DnxHd, // 编辑友好，高质量
}

/// 代理生成配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyConfig {
    pub resolution: ProxyResolution,
    pub codec: ProxyCodec,
    /// CRF 质量（H.264: 0-51，越小质量越高）
    pub crf: u8,
    /// 并行转码任务数
    pub concurrent_jobs: u8,
    /// 代理文件存储根目录（默认 ~/.mondrian/proxy）
    pub cache_dir: PathBuf,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            resolution: ProxyResolution::P720,
            codec: ProxyCodec::H264,
            crf: 23,
            concurrent_jobs: 2,
            cache_dir: default_proxy_dir(),
        }
    }
}

fn default_proxy_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    home.join(".mondrian").join("proxy")
}

/// 代理生成任务进度
#[derive(Debug, Clone)]
pub struct ProxyProgress {
    pub asset_id: AssetId,
    pub progress: f32, // 0.0 ~ 1.0
    pub is_done: bool,
    pub error: Option<String>,
}

/// 代理文件生成器
pub struct ProxyGenerator {
    config: ProxyConfig,
}

impl ProxyGenerator {
    pub fn new(config: ProxyConfig) -> Self {
        Self { config }
    }

    fn output_extension(&self) -> &'static str {
        match self.config.codec {
            ProxyCodec::H264 => "mp4",
            ProxyCodec::DnxHd => "mov",
        }
    }

    /// 计算指定素材的代理文件路径
    pub fn proxy_path(&self, source_path: &Path) -> PathBuf {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source_path.hash(&mut hasher);
        let hash = hasher.finish();
        let height = self.config.resolution.height();
        let codec = match self.config.codec {
            ProxyCodec::H264 => "h264",
            ProxyCodec::DnxHd => "dnxhd",
        };
        self.config.cache_dir.join(format!(
            "{hash:016x}_{height}p_{codec}.{}",
            self.output_extension()
        ))
    }

    /// 检查代理文件是否已存在且有效
    pub fn proxy_exists(&self, source_path: &Path) -> bool {
        self.proxy_path(source_path).exists()
    }

    /// 异步生成代理文件（后台 FFmpeg 转码）
    ///
    /// 通过 `mpsc::Sender` 实时回报进度。
    pub async fn generate(
        &self,
        asset_id: AssetId,
        source_path: PathBuf,
        progress_tx: mpsc::Sender<ProxyProgress>,
    ) -> Result<PathBuf> {
        if !source_path.exists() {
            return Err(mondrian_core::MondrianError::MediaOpen {
                path: source_path.display().to_string(),
                reason: "source file not found".to_string(),
            });
        }

        let output_path = self.proxy_path(&source_path);
        let tmp_output_path = output_path.with_extension(format!("{}.part", self.output_extension()));

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if output_path.exists() {
            let _ = progress_tx
                .send(ProxyProgress {
                    asset_id,
                    progress: 1.0,
                    is_done: true,
                    error: None,
                })
                .await;
            return Ok(output_path);
        }

        if tmp_output_path.exists() {
            let _ = std::fs::remove_file(&tmp_output_path);
        }

        tracing::info!(
            "Generating proxy for asset {asset_id}: {:?} → {:?}",
            source_path,
            output_path
        );

        let _ = progress_tx
            .send(ProxyProgress {
                asset_id,
                progress: 0.05,
                is_done: false,
                error: None,
            })
            .await;

        let height = self.config.resolution.height();
        let crf = self.config.crf.min(51);
        let source_for_cmd = source_path.clone();
        let output_for_cmd = tmp_output_path.clone();
        let codec = self.config.codec;

        let transcode_result = tokio::task::spawn_blocking(move || {
            run_ffmpeg_proxy_transcode(codec, crf, height, &source_for_cmd, &output_for_cmd)
        })
        .await
        .map_err(|e| mondrian_core::MondrianError::ProxyGenerationFailed {
            reason: format!("proxy task join failed: {e}"),
        })?;

        if let Err(err) = transcode_result {
            let _ = std::fs::remove_file(&output_path);
            let _ = std::fs::remove_file(&tmp_output_path);
            let _ = progress_tx
                .send(ProxyProgress {
                    asset_id,
                    progress: 1.0,
                    is_done: true,
                    error: Some(err.to_string()),
                })
                .await;
            return Err(err);
        }

        std::fs::rename(&tmp_output_path, &output_path).map_err(|e| {
            mondrian_core::MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "proxy finalize rename failed ({} -> {}): {}",
                    tmp_output_path.display(),
                    output_path.display(),
                    e
                ),
            }
        })?;

        let _ = progress_tx
            .send(ProxyProgress {
                asset_id,
                progress: 1.0,
                is_done: true,
                error: None,
            })
            .await;

        Ok(output_path)
    }
}

fn run_ffmpeg_proxy_transcode(
    codec: ProxyCodec,
    crf: u8,
    height: u32,
    source_path: &Path,
    output_path: &Path,
) -> Result<()> {
    let scale_arg = format!("scale=-2:{height}");
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(source_path)
        .arg("-vf")
        .arg(scale_arg)
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("128k");

    match codec {
        ProxyCodec::H264 => {
            cmd.arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("veryfast")
                .arg("-crf")
                .arg(crf.to_string())
                .arg("-movflags")
                .arg("+faststart");
        }
        ProxyCodec::DnxHd => {
            cmd.arg("-c:v").arg("dnxhd").arg("-b:v").arg("90M").arg("-f").arg("mov");
        }
    }

    cmd.arg(output_path);

    let output = cmd.output().map_err(|e| mondrian_core::MondrianError::ProxyGenerationFailed {
        reason: format!("failed to invoke ffmpeg (is ffmpeg in PATH?): {}", e),
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(mondrian_core::MondrianError::ProxyGenerationFailed {
            reason: format!("ffmpeg failed: {}", stderr.trim()),
        });
    }

    if !output_path.exists() {
        return Err(mondrian_core::MondrianError::ProxyGenerationFailed {
            reason: "ffmpeg exited successfully but output file is missing".to_string(),
        });
    }

    Ok(())
}
