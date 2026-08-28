//! Device-resident immutable grade resources for fused point grading.
//!
//! Effect compilation owns immutable creative-LUT and sampled color-curve
//! semantics. This module owns their device-specific packed 3D texture,
//! bounded residency, and bind-group lifetime. A cache entry is keyed only by
//! the sorted sets of complete semantic fingerprints, so animated intensity
//! and grade-node ordering reuse the same immutable upload without weakening
//! pixel semantics.

use mondrian_effects::{
    CompiledEffectGpuPlan, EffectGpuPointOp, PreparedColorCurves, PreparedLut3D,
    COLOR_CURVE_SAMPLE_COUNT, COLOR_CURVE_SAMPLE_ROWS,
};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    mem::size_of,
    sync::Arc,
};

const COPY_BYTES_PER_ROW_ALIGNMENT: usize = 256;
const RGBA32F_TEXEL_BYTES: usize = 4 * size_of::<f32>();
const LUT_INTENSITY_NOOP_THRESHOLD: f32 = 1.0e-4;

/// Bounds for device-resident creative LUT sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuCreativeLutCacheConfig {
    /// Maximum distinct LUT-set bindings retained by one compositor.
    pub max_entries: usize,
    /// Maximum logical GPU texture bytes retained by one compositor.
    pub max_texture_bytes: usize,
}

impl Default for GpuCreativeLutCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 16,
            max_texture_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Cumulative upload and bounded-residency evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GpuCreativeLutCacheDiagnostics {
    /// Cache lookups satisfied without a device upload.
    pub cache_hits: u64,
    /// Cache lookups that required materialization.
    pub cache_misses: u64,
    /// Packed immutable 3D textures uploaded.
    pub texture_uploads: u64,
    /// Resident entries evicted to maintain configured bounds.
    pub evictions: u64,
    /// Oversized entries executed once without being retained.
    pub oversized_bypasses: u64,
    /// Currently retained LUT-set entries.
    pub resident_entries: usize,
    /// Currently retained logical GPU texture bytes.
    pub resident_texture_bytes: usize,
}

