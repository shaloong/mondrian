fn main() {
    println!("cargo:rerun-if-changed=assets/app-ico.png");

    #[cfg(target_os = "windows")]
    {
        if let Err(err) = embed_windows_icon_from_png() {
            println!("cargo:warning=写入 Windows 图标资源失败: {err}");
        }
    }
}

#[cfg(target_os = "windows")]
fn embed_windows_icon_from_png() -> Result<(), Box<dyn std::error::Error>> {
    use image::ImageEncoder;

    let png_path = std::path::Path::new("assets/app-ico.png");
    let out_dir = std::env::var("OUT_DIR")?;
    let ico_path = std::path::Path::new(&out_dir).join("app.ico");

    let mut img = image::open(png_path)?.into_rgba8();
    let (src_w, src_h) = img.dimensions();
    if src_w > 256 || src_h > 256 {
        img = image::imageops::thumbnail(&img, 256, 256);
    }
    let (width, height) = img.dimensions();

    let mut file = std::fs::File::create(&ico_path)?;
    let encoder = image::codecs::ico::IcoEncoder::new(&mut file);
    encoder.write_image(img.as_raw(), width, height, image::ColorType::Rgba8.into())?;

    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_string_lossy().as_ref());
    res.compile()?;

    Ok(())
}
