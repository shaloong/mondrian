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

/// Freshness state for a project's expected proxy media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyStatus {
    /// No proxy file exists at the configured proxy path.
    Missing,
    /// The proxy file exists and is at least as new as the source media.
    Fresh,
    /// The proxy file exists but should not be used for playback.
    Stale,
}

impl ProxyStatus {
    /// Resolves freshness from an explicit source/proxy path pair.
    pub fn from_paths(source_path: &Path, proxy_path: &Path) -> Self {
        if !proxy_path.exists() {
            return Self::Missing;
        }

        let source_modified =
            std::fs::metadata(source_path).and_then(|metadata| metadata.modified());
        let proxy_modified = std::fs::metadata(proxy_path).and_then(|metadata| metadata.modified());
        match (source_modified, proxy_modified) {
            (Ok(source), Ok(proxy)) if proxy >= source => Self::Fresh,
            (Ok(_), Ok(_)) => Self::Stale,
            (Err(_), Ok(_)) => Self::Fresh,
            _ => Self::Stale,
        }
    }

    /// Returns true when preview playback may decode the proxy file.
    pub fn is_fresh(self) -> bool {
        self == Self::Fresh
    }
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

    /// 检查代理文件是否存在。
    ///
    /// This intentionally reports existence only. Use [`Self::proxy_status`]
    /// or [`Self::proxy_is_fresh`] for preview/playback scheduling.
    pub fn proxy_exists(&self, source_path: &Path) -> bool {
        self.proxy_path(source_path).exists()
    }

    /// Returns the freshness state for the configured proxy of `source_path`.
    pub fn proxy_status(&self, source_path: &Path) -> ProxyStatus {
        let proxy_path = self.proxy_path(source_path);
        ProxyStatus::from_paths(source_path, &proxy_path)
    }

    /// Returns true when the configured proxy exists and is safe to decode.
    pub fn proxy_is_fresh(&self, source_path: &Path) -> bool {
        self.proxy_status(source_path).is_fresh()
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
        let tmp_output_path =
            output_path.with_extension(format!("{}.part", self.output_extension()));

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if self.proxy_status(&source_path) == ProxyStatus::Fresh {
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

        finalize_proxy_output(&tmp_output_path, &output_path)?;

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

fn finalize_proxy_output(tmp_output_path: &Path, output_path: &Path) -> Result<()> {
    if !output_path.exists() {
        return std::fs::rename(tmp_output_path, output_path).map_err(|e| {
            mondrian_core::MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "proxy finalize rename failed ({} -> {}): {}",
                    tmp_output_path.display(),
                    output_path.display(),
                    e
                ),
            }
        });
    }

    let backup_path = output_path.with_extension(format!(
        "{}.replace-backup",
        output_path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("proxy")
    ));
    if backup_path.exists() {
        std::fs::remove_file(&backup_path).map_err(|e| {
            mondrian_core::MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "proxy finalize remove old backup failed ({}): {}",
                    backup_path.display(),
                    e
                ),
            }
        })?;
    }

    std::fs::rename(output_path, &backup_path).map_err(|e| {
        mondrian_core::MondrianError::ProxyGenerationFailed {
            reason: format!(
                "proxy finalize backup existing output failed ({} -> {}): {}",
                output_path.display(),
                backup_path.display(),
                e
            ),
        }
    })?;

    match std::fs::rename(tmp_output_path, output_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup_path);
            Ok(())
        }
        Err(rename_err) => {
            let restore_result = std::fs::rename(&backup_path, output_path);
            let restore_message = match restore_result {
                Ok(()) => "previous proxy restored".to_string(),
                Err(restore_err) => format!("previous proxy restore failed: {restore_err}"),
            };
            Err(mondrian_core::MondrianError::ProxyGenerationFailed {
                reason: format!(
                    "proxy finalize rename failed ({} -> {}): {}; {}",
                    tmp_output_path.display(),
                    output_path.display(),
                    rename_err,
                    restore_message
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{finalize_proxy_output, ProxyConfig, ProxyGenerator, ProxyStatus};
    use std::time::Duration;

    fn test_proxy_config(cache_dir: std::path::PathBuf) -> ProxyConfig {
        ProxyConfig { cache_dir, ..ProxyConfig::default() }
    }

    #[test]
    fn proxy_status_reports_missing_when_proxy_file_does_not_exist() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mp4");
        std::fs::write(&source, b"source").expect("source");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));

        assert_eq!(generator.proxy_status(&source), ProxyStatus::Missing);
        assert!(!generator.proxy_is_fresh(&source));
    }

    #[test]
    fn proxy_status_reports_fresh_when_proxy_is_newer_than_source() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mp4");
        std::fs::write(&source, b"source").expect("source");
        std::thread::sleep(Duration::from_millis(20));

        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let proxy = generator.proxy_path(&source);
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");

        assert_eq!(generator.proxy_status(&source), ProxyStatus::Fresh);
        assert!(generator.proxy_is_fresh(&source));
    }

    #[test]
    fn proxy_status_reports_stale_when_source_is_newer_than_proxy() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.mp4");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let proxy = generator.proxy_path(&source);
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&source, b"source").expect("source");

        assert_eq!(generator.proxy_status(&source), ProxyStatus::Stale);
        assert!(!generator.proxy_is_fresh(&source));
    }

    #[test]
    fn finalize_proxy_output_replaces_existing_proxy_after_tmp_is_ready() {
        let root = tempfile::tempdir().expect("tempdir");
        let output = root.path().join("proxy.mp4");
        let tmp = root.path().join("proxy.mp4.part");
        std::fs::write(&output, b"old proxy").expect("old proxy");
        std::fs::write(&tmp, b"new proxy").expect("new proxy");

        finalize_proxy_output(&tmp, &output).expect("finalize");

        assert_eq!(std::fs::read(&output).expect("output"), b"new proxy");
        assert!(!tmp.exists());
        assert!(!root.path().join("proxy.mp4.replace-backup").exists());
    }

    #[test]
    fn finalize_proxy_output_restores_existing_proxy_when_new_file_is_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let output = root.path().join("proxy.mp4");
        let tmp = root.path().join("missing-proxy.mp4.part");
        std::fs::write(&output, b"old proxy").expect("old proxy");

        let err = finalize_proxy_output(&tmp, &output).expect_err("finalize should fail");

        assert!(err.to_string().contains("previous proxy restored"));
        assert_eq!(std::fs::read(&output).expect("output"), b"old proxy");
        assert!(!root.path().join("proxy.mp4.replace-backup").exists());
    }
}
