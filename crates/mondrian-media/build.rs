//! Build-time FFmpeg feature detection.
//!
//! `ffmpeg-sys-next` generates bindings from the headers installed on the
//! build machine, so enum constants introduced after the oldest supported
//! FFmpeg (6.1) are absent there. Probe the concrete libavcodec version once
//! and expose `mondrian_ffmpeg_*` for conditional references. An
//! undetectable version fails closed to the oldest supported surface.

use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_videotoolbox();
    }
    println!("cargo:rustc-check-cfg=cfg(mondrian_ffmpeg_7_0)");
    println!("cargo:rustc-check-cfg=cfg(mondrian_ffmpeg_7_1)");
    println!("cargo:rustc-check-cfg=cfg(mondrian_ffmpeg_8_0)");
    println!("cargo:rustc-check-cfg=cfg(mondrian_ffmpeg_d3d12va_frame_v2)");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=FFMPEG_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKGRS_TRIPLET");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    if has_d3d12va_frame_v2() {
        println!("cargo:rustc-cfg=mondrian_ffmpeg_d3d12va_frame_v2");
    }

    match detect_libavcodec_version() {
        Some((major, minor)) => {
            // FFmpeg 8 removes deprecated AVFrame scan-field mirrors.
            if major >= 62 {
                println!("cargo:rustc-cfg=mondrian_ffmpeg_8_0");
            }
            // FFmpeg 7.0 ships libavcodec 61.3; 7.1 ships 61.19.
            if major > 61 || (major == 61 && minor >= 3) {
                println!("cargo:rustc-cfg=mondrian_ffmpeg_7_0");
            }
            if major > 61 || (major == 61 && minor >= 19) {
                println!("cargo:rustc-cfg=mondrian_ffmpeg_7_1");
            }
        }
        None => {
            println!(
                "cargo:warning=libavcodec version undetectable; assuming the oldest supported FFmpeg surface (6.1). Set FFMPEG_DIR, FFMPEG_INCLUDE_DIR, VCPKG_ROOT, or PKG_CONFIG_PATH so the media build can expose the full header surface."
            );
        }
    }
}

fn has_d3d12va_frame_v2() -> bool {
    candidate_include_dirs().into_iter().any(|include_dir| {
        let header = include_dir.join("libavutil").join("hwcontext_d3d12va.h");
        println!("cargo:rerun-if-changed={}", header.display());
        std::fs::read_to_string(header).is_ok_and(|text| {
            text.contains("int subresource_index;") && text.contains("AVD3D12VAFrameFlags flags;")
        })
    })
}

fn detect_libavcodec_version() -> Option<(u32, u32)> {
    if let Some(version) = pkg_config_version() {
        return Some(version);
    }
    for include_dir in candidate_include_dirs() {
        if let Some(version) = parse_version_header(&include_dir) {
            return Some(version);
        }
    }
    None
}

fn pkg_config_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("pkg-config")
        .args(["--modversion", "libavcodec"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn candidate_include_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("FFMPEG_INCLUDE_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("FFMPEG_DIR") {
        dirs.push(PathBuf::from(dir).join("include"));
    }
    if let Some(root) = std::env::var_os("VCPKG_ROOT") {
        let triplet = std::env::var("VCPKGRS_TRIPLET").unwrap_or_else(|_| "x64-windows".to_owned());
        dirs.push(PathBuf::from(root).join("installed").join(triplet).join("include"));
    }
    dirs
}

fn parse_version_header(include_dir: &std::path::Path) -> Option<(u32, u32)> {
    let avcodec_dir = include_dir.join("libavcodec");
    let text = std::fs::read_to_string(avcodec_dir.join("version.h")).ok()?;
    let minor = parse_define(&text, "LIBAVCODEC_VERSION_MINOR")?;
    let major = parse_define(&text, "LIBAVCODEC_VERSION_MAJOR").or_else(|| {
        let major_text = std::fs::read_to_string(avcodec_dir.join("version_major.h")).ok()?;
        parse_define(&major_text, "LIBAVCODEC_VERSION_MAJOR")
    })?;
    Some((major, minor))
}

fn parse_define(text: &str, name: &str) -> Option<u32> {
    let line = text.lines().find(|line| line.starts_with(&format!("#define {name}")))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

/// Compile the small public-API codec adapter with the selected macOS SDK.
fn build_videotoolbox() {
    use std::process::Command;

    println!("cargo:rerun-if-changed=native/videotoolbox.c");
    println!("cargo:rerun-if-changed=native/videotoolbox.h");
    println!("cargo:rerun-if-changed=native/videotoolbox_encode.c");
    println!("cargo:rerun-if-changed=native/videotoolbox_encode.h");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    let target = std::env::var("TARGET").expect("Cargo target is required");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output is required"));
    let archive = output.join("libmondrian_videotoolbox.a");
    let mut objects = Vec::new();
    for name in ["videotoolbox", "videotoolbox_encode"] {
        let object = output.join(format!("{name}.o"));
        let mut compiler = Command::new("xcrun");
        compiler
            .args([
                "--sdk", "macosx", "clang", "-target", &target, "-std=c11", "-O2", "-fPIC",
                "-Wall", "-Wextra", "-Werror", "-c",
            ])
            .arg(format!("native/{name}.c"))
            .arg("-o")
            .arg(&object);
        if let Ok(version) = std::env::var("MACOSX_DEPLOYMENT_TARGET") {
            compiler.arg(format!("-mmacosx-version-min={version}"));
        }
        assert!(
            compiler.status().expect("execute macOS clang").success(),
            "compile {name} adapter"
        );
        objects.push(object);
    }
    assert!(
        Command::new("xcrun")
            .args(["--sdk", "macosx", "ar", "rcs"])
            .arg(archive)
            .args(objects)
            .status()
            .expect("execute macOS archiver")
            .success(),
        "archive VideoToolbox adapter"
    );
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=mondrian_videotoolbox");
    for framework in ["CoreFoundation", "CoreMedia", "CoreVideo", "VideoToolbox"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
