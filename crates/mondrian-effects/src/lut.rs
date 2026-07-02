//! 3D LUT 加载、校验与 CPU 采样。

use mondrian_core::{MondrianError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lut3D {
    pub name: String,
    pub size: u32,
    pub data: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LutLibraryEntry {
    pub name: String,
    pub path: PathBuf,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct LutLibrary {
    root: PathBuf,
}

impl LutLibrary {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn import_cube_file(&self, source: &Path) -> Result<PathBuf> {
        if source.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase)
            != Some("cube".to_string())
        {
            return Err(lut_error("LUT library only accepts .cube files"));
        }
        let parsed = Lut3D::from_cube_file(source)?;
        self.ensure_root()?;
        let file_name = source
            .file_name()
            .ok_or_else(|| lut_error("LUT source path has no file name"))?;
        let destination = self.root.join(file_name);
        std::fs::copy(source, &destination)?;
        tracing::info!(
            path = %destination.display(),
            size = parsed.size,
            "imported LUT into application library"
        );
        Ok(destination)
    }

    pub fn list_luts(&self) -> Result<Vec<LutLibraryEntry>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        for item in std::fs::read_dir(&self.root)? {
            let item = item?;
            let path = item.path();
            if !path.is_file()
                || path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase)
                    != Some("cube".to_string())
            {
                continue;
            }
            let lut = Lut3D::from_cube_file(&path)?;
            entries.push(LutLibraryEntry { name: lut.name, path, size: lut.size });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        Ok(entries)
    }

    pub fn search_luts(&self, query: &str) -> Result<Vec<LutLibraryEntry>> {
        let query = query.trim().to_ascii_lowercase();
        let mut entries = self.list_luts()?;
        if query.is_empty() {
            return Ok(entries);
        }

        entries.retain(|entry| {
            entry.name.to_ascii_lowercase().contains(&query)
                || entry.path.display().to_string().to_ascii_lowercase().contains(&query)
                || entry.size.to_string().contains(&query)
        });
        Ok(entries)
    }

    pub fn remove_lut(&self, path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Lut3D {
    pub fn identity(size: u32) -> Result<Self> {
        validate_lut_size(size)?;
        let mut data = Vec::with_capacity((size * size * size) as usize);
        let denom = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / denom, g as f32 / denom, b as f32 / denom]);
                }
            }
        }
        Ok(Self { name: format!("identity-{size}"), size, data })
    }

    /// 从 .cube 文件解析 3D LUT
    pub fn from_cube_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
        Self::from_cube_str(name, &content)
    }

    pub fn from_cube_file_cached(path: &Path) -> Result<Self> {
        LutCache::global().load_cube(path)
    }

    pub fn from_cube_str(name: impl Into<String>, content: &str) -> Result<Self> {
        let mut size = 0u32;
        let mut data = Vec::new();

        for (line_index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if line.starts_with("TITLE")
                || line.starts_with("DOMAIN_MIN")
                || line.starts_with("DOMAIN_MAX")
                || line.starts_with("LUT_1D_SIZE")
            {
                continue;
            }
            if line.starts_with("LUT_3D_SIZE") {
                size = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(
                    || lut_error(format!("invalid LUT_3D_SIZE at line {}", line_index + 1)),
                )?;
                validate_lut_size(size)?;
                continue;
            }
            let vals: Vec<f32> = line
                .split_whitespace()
                .map(str::parse::<f32>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|err| {
                    lut_error(format!(
                        "invalid LUT value at line {}: {err}",
                        line_index + 1
                    ))
                })?;
            if vals.len() == 3 {
                if vals.iter().any(|v| !v.is_finite()) {
                    return Err(lut_error(format!(
                        "non-finite LUT value at line {}",
                        line_index + 1
                    )));
                }
                data.push([vals[0], vals[1], vals[2]]);
            } else {
                return Err(lut_error(format!(
                    "expected 3 LUT values at line {}, got {}",
                    line_index + 1,
                    vals.len()
                )));
            }
        }

        validate_lut_size(size)?;
        let expected = size as usize * size as usize * size as usize;
        if data.len() != expected {
            return Err(lut_error(format!(
                "LUT data length mismatch: expected {expected}, got {}",
                data.len()
            )));
        }

        Ok(Self { name: name.into(), size, data })
    }

    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.size < 2 || self.data.is_empty() {
            return rgb;
        }

        let max = (self.size - 1) as f32;
        let r = rgb[0].clamp(0.0, 1.0) * max;
        let g = rgb[1].clamp(0.0, 1.0) * max;
        let b = rgb[2].clamp(0.0, 1.0) * max;
        let r0 = r.floor() as u32;
        let g0 = g.floor() as u32;
        let b0 = b.floor() as u32;
        let r1 = (r0 + 1).min(self.size - 1);
        let g1 = (g0 + 1).min(self.size - 1);
        let b1 = (b0 + 1).min(self.size - 1);
        let fr = r - r0 as f32;
        let fg = g - g0 as f32;
        let fb = b - b0 as f32;

        let c000 = self.at(r0, g0, b0);
        let c100 = self.at(r1, g0, b0);
        let c010 = self.at(r0, g1, b0);
        let c110 = self.at(r1, g1, b0);
        let c001 = self.at(r0, g0, b1);
        let c101 = self.at(r1, g0, b1);
        let c011 = self.at(r0, g1, b1);
        let c111 = self.at(r1, g1, b1);

        lerp3(
            lerp3(lerp3(c000, c100, fr), lerp3(c010, c110, fr), fg),
            lerp3(lerp3(c001, c101, fr), lerp3(c011, c111, fr), fg),
            fb,
        )
    }

    pub fn apply_rgba8_in_place(&self, rgba: &mut [u8], intensity: f32) {
        let intensity = intensity.clamp(0.0, 1.0);
        if intensity <= 1.0e-4 {
            return;
        }
        for px in rgba.chunks_exact_mut(4) {
            let src = [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ];
            let graded = self.sample(src);
            let out = lerp3(src, graded, intensity);
            px[0] = (out[0].clamp(0.0, 1.0) * 255.0).round() as u8;
            px[1] = (out[1].clamp(0.0, 1.0) * 255.0).round() as u8;
            px[2] = (out[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }

    fn at(&self, r: u32, g: u32, b: u32) -> [f32; 3] {
        let idx = (b * self.size * self.size + g * self.size + r) as usize;
        self.data.get(idx).copied().unwrap_or([0.0, 0.0, 0.0])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LutCacheFingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
struct LutCacheEntry {
    fingerprint: LutCacheFingerprint,
    lut: Lut3D,
}

#[derive(Debug, Default)]
pub struct LutCache {
    entries: RwLock<HashMap<PathBuf, LutCacheEntry>>,
}

impl LutCache {
    pub fn global() -> &'static Self {
        static CACHE: OnceLock<LutCache> = OnceLock::new();
        CACHE.get_or_init(LutCache::default)
    }

    pub fn load_cube(&self, path: &Path) -> Result<Lut3D> {
        let key = cache_key_for_path(path);
        let fingerprint = lut_file_fingerprint(path)?;
        if let Some(entry) = self
            .entries
            .read()
            .expect("LUT cache read lock")
            .get(&key)
            .filter(|entry| entry.fingerprint == fingerprint)
        {
            return Ok(entry.lut.clone());
        }

        let lut = Lut3D::from_cube_file(path)?;
        self.entries
            .write()
            .expect("LUT cache write lock")
            .insert(key, LutCacheEntry { fingerprint, lut: lut.clone() });
        Ok(lut)
    }

    pub fn clear(&self) {
        self.entries.write().expect("LUT cache write lock").clear();
    }

    pub fn len(&self) -> usize {
        self.entries.read().expect("LUT cache read lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().expect("LUT cache read lock").is_empty()
    }
}

fn cache_key_for_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn lut_file_fingerprint(path: &Path) -> Result<LutCacheFingerprint> {
    let metadata = std::fs::metadata(path)?;
    Ok(LutCacheFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn validate_lut_size(size: u32) -> Result<()> {
    if !(2..=129).contains(&size) {
        return Err(lut_error(format!("unsupported 3D LUT size: {size}")));
    }
    Ok(())
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn lut_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::UnsupportedFormat { format: reason.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn cube_identity_2() -> &'static str {
        "LUT_3D_SIZE 2
0 0 0
1 0 0
0 1 0
1 1 0
0 0 1
1 0 1
0 1 1
1 1 1
"
    }

    #[test]
    fn identity_lut_samples_midpoint() {
        let lut = Lut3D::identity(2).expect("identity lut");
        let sampled = lut.sample([0.25, 0.5, 0.75]);
        assert!((sampled[0] - 0.25).abs() < 1.0e-6);
        assert!((sampled[1] - 0.5).abs() < 1.0e-6);
        assert!((sampled[2] - 0.75).abs() < 1.0e-6);
    }

    #[test]
    fn cube_parser_rejects_missing_values() {
        let cube = "LUT_3D_SIZE 2\n0 0 0\n";
        assert!(Lut3D::from_cube_str("bad", cube).is_err());
    }

    #[test]
    fn lut_library_imports_valid_cube_files() {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let root = std::env::temp_dir().join(format!("mondrian-lut-library-{unique}"));
        let source_dir = root.join("source");
        let library_dir = root.join("library");
        std::fs::create_dir_all(&source_dir).expect("source dir");
        let source = source_dir.join("identity.cube");
        std::fs::write(&source, cube_identity_2()).expect("cube file");

        let library = LutLibrary::new(&library_dir);
        let imported = library.import_cube_file(&source).expect("imported");
        assert_eq!(imported.file_name(), source.file_name());
        let entries = library.list_luts().expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "identity");
        assert_eq!(entries[0].size, 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lut_library_rejects_non_cube_imports() {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let root = std::env::temp_dir().join(format!("mondrian-lut-library-reject-{unique}"));
        std::fs::create_dir_all(&root).expect("root");
        let source = root.join("not-a-lut.txt");
        std::fs::write(&source, cube_identity_2()).expect("file");

        let library = LutLibrary::new(root.join("library"));
        assert!(library.import_cube_file(&source).is_err());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lut_library_supports_search_and_remove() {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let root = std::env::temp_dir().join(format!("mondrian-lut-library-search-{unique}"));
        let source_dir = root.join("source");
        let library_dir = root.join("library");
        std::fs::create_dir_all(&source_dir).expect("source dir");

        let source = source_dir.join("identity.cube");
        std::fs::write(&source, cube_identity_2()).expect("cube file");

        let library = LutLibrary::new(&library_dir);
        let imported = library.import_cube_file(&source).expect("imported");
        let filtered = library.search_luts("identity").expect("search");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, imported);

        library.remove_lut(&imported).expect("remove");
        let remaining = library.search_luts("").expect("list after remove");
        assert!(remaining.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lut_cache_reuses_entries_and_invalidates_on_file_change() {
        use std::time::UNIX_EPOCH;

        let cache = LutCache::default();
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let root = std::env::temp_dir().join(format!("mondrian-lut-cache-{unique}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        let path = root.join("look.cube");
        std::fs::write(&path, cube_identity_2()).expect("cube");

        let first = cache.load_cube(&path).expect("first");
        let second = cache.load_cube(&path).expect("second");
        assert_eq!(first, second);
        assert_eq!(cache.len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(
            &path,
            "LUT_3D_SIZE 2
1 1 1
0 1 1
1 0 1
0 0 1
1 1 0
0 1 0
1 0 0
0 0 0
",
        )
        .expect("changed cube");
        let changed = cache.load_cube(&path).expect("changed");
        assert_ne!(first.data, changed.data);
        assert_eq!(
            cache.len(),
            1,
            "cache should still have 1 entry after file modification"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lut_apply_preserves_alpha_and_respects_intensity() {
        let lut = Lut3D {
            name: "invert".to_string(),
            size: 2,
            data: vec![
                [1.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
            ],
        };
        let mut rgba = vec![64, 128, 192, 77];
        lut.apply_rgba8_in_place(&mut rgba, 0.5);
        assert_eq!(rgba[3], 77);
        assert!(rgba[0] > 64);
        assert!((rgba[1] as i16 - 128).abs() <= 1);
        assert!(rgba[2] < 192);
    }
}
