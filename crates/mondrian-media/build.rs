//! Build-time FFmpeg feature detection.
//!
//! `ffmpeg-sys-next` generates bindings from the headers installed on the
//! build machine, so enum constants introduced after the oldest supported
//! FFmpeg (6.1) are absent there. Probe the concrete libavcodec version once
//! and expose `mondrian_ffmpeg_7_1` for conditional references. An
//! undetectable version fails closed to the oldest supported surface.

use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(mondrian_ffmpeg_7_0)");
    println!("cargo:rustc-check-cfg=cfg(mondrian_ffmpeg_7_1)");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=FFMPEG_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKGRS_TRIPLET");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    match detect_libavcodec_version() {
        Some((major, minor)) => {
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
