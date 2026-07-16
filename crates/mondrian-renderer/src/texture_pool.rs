//! GPU texture pool — size-class reuse for render targets.
//!
//! Creating and destroying textures every frame is expensive. This pool
//! retains textures by (width, height, format) and reuses them across
//! frames within a configurable capacity.

use parking_lot::Mutex;
use std::collections::HashMap;
use wgpu;

/// Key for pooling textures of the same dimensions and format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TextureKey {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
}

impl TextureKey {
    fn new(
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Self {
        Self { width, height, format, usage }
    }

    fn from_texture(texture: &wgpu::Texture) -> Self {
        Self::new(
            texture.width(),
            texture.height(),
            texture.format(),
            texture.usage(),
        )
    }
}

/// A texture held in the pool along with its last-used frame counter.
struct PooledTexture {
    texture: wgpu::Texture,
    last_used_frame: u64,
}

/// GPU texture pool for render target reuse.
///
/// ```text
/// let texture = pool.acquire(device, 1920, 1080, Rgba8Unorm, frame_num);
/// // ... use texture ...
/// // Texture is automatically returned on drop, or reuse next frame.
/// ```
pub struct TexturePool {
    /// Pooled textures by key.
    pool: Mutex<HashMap<TextureKey, Vec<PooledTexture>>>,
    /// Maximum textures to keep per key.
    max_per_key: usize,
    /// Maximum total textures across all keys.
    max_total: usize,
    /// Current frame counter for LRU eviction.
    frame_counter: Mutex<u64>,
}

impl TexturePool {
    pub fn new(max_per_key: usize, max_total: usize) -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
            max_per_key,
            max_total,
            frame_counter: Mutex::new(0),
        }
    }

    /// Advance the frame counter. Old unused textures may be evicted on next acquire.
    pub fn advance_frame(&self) {
        *self.frame_counter.lock() += 1;
    }

    /// Acquire a texture from the pool, or create a new one.
    pub fn acquire(
        &self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        let key = TextureKey::new(width, height, format, usage);

        {
            let mut pool = self.pool.lock();
            if let Some(textures) = pool.get_mut(&key) {
                if !textures.is_empty() {
                    let pooled = textures.swap_remove(0);
                    tracing::trace!(?key, "texture pool hit");
                    return pooled.texture;
                }
            }
        }

        tracing::trace!(?key, "texture pool miss — creating new texture");
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pooled_texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    /// Return a texture to the pool for future reuse.
    pub fn release(&self, texture: wgpu::Texture) {
        let key = TextureKey::from_texture(&texture);
        let frame = *self.frame_counter.lock();

        let mut pool = self.pool.lock();

        // Evict old entries if over capacity
        let total: usize = pool.values().map(|v| v.len()).sum();
        if total >= self.max_total {
            self.evict_lru(&mut pool, frame);
        }

        let entries = pool.entry(key).or_default();
        if entries.len() >= self.max_per_key {
            entries.remove(0); // drop oldest
        }
        entries.push(PooledTexture { texture, last_used_frame: frame });
    }

    fn evict_lru(&self, pool: &mut HashMap<TextureKey, Vec<PooledTexture>>, current_frame: u64) {
        // Drop textures not used in the last 3 frames.
        let threshold = current_frame.saturating_sub(3);
        for textures in pool.values_mut() {
            textures.retain(|t| t.last_used_frame >= threshold);
        }
        pool.retain(|_, v| !v.is_empty());
    }

    /// Clear all pooled textures.
    pub fn clear(&self) {
        self.pool.lock().clear();
    }

    /// Number of textures currently pooled.
    pub fn len(&self) -> usize {
        self.pool.lock().values().map(|v| v.len()).sum()
    }

    /// Returns true if no textures are currently pooled.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::TextureKey;

    #[test]
    fn texture_pool_keys_preserve_exact_texture_contract() {
        let render_sample =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let render_copy = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        let rgba = TextureKey::new(1920, 1080, wgpu::TextureFormat::Rgba8Unorm, render_sample);
        let bgra = TextureKey::new(1920, 1080, wgpu::TextureFormat::Bgra8Unorm, render_sample);
        let hdr = TextureKey::new(1920, 1080, wgpu::TextureFormat::Rgb10a2Unorm, render_sample);
        let different_usage =
            TextureKey::new(1920, 1080, wgpu::TextureFormat::Rgba8Unorm, render_copy);

        assert_ne!(rgba, bgra);
        assert_ne!(rgba, hdr);
        assert_ne!(bgra, hdr);
        assert_ne!(rgba, different_usage);
    }
}
