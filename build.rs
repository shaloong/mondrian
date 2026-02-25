/// Mondrian 工作区根 build.rs
/// 用途：
///   1. 检测系统 FFmpeg 安装，若未找到则打印友好错误
///   2. 将 Cargo.toml 版本号写入环境变量，供运行时 env!() 读取
///   3. 链接系统 wgpu 所需的原生库（仅 Linux 需显式处理）
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    detect_ffmpeg();
    emit_platform_links();
}

/// 使用 pkg-config 检测 FFmpeg libavcodec
fn detect_ffmpeg() {
    let output = Command::new("pkg-config").args(["--modversion", "libavcodec"]).output();

    match output {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            println!("cargo:warning=检测到 FFmpeg libavcodec: {ver}");
        }
        _ => {
            // Windows 下通过 vcpkg 或手动设置 FFMPEG_DIR 环境变量
            if let Ok(ffmpeg_dir) = std::env::var("FFMPEG_DIR") {
                println!("cargo:warning=使用 FFMPEG_DIR={ffmpeg_dir}");
                println!("cargo:rustc-link-search=native={ffmpeg_dir}/lib");
            } else {
                println!(
                    "cargo:warning=\n\
                     ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
                     ⚠  未检测到 FFmpeg！媒体解码功能将无法编译。\n\
                     \n\
                     安装方法：\n\
                     • macOS:   brew install ffmpeg\n\
                     • Ubuntu:  sudo apt install libavcodec-dev libavformat-dev \\\n\
                                  libavfilter-dev libavdevice-dev libswscale-dev\n\
                     • Windows: 设置 FFMPEG_DIR 环境变量指向 FFmpeg 安装目录，\n\
                                或运行 scripts/setup_ffmpeg.ps1\n\
                     ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
                );
            }
        }
    }
}

/// 平台特定链接标志
fn emit_platform_links() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" => {
            // wgpu Vulkan 后端需要
            println!("cargo:rustc-link-lib=dylib=vulkan");
        }
        "macos" => {
            println!("cargo:rustc-link-lib=framework=Metal");
            println!("cargo:rustc-link-lib=framework=QuartzCore");
        }
        "windows" => {
            // DX12 由系统自带，无需显式链接
        }
        _ => {}
    }
}