/// Failure to materialize an exact creative LUT binding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GpuCreativeLutError {
    /// Two different payloads claimed the same complete semantic fingerprint.
    #[error("different creative LUT payloads share semantic fingerprint {fingerprint:02x?}")]
    SemanticFingerprintCollision { fingerprint: [u8; 32] },
    /// A prepared plan binding omitted one active LUT resource.
    #[error("prepared creative LUT binding is missing fingerprint {fingerprint:02x?}")]
    PreparedBindingMissing { fingerprint: [u8; 32] },
    /// Two different curve payloads claimed the same complete semantic fingerprint.
    #[error("different color-curve payloads share semantic fingerprint {fingerprint:02x?}")]
    CurveSemanticFingerprintCollision { fingerprint: [u8; 32] },
    /// A prepared plan binding omitted one active color-curve resource.
    #[error("prepared color-curve binding is missing fingerprint {fingerprint:02x?}")]
    PreparedCurveBindingMissing { fingerprint: [u8; 32] },
    /// Packed texture dimensions exceed the active device contract.
    #[error(
        "creative LUT atlas {width}x{height}x{depth} exceeds max 3D texture dimension {maximum}"
    )]
    TextureExtentUnsupported {
        width: u32,
        height: u32,
        depth: u32,
        maximum: u32,
    },
    /// Packed texture byte accounting overflowed the host address space.
    #[error("creative LUT atlas byte size overflow")]
    TextureByteSizeOverflow,
    /// One upload row cannot be represented by the wgpu copy contract.
    #[error("creative LUT upload row layout overflow")]
    UploadLayoutOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GpuCreativeLutSetIdentity {
    lut_fingerprints: Arc<[[u8; 32]]>,
    curve_fingerprints: Arc<[[u8; 32]]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GpuCreativeLutLocation {
    pub(crate) base_layer: u32,
    pub(crate) edge_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GpuColorCurveLocation {
    pub(crate) base_layer: u32,
    pub(crate) sample_count: u32,
}

pub(crate) struct GpuCreativeLutPreparedBinding {
    resident: Arc<GpuCreativeLutResidentSet>,
}

impl GpuCreativeLutPreparedBinding {
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.resident.bind_group
    }

    pub(crate) fn location(&self, lut: &PreparedLut3D) -> Option<GpuCreativeLutLocation> {
        self.resident.locations.get(lut.semantic_fingerprint()).copied()
    }

    pub(crate) fn curve_location(
        &self,
        curves: &PreparedColorCurves,
    ) -> Option<GpuColorCurveLocation> {
        self.resident.curve_locations.get(curves.semantic_fingerprint()).copied()
    }
}

struct GpuCreativeLutResidentSet {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    locations: HashMap<[u8; 32], GpuCreativeLutLocation>,
    curve_locations: HashMap<[u8; 32], GpuColorCurveLocation>,
    texture_bytes: usize,
}

struct GpuCreativeLutCacheState {
    entries: HashMap<GpuCreativeLutSetIdentity, Arc<GpuCreativeLutResidentSet>>,
    lru: VecDeque<GpuCreativeLutSetIdentity>,
    diagnostics: GpuCreativeLutCacheDiagnostics,
}

/// One device-scoped bounded creative-LUT residency owner.
pub(crate) struct GpuCreativeLutRuntime {
    layout: wgpu::BindGroupLayout,
    dummy: Arc<GpuCreativeLutResidentSet>,
    config: GpuCreativeLutCacheConfig,
    state: Mutex<GpuCreativeLutCacheState>,
}

impl GpuCreativeLutRuntime {
    pub(crate) fn new(device: &wgpu::Device, config: GpuCreativeLutCacheConfig) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.creative-lut.layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let dummy = Arc::new(create_dummy_resident_set(device, &layout));
        Self {
            layout,
            dummy,
            config,
            state: Mutex::new(GpuCreativeLutCacheState {
                entries: HashMap::new(),
                lru: VecDeque::new(),
                diagnostics: GpuCreativeLutCacheDiagnostics::default(),
            }),
        }
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn prepare_plan(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<&CompiledEffectGpuPlan>,
    ) -> Result<GpuCreativeLutPreparedBinding, GpuCreativeLutError> {
        let luts = collect_active_luts(plan)?;
        let curves = collect_active_curves(plan)?;
        if luts.is_empty() && curves.is_empty() {
            return Ok(GpuCreativeLutPreparedBinding { resident: Arc::clone(&self.dummy) });
        }
        let key = GpuCreativeLutSetIdentity {
            lut_fingerprints: luts
                .iter()
                .map(|lut| *lut.semantic_fingerprint())
                .collect::<Vec<_>>()
                .into(),
            curve_fingerprints: curves
                .iter()
                .map(|curve| *curve.semantic_fingerprint())
                .collect::<Vec<_>>()
                .into(),
        };
        let mut state = self.state.lock();
        if let Some(resident) = state.entries.get(&key).cloned() {
            state.diagnostics.cache_hits = state.diagnostics.cache_hits.saturating_add(1);
            touch_lru(&mut state.lru, &key);
            return Ok(GpuCreativeLutPreparedBinding { resident });
        }
        state.diagnostics.cache_misses = state.diagnostics.cache_misses.saturating_add(1);
        let resident = Arc::new(upload_resident_set(
            device,
            queue,
            &self.layout,
            &luts,
            &curves,
        )?);
        state.diagnostics.texture_uploads = state.diagnostics.texture_uploads.saturating_add(1);
        if self.config.max_entries == 0 || resident.texture_bytes > self.config.max_texture_bytes {
            state.diagnostics.oversized_bypasses =
                state.diagnostics.oversized_bypasses.saturating_add(1);
            return Ok(GpuCreativeLutPreparedBinding { resident });
        }
        while state.entries.len() >= self.config.max_entries
            || state.diagnostics.resident_texture_bytes.saturating_add(resident.texture_bytes)
                > self.config.max_texture_bytes
        {
            let Some(oldest) = state.lru.pop_front() else {
                break;
            };
            if let Some(evicted) = state.entries.remove(&oldest) {
                state.diagnostics.resident_texture_bytes =
                    state.diagnostics.resident_texture_bytes.saturating_sub(evicted.texture_bytes);
                state.diagnostics.evictions = state.diagnostics.evictions.saturating_add(1);
            }
        }
        state.diagnostics.resident_texture_bytes =
            state.diagnostics.resident_texture_bytes.saturating_add(resident.texture_bytes);
        state.entries.insert(key.clone(), Arc::clone(&resident));
        state.lru.push_back(key);
        state.diagnostics.resident_entries = state.entries.len();
        Ok(GpuCreativeLutPreparedBinding { resident })
    }

    pub(crate) fn diagnostics(&self) -> GpuCreativeLutCacheDiagnostics {
        self.state.lock().diagnostics
    }
}

fn collect_active_curves(
    plan: Option<&CompiledEffectGpuPlan>,
) -> Result<Vec<Arc<PreparedColorCurves>>, GpuCreativeLutError> {
    let mut unique = BTreeMap::<[u8; 32], Arc<PreparedColorCurves>>::new();
    for operation in plan.into_iter().flat_map(CompiledEffectGpuPlan::operations) {
        let EffectGpuPointOp::ColorCurves { curves } = operation else {
            continue;
        };
        let fingerprint = *curves.semantic_fingerprint();
        if let Some(existing) = unique.get(&fingerprint) {
            if existing.as_ref() != curves.as_ref() {
                return Err(GpuCreativeLutError::CurveSemanticFingerprintCollision { fingerprint });
            }
        } else {
            unique.insert(fingerprint, Arc::clone(curves));
        }
    }
    Ok(unique.into_values().collect())
}

fn collect_active_luts(
    plan: Option<&CompiledEffectGpuPlan>,
) -> Result<Vec<Arc<PreparedLut3D>>, GpuCreativeLutError> {
    let mut unique = BTreeMap::<[u8; 32], Arc<PreparedLut3D>>::new();
    for operation in plan.into_iter().flat_map(CompiledEffectGpuPlan::operations) {
        let EffectGpuPointOp::Lut3D { lut, intensity } = operation else {
            continue;
        };
        if intensity.clamp(0.0, 1.0) <= LUT_INTENSITY_NOOP_THRESHOLD {
            continue;
        }
        let fingerprint = *lut.semantic_fingerprint();
        if let Some(existing) = unique.get(&fingerprint) {
            if existing.lut() != lut.lut() {
                return Err(GpuCreativeLutError::SemanticFingerprintCollision { fingerprint });
            }
        } else {
            unique.insert(fingerprint, Arc::clone(lut));
        }
    }
    Ok(unique.into_values().collect())
}

fn upload_resident_set(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    luts: &[Arc<PreparedLut3D>],
    curves: &[Arc<PreparedColorCurves>],
) -> Result<GpuCreativeLutResidentSet, GpuCreativeLutError> {
    let curve_width = u32::try_from(COLOR_CURVE_SAMPLE_COUNT).map_err(|_| {
        GpuCreativeLutError::TextureExtentUnsupported {
            width: u32::MAX,
            height: 0,
            depth: 0,
            maximum: device.limits().max_texture_dimension_3d,
        }
    })?;
    let width = luts
        .iter()
        .map(|lut| lut.size)
        .chain((!curves.is_empty()).then_some(curve_width))
        .max()
        .unwrap_or(1);
    let height = luts
        .iter()
        .map(|lut| lut.size)
        .chain((!curves.is_empty()).then_some(COLOR_CURVE_SAMPLE_ROWS as u32))
        .max()
        .unwrap_or(1);
    let depth = luts
        .iter()
        .try_fold(0_u32, |total, lut| {
            total.checked_add(lut.size).ok_or(GpuCreativeLutError::TextureByteSizeOverflow)
        })?
        .checked_add(curves.len() as u32)
        .ok_or(GpuCreativeLutError::TextureByteSizeOverflow)?;
    let maximum = device.limits().max_texture_dimension_3d;
    if width > maximum || height > maximum || depth > maximum {
        return Err(GpuCreativeLutError::TextureExtentUnsupported {
            width,
            height,
            depth,
            maximum,
        });
    }
    let texture_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(height as usize))
        .and_then(|area| area.checked_mul(depth as usize))
        .and_then(|texels| texels.checked_mul(RGBA32F_TEXEL_BYTES))
        .ok_or(GpuCreativeLutError::TextureByteSizeOverflow)?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mondrian.creative-lut.atlas"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: depth },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let mut locations = HashMap::with_capacity(luts.len());
    let mut curve_locations = HashMap::with_capacity(curves.len());
    let mut base_layer = 0_u32;
    for lut in luts {
        let (bytes, bytes_per_row) = pack_lut_slab(lut)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: base_layer },
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(lut.size),
            },
            wgpu::Extent3d {
                width: lut.size,
                height: lut.size,
                depth_or_array_layers: lut.size,
            },
        );
        locations.insert(
            *lut.semantic_fingerprint(),
            GpuCreativeLutLocation { base_layer, edge_size: lut.size },
        );
        base_layer = base_layer
            .checked_add(lut.size)
            .ok_or(GpuCreativeLutError::TextureByteSizeOverflow)?;
    }
    for curve in curves {
        let (bytes, bytes_per_row) = pack_curve_slab(curve)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: base_layer },
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(COLOR_CURVE_SAMPLE_ROWS as u32),
            },
            wgpu::Extent3d {
                width: COLOR_CURVE_SAMPLE_COUNT as u32,
                height: COLOR_CURVE_SAMPLE_ROWS as u32,
                depth_or_array_layers: 1,
            },
        );
        curve_locations.insert(
            *curve.semantic_fingerprint(),
            GpuColorCurveLocation {
                base_layer,
                sample_count: COLOR_CURVE_SAMPLE_COUNT as u32,
            },
        );
        base_layer =
            base_layer.checked_add(1).ok_or(GpuCreativeLutError::TextureByteSizeOverflow)?;
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.creative-lut.atlas-view"),
        dimension: Some(wgpu::TextureViewDimension::D3),
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        ..wgpu::TextureViewDescriptor::default()
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mondrian.creative-lut.bind-group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    Ok(GpuCreativeLutResidentSet {
        _texture: texture,
        bind_group,
        locations,
        curve_locations,
        texture_bytes,
    })
}

