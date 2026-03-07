//! 3D LUT 加载与管理

use mondrian_core::Result;
use std::path::Path;

pub struct Lut3D {
    pub name: String,
    pub size: u32,
    pub data: Vec<[f32; 3]>,
}

impl Lut3D {
    /// 从 .cube 文件解析 3D LUT
    pub fn from_cube_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();

        let mut size = 0u32;
        let mut data = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if line.starts_with("LUT_3D_SIZE") {
                size = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(33);
                continue;
            }
            let vals: Vec<f32> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
            if vals.len() == 3 {
                data.push([vals[0], vals[1], vals[2]]);
            }
        }

        Ok(Self { name, size, data })
    }
}
