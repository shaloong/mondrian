use std::fs;

/// 配置 UI 字体
pub fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    apply_system_font_fallbacks(&mut fonts);
    ctx.set_fonts(fonts);
}

fn apply_system_font_fallbacks(fonts: &mut egui::FontDefinitions) {
    for (name, bytes) in platform_proportional_fonts() {
        fonts.font_data.insert(name.clone(), egui::FontData::from_owned(bytes).into());
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, name);
        }
    }

    for (name, bytes) in platform_monospace_fonts() {
        fonts.font_data.insert(name.clone(), egui::FontData::from_owned(bytes).into());
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            family.insert(0, name);
        }
    }
}

fn load_fonts_from_paths(prefix: &str, paths: &[&str]) -> Vec<(String, Vec<u8>)> {
    paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| fs::read(path).ok().map(|bytes| (index, bytes)))
        .map(|(index, bytes)| (format!("{prefix}_{index}"), bytes))
        .collect()
}

#[cfg(target_os = "windows")]
fn platform_proportional_fonts() -> Vec<(String, Vec<u8>)> {
    load_fonts_from_paths(
        "sys_prop",
        &[
            r"C:\\Windows\\Fonts\\segoeui.ttf",
            r"C:\\Windows\\Fonts\\msyh.ttc",
            r"C:\\Windows\\Fonts\\msjh.ttc",
            r"C:\\Windows\\Fonts\\arial.ttf",
        ],
    )
}

#[cfg(target_os = "macos")]
fn platform_proportional_fonts() -> Vec<(String, Vec<u8>)> {
    load_fonts_from_paths(
        "sys_prop",
        &[
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ],
    )
}

#[cfg(target_os = "linux")]
fn platform_proportional_fonts() -> Vec<(String, Vec<u8>)> {
    load_fonts_from_paths(
        "sys_prop",
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ],
    )
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_proportional_fonts() -> Vec<(String, Vec<u8>)> {
    Vec::new()
}

#[cfg(target_os = "windows")]
fn platform_monospace_fonts() -> Vec<(String, Vec<u8>)> {
    load_fonts_from_paths(
        "sys_mono",
        &[
            r"C:\\Windows\\Fonts\\consola.ttf",
            r"C:\\Windows\\Fonts\\CascadiaCode.ttf",
            r"C:\\Windows\\Fonts\\msgothic.ttc",
        ],
    )
}

#[cfg(target_os = "macos")]
fn platform_monospace_fonts() -> Vec<(String, Vec<u8>)> {
    load_fonts_from_paths(
        "sys_mono",
        &[
            "/System/Library/Fonts/SFMono-Regular.otf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/PingFang.ttc",
        ],
    )
}

#[cfg(target_os = "linux")]
fn platform_monospace_fonts() -> Vec<(String, Vec<u8>)> {
    load_fonts_from_paths(
        "sys_mono",
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansMonoCJK-Regular.ttc",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        ],
    )
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_monospace_fonts() -> Vec<(String, Vec<u8>)> {
    Vec::new()
}
