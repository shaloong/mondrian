fn main() {
    println!("cargo:rerun-if-changed=assets/favicon.ico");

    #[cfg(target_os = "windows")]
    {
        if let Err(err) = embed_windows_icon() {
            println!("cargo:warning=写入 Windows 图标资源失败: {err}");
        }
    }
}

#[cfg(target_os = "windows")]
fn embed_windows_icon() -> Result<(), Box<dyn std::error::Error>> {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/favicon.ico");
    res.compile()?;

    Ok(())
}