fn create_dummy_resident_set(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> GpuCreativeLutResidentSet {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mondrian.creative-lut.dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.creative-lut.dummy-view"),
        dimension: Some(wgpu::TextureViewDimension::D3),
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        ..wgpu::TextureViewDescriptor::default()
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mondrian.creative-lut.dummy-bind-group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    GpuCreativeLutResidentSet {
        _texture: texture,
        bind_group,
        locations: HashMap::new(),
        curve_locations: HashMap::new(),
        texture_bytes: 0,
    }
}

fn pack_curve_slab(curves: &PreparedColorCurves) -> Result<(Vec<u8>, u32), GpuCreativeLutError> {
    let source_row_bytes = COLOR_CURVE_SAMPLE_COUNT
        .checked_mul(RGBA32F_TEXEL_BYTES)
        .ok_or(GpuCreativeLutError::UploadLayoutOverflow)?;
    let bytes_per_row = source_row_bytes
        .div_ceil(COPY_BYTES_PER_ROW_ALIGNMENT)
        .checked_mul(COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or(GpuCreativeLutError::UploadLayoutOverflow)?;
    let mut bytes = vec![0_u8; bytes_per_row * COLOR_CURVE_SAMPLE_ROWS];
    for (row_index, row) in curves.samples().iter().enumerate() {
        for (sample_index, sample) in row.iter().enumerate() {
            let destination_texel = row_index * bytes_per_row + sample_index * RGBA32F_TEXEL_BYTES;
            for (component, value) in sample.iter().enumerate() {
                let destination = destination_texel + component * size_of::<f32>();
                bytes[destination..destination + size_of::<f32>()]
                    .copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok((
        bytes,
        u32::try_from(bytes_per_row).map_err(|_| GpuCreativeLutError::UploadLayoutOverflow)?,
    ))
}

fn pack_lut_slab(lut: &PreparedLut3D) -> Result<(Vec<u8>, u32), GpuCreativeLutError> {
    let edge = lut.size as usize;
    let source_row_bytes = edge
        .checked_mul(RGBA32F_TEXEL_BYTES)
        .ok_or(GpuCreativeLutError::UploadLayoutOverflow)?;
    let bytes_per_row = source_row_bytes
        .div_ceil(COPY_BYTES_PER_ROW_ALIGNMENT)
        .checked_mul(COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or(GpuCreativeLutError::UploadLayoutOverflow)?;
    let byte_len = bytes_per_row
        .checked_mul(edge)
        .and_then(|plane| plane.checked_mul(edge))
        .ok_or(GpuCreativeLutError::UploadLayoutOverflow)?;
    let mut bytes = vec![0_u8; byte_len];
    for blue in 0..edge {
        for green in 0..edge {
            let destination_row = (blue * edge + green) * bytes_per_row;
            for red in 0..edge {
                let source = lut.data[blue * edge * edge + green * edge + red];
                let destination_texel = destination_row + red * RGBA32F_TEXEL_BYTES;
                for (component, value) in source.into_iter().chain([1.0]).enumerate() {
                    let destination = destination_texel + component * size_of::<f32>();
                    bytes[destination..destination + size_of::<f32>()]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
    let bytes_per_row =
        u32::try_from(bytes_per_row).map_err(|_| GpuCreativeLutError::UploadLayoutOverflow)?;
    Ok((bytes, bytes_per_row))
}

fn touch_lru(lru: &mut VecDeque<GpuCreativeLutSetIdentity>, key: &GpuCreativeLutSetIdentity) {
    if let Some(index) = lru.iter().position(|candidate| candidate == key) {
        lru.remove(index);
    }
    lru.push_back(key.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::{
        compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
        EffectRenderOp, Lut3D,
    };

    #[test]
    fn packed_lut_slab_preserves_red_fastest_cube_order_and_alignment() {
        let lut = PreparedLut3D::new(Lut3D {
            name: "ordered".to_owned(),
            size: 2,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data: (0..8).map(|index| [index as f32, 100.0 + index as f32, 200.0]).collect(),
        });
        let (bytes, bytes_per_row) = pack_lut_slab(&lut).expect("packed LUT");
        assert_eq!(bytes_per_row, 256);
        let read = |blue: usize, green: usize, red: usize, component: usize| {
            let offset = (blue * 2 + green) * bytes_per_row as usize
                + red * RGBA32F_TEXEL_BYTES
                + component * size_of::<f32>();
            f32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("component"))
        };
        assert_eq!(read(0, 0, 0, 0), 0.0);
        assert_eq!(read(0, 0, 1, 0), 1.0);
        assert_eq!(read(0, 1, 0, 0), 2.0);
        assert_eq!(read(1, 0, 0, 0), 4.0);
        assert_eq!(read(1, 1, 1, 1), 107.0);
        assert_eq!(read(1, 1, 1, 3), 1.0);
    }

    #[tokio::test]
    async fn device_residency_reuses_lut_across_animated_grade_parameters() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping creative LUT residency test: no GPU adapter available");
            return;
        };
        let lut = Arc::new(PreparedLut3D::new(
            Lut3D::identity(3).expect("identity LUT"),
        ));
        let plan = |intensity| {
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::ColorAdjust {
                exposure: intensity * 0.1,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            });
            builder.append_unary(EffectRenderOp::Lut3D { lut: Arc::clone(&lut), intensity });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            lower_effect_graph_to_gpu_plan(&graph).expect("GPU LUT plan")
        };
        let first_plan = plan(0.4);
        let second_plan = plan(0.9);
        let noop_plan = plan(0.0);
        let runtime = GpuCreativeLutRuntime::new(
            &context.device,
            GpuCreativeLutCacheConfig { max_entries: 2, max_texture_bytes: 1024 * 1024 },
        );

        let first = runtime
            .prepare_plan(&context.device, &context.queue, Some(&first_plan))
            .expect("first LUT binding");
        assert_eq!(
            first.location(&lut),
            Some(GpuCreativeLutLocation { base_layer: 0, edge_size: 3 })
        );
        let second = runtime
            .prepare_plan(&context.device, &context.queue, Some(&second_plan))
            .expect("reused LUT binding");
        assert_eq!(second.location(&lut), first.location(&lut));
        let noop = runtime
            .prepare_plan(&context.device, &context.queue, Some(&noop_plan))
            .expect("no-op LUT binding");
        assert_eq!(noop.location(&lut), None);

        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.cache_misses, 1);
        assert_eq!(diagnostics.cache_hits, 1);
        assert_eq!(diagnostics.texture_uploads, 1);
        assert_eq!(diagnostics.resident_entries, 1);
        assert!(diagnostics.resident_texture_bytes > 0);
    }

    #[tokio::test]
    async fn device_residency_enforces_entry_and_byte_bounds() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping creative LUT bound test: no GPU adapter available");
            return;
        };
        let plan_for = |lut: Lut3D| {
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Lut3D {
                lut: Arc::new(PreparedLut3D::new(lut)),
                intensity: 1.0,
            });
            let graph = compile_reference_render_graph(builder.finish()).expect("valid graph");
            lower_effect_graph_to_gpu_plan(&graph).expect("GPU LUT plan")
        };
        let first = plan_for(Lut3D::identity(2).expect("first LUT"));
        let mut second_lut = Lut3D::identity(3).expect("second LUT");
        second_lut.name = "second-identity".to_owned();
        let second = plan_for(second_lut);
        let bounded = GpuCreativeLutRuntime::new(
            &context.device,
            GpuCreativeLutCacheConfig { max_entries: 1, max_texture_bytes: 1024 * 1024 },
        );
        bounded
            .prepare_plan(&context.device, &context.queue, Some(&first))
            .expect("first resident LUT");
        bounded
            .prepare_plan(&context.device, &context.queue, Some(&second))
            .expect("second resident LUT");
        bounded
            .prepare_plan(&context.device, &context.queue, Some(&first))
            .expect("first LUT re-upload after eviction");
        let diagnostics = bounded.diagnostics();
        assert_eq!(diagnostics.cache_misses, 3);
        assert_eq!(diagnostics.cache_hits, 0);
        assert_eq!(diagnostics.texture_uploads, 3);
        assert_eq!(diagnostics.evictions, 2);
        assert_eq!(diagnostics.resident_entries, 1);

        let bypassed = GpuCreativeLutRuntime::new(
            &context.device,
            GpuCreativeLutCacheConfig { max_entries: 1, max_texture_bytes: 1 },
        );
        for _ in 0..2 {
            bypassed
                .prepare_plan(&context.device, &context.queue, Some(&first))
                .expect("oversized one-shot LUT");
        }
        let diagnostics = bypassed.diagnostics();
        assert_eq!(diagnostics.cache_misses, 2);
        assert_eq!(diagnostics.cache_hits, 0);
        assert_eq!(diagnostics.texture_uploads, 2);
        assert_eq!(diagnostics.oversized_bypasses, 2);
        assert_eq!(diagnostics.resident_entries, 0);
        assert_eq!(diagnostics.resident_texture_bytes, 0);
    }
}
