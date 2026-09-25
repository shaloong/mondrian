fn main() {
    println!("cargo:rerun-if-changed=assets/favicon.ico");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKG_DEFAULT_TRIPLET");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
    emit_cargo_build_attestation();

    #[cfg(target_os = "linux")]
    emit_linux_link_resource_policy();

    #[cfg(target_os = "windows")]
    {
        build_openfx_host();
        if let Err(err) = embed_windows_icon() {
            println!("cargo:warning=写入 Windows 图标资源失败: {err}");
        }
        if let Err(err) = deploy_windows_runtime_dlls() {
            println!("cargo:warning=部署 Windows 媒体运行时失败: {err}");
        }
    }
}

#[cfg(target_os = "windows")]
fn build_openfx_host() {
    let vendor = std::path::Path::new("native/openfx/vendor");
    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").define("NOMINMAX", None);
    build.include(vendor.join("include"));
    build.include(vendor.join("host/include"));
    for name in [
        "ofxhBinary",
        "ofxhClip",
        "ofxhHost",
        "ofxhImageEffect",
        "ofxhImageEffectAPI",
        "ofxhInteract",
        "ofxhMemory",
        "ofxhParam",
        "ofxhPluginAPICache",
        "ofxhPluginCache",
        "ofxhPropertySuite",
        "ofxhUtilities",
    ] {
        build.file(vendor.join("host/src").join(format!("{name}.cpp")));
    }
    for name in [
        "render",
        "image_clip",
        "effect_instance",
        "host_descriptor",
        "parameter_instance",
    ] {
        build.file(format!("native/openfx/host/{name}.cpp"));
    }
    println!("cargo:rerun-if-changed=native/openfx");
    build.compile("mondrian_openfx_host");
}

#[cfg(target_os = "linux")]
fn emit_linux_link_resource_policy() {
    // Rust 1.97's Linux GNU target uses bundled lld, whose default is every
    // available hardware thread. App tests, binaries, and examples link the
    // complete product and validation harness, so bound lld itself in addition
    // to Cargo's job count. This changes neither code generation nor runtime
    // behavior.
    println!("cargo:rustc-link-arg-tests=-Wl,--threads=1");
    println!("cargo:rustc-link-arg-bins=-Wl,--threads=1");
    println!("cargo:rustc-link-arg-examples=-Wl,--threads=1");
}

fn emit_cargo_build_attestation() {
    if let Ok(profile) = std::env::var("PROFILE") {
        println!("cargo:rustc-env=MONDRIAN_BUILD_CARGO_PROFILE={profile}");
    }
    if let Ok(opt_level) = std::env::var("OPT_LEVEL") {
        println!("cargo:rustc-env=MONDRIAN_BUILD_RUSTC_OPT_LEVEL={opt_level}");
    }
}

#[cfg(target_os = "windows")]
fn deploy_windows_runtime_dlls() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is unavailable")?);
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .ok_or("OUT_DIR does not have the expected Cargo profile layout")?;
    let runtime_dir = if let Some(ffmpeg_dir) = std::env::var_os("FFMPEG_DIR") {
        PathBuf::from(ffmpeg_dir).join("bin")
    } else if let Some(vcpkg_root) = std::env::var_os("VCPKG_ROOT") {
        let triplet = std::env::var("VCPKG_DEFAULT_TRIPLET").unwrap_or_else(|_| {
            if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
                "arm64-windows".to_owned()
            } else {
                "x64-windows".to_owned()
            }
        });
        PathBuf::from(vcpkg_root).join("installed").join(triplet).join("bin")
    } else {
        return Ok(());
    };

    copy_runtime_dlls(&runtime_dir, profile_dir)
}

#[cfg(target_os = "windows")]
fn copy_runtime_dlls(
    runtime_dir: &std::path::Path,
    profile_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !runtime_dir.is_dir() {
        return Err(format!(
            "runtime directory does not exist: {}",
            runtime_dir.display()
        )
        .into());
    }
    std::fs::create_dir_all(profile_dir)?;
    for entry in std::fs::read_dir(runtime_dir)? {
        let entry = entry?;
        let source = entry.path();
        if source
            .extension()
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("dll"))
        {
            continue;
        }
        let destination = profile_dir.join(entry.file_name());
        if should_copy_runtime_file(&source, &destination)? {
            std::fs::copy(source, destination)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn should_copy_runtime_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<bool> {
    let source_metadata = std::fs::metadata(source)?;
    let Ok(destination_metadata) = std::fs::metadata(destination) else {
        return Ok(true);
    };
    Ok(source_metadata.len() != destination_metadata.len()
        || source_metadata.modified()? > destination_metadata.modified()?)
}

#[cfg(target_os = "windows")]
fn embed_windows_icon() -> Result<(), Box<dyn std::error::Error>> {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/favicon.ico");
    res.compile()?;

    Ok(())
}
