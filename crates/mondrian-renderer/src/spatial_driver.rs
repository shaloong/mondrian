//! GPU spatial effect chain admission planning and scratch-texture pooling.
//!
//! A lowered [`CompiledEffectGpuPipeline`] is a per-layer chain of spatial
//! kernels and standalone fused point stages. The compositor can execute that
//! chain on one layer's own working raster, but admission must be decidable
//! before any command is recorded: [`gpu_spatial_chain_plan`] is a pure function
//! that resolves every stage up front, and [`GpuSpatialScratchPool`] owns the
//! ping-pong rasters a chain rotates through.
//!
//! The driver boundary is straight alpha in, straight alpha out. Nothing here
//! premultiplies or unpremultiplies: each spatial kernel performs its own
//! premultiply/unpremultiply pair internally (see `spatial_pass_gpu`), and the
//! point stages and copy-out are bit-exact with respect to coverage.

use std::collections::HashMap;

use mondrian_effects::{
    CompiledEffectGpuPipeline, EffectFrameExtent, EffectGpuPointOp, EffectGpuSpatialPass,
    EffectGpuStage, EffectGraphNodeId, EffectRasterBounds,
};

use crate::product_gpu_working_texture_format;
use crate::spatial_pass_gpu::{EffectSpatialGpu, EffectSpatialGpuError};

pub mod cost;

pub use cost::{
    choose_spatial_chain, choose_spatial_chain_for_plan, SpatialChainChoice,
    SpatialChainChoiceDiagnostics, SpatialChainChoiceReason, SpatialChainPreference,
};

/// Retained scratch-texture byte budget for one compositor pool (512 MiB).
pub const GPU_SPATIAL_SCRATCH_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Largest raster edge a spatial chain admits without consulting a device.
///
/// `8192` is the WebGPU guaranteed `max_texture_dimension_2d`, so rejecting a
/// larger layer keeps admission independent of the physical adapter.
pub const GPU_SPATIAL_MAX_RASTER_DIMENSION: u32 = 8192;

/// Bytes per pixel of the fixed `Rgba32Float` spatial working format.
const GPU_SPATIAL_BYTES_PER_PIXEL: u64 = 16;

/// One resolved stage of a GPU spatial effect chain.
#[derive(Debug, Clone, PartialEq)]
pub enum GpuSpatialStagePlan {
    /// Fused pointwise operations recorded as one standalone full-raster pass.
    PointOps {
        /// Fused operations in graph order.
        operations: Vec<EffectGpuPointOp>,
    },
    /// One spatial kernel expanded into `scratch_textures` texture passes.
    Spatial {
        /// Kernel to record.
        pass: EffectGpuSpatialPass,
        /// Texture passes the kernel's pass table needs from the scratch pool.
        scratch_textures: u32,
        /// Horizontal halo the kernel may read outside its output texel.
        ///
        /// `u32::MAX` marks a kernel that may read any pixel of the frame; this
        /// driver always records the full layer raster, so the value is inert
        /// today and load-bearing only for a future tiled driver.
        horizontal_halo: u32,
        /// Vertical halo the kernel may read outside its output texel.
        vertical_halo: u32,
    },
}

impl GpuSpatialStagePlan {
    /// Spatial kernel of this stage, when it is not a point stage.
    pub fn spatial_pass(&self) -> Option<&EffectGpuSpatialPass> {
        match self {
            Self::PointOps { .. } => None,
            Self::Spatial { pass, .. } => Some(pass),
        }
    }

    /// Fused point operations of this stage, when it is not a spatial kernel.
    pub fn point_operations(&self) -> Option<&[EffectGpuPointOp]> {
        match self {
            Self::PointOps { operations } => Some(operations),
            Self::Spatial { .. } => None,
        }
    }
}

/// Fully resolved admission plan for one GPU spatial effect chain.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuSpatialChainPlan {
    /// Stages in lowering order; the driver must execute them as written.
    pub stages: Vec<GpuSpatialStagePlan>,
    /// Total spatial texture passes across every spatial stage.
    pub spatial_passes: u32,
    /// Number of standalone point stages.
    pub point_stages: u32,
    /// Scratch textures a chain must acquire: the largest pass count plus one.
    pub scratch_textures: usize,
    /// Bytes those scratch textures occupy at this raster extent.
    pub scratch_bytes: u64,
    /// Distinct raster extents recorded by spatial stages.
    pub(crate) scratch_extents: usize,
    /// Final raster dimensions and pixel origin relative to the input layer.
    pub output_bounds: EffectRasterBounds,
}

impl GpuSpatialChainPlan {
    /// Scratch textures as the pool's `u32` request.
    pub fn scratch_texture_request(&self) -> u32 {
        u32::try_from(self.scratch_textures).unwrap_or(u32::MAX)
    }
}

/// Why a lowered spatial pipeline cannot be executed as a GPU chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GpuSpatialChainBlocker {
    /// The pipeline has no stages to execute.
    #[error("spatial effect chain has no executable stages")]
    EmptyChain,
    /// An auxiliary stage has no admitted raster for its required slot.
    #[error("spatial chain requires auxiliary slot {slot}")]
    MissingAuxiliaryInput {
        /// Graph-declared slot.
        slot: u32,
    },
    /// A spatial kernel has no GPU implementation, or its pass table is unknown.
    #[error("spatial pass `{pass}` cannot run on the GPU: {detail}")]
    UnsupportedPass {
        /// Kernel or guard that refused.
        pass: &'static str,
        /// Full failure text.
        detail: String,
    },
    /// A point stage contains the geometry-only `Crop` operation.
    #[error("point stage contains a Crop geometry operation at node {node_id:?}")]
    CropInPointStage {
        /// Graph node that owns the rejected operation.
        node_id: EffectGraphNodeId,
    },
    /// The layer raster exceeds the largest extent admission can promise.
    #[error("spatial chain extent {width}x{height} exceeds the {limit} px limit")]
    ExtentExceedsDeviceLimit {
        /// Requested raster width.
        width: u32,
        /// Requested raster height.
        height: u32,
        /// Admitted maximum edge.
        limit: u32,
    },
    /// The resolved scratch pool would exceed the retained-byte budget.
    #[error("spatial chain needs {required_bytes} scratch bytes, budget is {budget_bytes}")]
    ScratchBudgetExceeded {
        /// Bytes the resolved chain requires.
        required_bytes: u64,
        /// Bytes the pool may retain.
        budget_bytes: u64,
    },
}

impl GpuSpatialChainBlocker {
    /// Machine-readable code for diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyChain => "spatial_chain_empty",
            Self::MissingAuxiliaryInput { .. } => "spatial_auxiliary_missing",
            Self::UnsupportedPass { .. } => "spatial_pass_unsupported",
            Self::CropInPointStage { .. } => "spatial_crop_in_point_stage",
            Self::ExtentExceedsDeviceLimit { .. } => "spatial_extent_exceeds_device_limit",
            Self::ScratchBudgetExceeded { .. } => "spatial_scratch_budget_exceeded",
        }
    }
}

/// Resolve a lowered spatial pipeline into an executable chain plan.
///
/// Pure: no device is consulted and no command is recorded. Every stage's
/// texture-pass count is resolved here, so a chain that reaches the compositor
/// has already proven it can allocate and run every stage.
pub fn gpu_spatial_chain_plan(
    pipeline: &CompiledEffectGpuPipeline,
    width: u32,
    height: u32,
) -> Result<GpuSpatialChainPlan, GpuSpatialChainBlocker> {
    gpu_spatial_chain_plan_with_auxiliary(pipeline, width, height, &[])
}

/// Plan a primary chain with the caller's admitted external texture slots.
pub fn gpu_spatial_chain_plan_with_auxiliary(
    pipeline: &CompiledEffectGpuPipeline,
    width: u32,
    height: u32,
    auxiliary_slots: &[u32],
) -> Result<GpuSpatialChainPlan, GpuSpatialChainBlocker> {
    if width == 0 || height == 0 {
        // An empty raster has no chain to run; the compositor rejects zero-sized
        // composites before this point, so this is a defensive refusal.
        return Err(GpuSpatialChainBlocker::EmptyChain);
    }
    if width > GPU_SPATIAL_MAX_RASTER_DIMENSION || height > GPU_SPATIAL_MAX_RASTER_DIMENSION {
        return Err(GpuSpatialChainBlocker::ExtentExceedsDeviceLimit {
            width,
            height,
            limit: GPU_SPATIAL_MAX_RASTER_DIMENSION,
        });
    }

    let mut stages = Vec::with_capacity(pipeline.stages().len());
    let mut spatial_passes = 0_u32;
    let mut point_stages = 0_u32;
    let mut largest_pass_count = 0_u32;
    let mut output_bounds = EffectRasterBounds::from_extent(EffectFrameExtent::new(width, height));
    let mut scratch_extents = std::collections::HashSet::new();
    for stage in pipeline.stages() {
        match stage {
            EffectGpuStage::PointOps { operations, node_ids } => {
                if let Some(index) = operations
                    .iter()
                    .position(|operation| matches!(operation, EffectGpuPointOp::Crop { .. }))
                {
                    // A standalone full-raster point pass would silently drop the
                    // crop geometry that the layer draw folds into its source rect.
                    let node_id = node_ids
                        .get(index)
                        .or_else(|| node_ids.first())
                        .copied()
                        .unwrap_or(EffectGraphNodeId(0));
                    return Err(GpuSpatialChainBlocker::CropInPointStage { node_id });
                }
                point_stages = point_stages.saturating_add(1);
                stages.push(GpuSpatialStagePlan::PointOps { operations: operations.clone() });
            }
            EffectGpuStage::Spatial { pass, .. } => {
                if let Some(external_slot) = pass.auxiliary_slot() {
                    let slot =
                        external_slot.ok_or_else(|| GpuSpatialChainBlocker::UnsupportedPass {
                            pass: pass.name(),
                            detail: "a graph-local auxiliary input requires the graph dispatcher"
                                .into(),
                        })?;
                    if !auxiliary_slots.contains(&slot) {
                        return Err(GpuSpatialChainBlocker::MissingAuxiliaryInput { slot });
                    }
                }
                output_bounds =
                    output_bounds.checked_outset(pass.output_outset()).ok_or_else(|| {
                        GpuSpatialChainBlocker::UnsupportedPass {
                            pass: pass.name(),
                            detail: "output raster geometry overflow".into(),
                        }
                    })?;
                let output = output_bounds.extent();
                scratch_extents.insert(output);
                if output.width() > GPU_SPATIAL_MAX_RASTER_DIMENSION
                    || output.height() > GPU_SPATIAL_MAX_RASTER_DIMENSION
                {
                    return Err(GpuSpatialChainBlocker::ExtentExceedsDeviceLimit {
                        width: output.width(),
                        height: output.height(),
                        limit: GPU_SPATIAL_MAX_RASTER_DIMENSION,
                    });
                }
                let passes = match EffectSpatialGpu::texture_pass_count(pass) {
                    Ok(passes) => passes,
                    Err(EffectSpatialGpuError::UnsupportedPass(name)) => {
                        return Err(GpuSpatialChainBlocker::UnsupportedPass {
                            pass: name,
                            detail: format!("spatial pass `{name}` has no GPU implementation"),
                        });
                    }
                    Err(error) => {
                        return Err(GpuSpatialChainBlocker::UnsupportedPass {
                            pass: "spatial_pass",
                            detail: error.to_string(),
                        });
                    }
                };
                spatial_passes = spatial_passes.saturating_add(passes);
                largest_pass_count = largest_pass_count.max(passes);
                stages.push(GpuSpatialStagePlan::Spatial {
                    pass: *pass,
                    scratch_textures: passes,
                    horizontal_halo: pass.horizontal_halo(),
                    vertical_halo: pass.vertical_halo(),
                });
            }
        }
    }
    if stages.is_empty() {
        return Err(GpuSpatialChainBlocker::EmptyChain);
    }

    // The pool always holds one more texture than the largest pass count needs,
    // so no stage's destination can alias the source it samples.
    let scratch_textures =
        usize::try_from(largest_pass_count).unwrap_or(usize::MAX).saturating_add(1);
    if scratch_extents.is_empty() {
        scratch_extents.insert(output_bounds.extent());
    }
    let pixels = scratch_extents.iter().fold(0u64, |pixels, extent| {
        pixels.saturating_add(u64::from(extent.width()) * u64::from(extent.height()))
    });
    let scratch_bytes = u64::try_from(scratch_textures)
        .unwrap_or(u64::MAX)
        .saturating_mul(pixels)
        .saturating_mul(GPU_SPATIAL_BYTES_PER_PIXEL);
    if scratch_bytes > GPU_SPATIAL_SCRATCH_BUDGET_BYTES {
        return Err(GpuSpatialChainBlocker::ScratchBudgetExceeded {
            required_bytes: scratch_bytes,
            budget_bytes: GPU_SPATIAL_SCRATCH_BUDGET_BYTES,
        });
    }

    Ok(GpuSpatialChainPlan {
        stages,
        spatial_passes,
        point_stages,
        scratch_textures,
        scratch_bytes,
        scratch_extents: scratch_extents.len(),
        output_bounds,
    })
}

/// Retained scratch-texture evidence for one compositor pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuSpatialScratchPoolDiagnostics {
    /// Distinct raster extents currently retained.
    pub extents: u32,
    /// Scratch textures currently retained across every extent.
    pub retained_textures: u32,
    /// Bytes those retained textures occupy.
    pub retained_bytes: u64,
    /// Peak retained textures since pool construction.
    pub peak_textures: u32,
    /// Peak retained bytes since pool construction.
    pub peak_bytes: u64,
}

struct GpuSpatialScratchExtent {
    textures: Vec<wgpu::Texture>,
    last_used: u64,
}

/// Per-compositor pool of straight-alpha `Rgba32Float` spatial scratch rasters.
///
/// Entries are keyed by raster extent and only ever grow, so a frame that asks
/// for a chain it already ran allocates nothing. Extents are evicted least
/// recently used first once the retained bytes exceed the budget.
pub struct GpuSpatialScratchPool {
    entries: HashMap<(u32, u32), GpuSpatialScratchExtent>,
    budget_bytes: u64,
    retained_bytes: u64,
    peak_textures: u32,
    peak_bytes: u64,
    tick: u64,
}

impl Default for GpuSpatialScratchPool {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuSpatialScratchPool {
    /// Create a pool with the product scratch-byte budget.
    pub fn new() -> Self {
        Self::with_budget(GPU_SPATIAL_SCRATCH_BUDGET_BYTES)
    }

    /// Create a pool with an explicit retained-byte budget.
    pub fn with_budget(budget_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            budget_bytes,
            retained_bytes: 0,
            peak_textures: 0,
            peak_bytes: 0,
            tick: 0,
        }
    }

    /// Acquire `count` scratch textures for one raster extent.
    ///
    /// The entry grows to `count` and is never shrunk, so repeated frames reuse
    /// the same backend textures. The returned textures are ordered and stable
    /// within an extent; a caller must not sample and render to the same index.
    pub fn acquire(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        count: u32,
    ) -> Vec<wgpu::Texture> {
        let key = (width, height);
        let wanted = usize::try_from(count).unwrap_or(usize::MAX);
        let held = self.entries.get(&key).map_or(0, |entry| entry.textures.len());
        let missing = wanted.saturating_sub(held);
        let mut created = Vec::with_capacity(missing);
        for _ in 0..missing {
            created.push(create_spatial_scratch(device, width, height));
        }
        let tick = self.next_tick();
        let entry = self
            .entries
            .entry(key)
            .or_insert_with(|| GpuSpatialScratchExtent { textures: Vec::new(), last_used: tick });
        entry.last_used = tick;
        entry.textures.append(&mut created);
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(texture_bytes(width, height).saturating_mul(missing as u64));
        self.trim_to_budget(key);
        self.observe_peak();
        self.entries.get(&key).map(|entry| entry.textures.clone()).unwrap_or_default()
    }

    /// Current retained-texture evidence.
    pub fn diagnostics(&self) -> GpuSpatialScratchPoolDiagnostics {
        let retained_textures =
            self.entries.values().map(|entry| entry.textures.len()).sum::<usize>();
        GpuSpatialScratchPoolDiagnostics {
            extents: u32::try_from(self.entries.len()).unwrap_or(u32::MAX),
            retained_textures: u32::try_from(retained_textures).unwrap_or(u32::MAX),
            retained_bytes: self.retained_bytes,
            peak_textures: self.peak_textures,
            peak_bytes: self.peak_bytes,
        }
    }

    /// Drop every retained extent.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.retained_bytes = 0;
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    fn observe_peak(&mut self) {
        let retained = self.entries.values().map(|entry| entry.textures.len()).sum::<usize>();
        self.peak_textures = self.peak_textures.max(u32::try_from(retained).unwrap_or(u32::MAX));
        self.peak_bytes = self.peak_bytes.max(self.retained_bytes);
    }

    fn trim_to_budget(&mut self, keep: (u32, u32)) {
        while self.retained_bytes > self.budget_bytes {
            let victim = self
                .entries
                .iter()
                .filter(|(key, _)| **key != keep)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key);
            let Some(victim) = victim else {
                // The extent that just grew cannot be evicted without breaking
                // the caller that asked for it.
                break;
            };
            if let Some(removed) = self.entries.remove(&victim) {
                let bytes =
                    texture_bytes(victim.0, victim.1).saturating_mul(removed.textures.len() as u64);
                self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
            }
        }
    }
}

/// Bytes one `Rgba32Float` scratch texture occupies at this extent.
fn texture_bytes(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(GPU_SPATIAL_BYTES_PER_PIXEL)
}

fn create_spatial_scratch(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mondrian.effect-spatial.scratch"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: product_gpu_working_texture_format().to_wgpu(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::{
        compile_reference_render_graph, lower_effect_graph_to_gpu_pipeline,
        ChromaticAberrationRemovalGrade, EffectGraphBuilderState, EffectRenderOp,
    };

    fn chromatic_aberration_removal_pipeline() -> CompiledEffectGpuPipeline {
        let grade = ChromaticAberrationRemovalGrade::new(5.0, -5.0, 10.0, [0.0, 1.0]).unwrap();
        let mut graph = EffectGraphBuilderState::new();
        graph.append_unary(EffectRenderOp::ChromaticAberrationRemoval { grade });
        let graph = compile_reference_render_graph(graph.finish()).unwrap();
        lower_effect_graph_to_gpu_pipeline(&graph).unwrap()
    }

    #[test]
    fn spatial_chain_chromatic_aberration_removal_fits_dci_4k() {
        let plan =
            gpu_spatial_chain_plan(&chromatic_aberration_removal_pipeline(), 4096, 2160).unwrap();
        assert_eq!(plan.scratch_bytes, 283_115_520);
        assert_eq!(plan.scratch_textures, 2);
        assert_eq!(plan.spatial_passes, 1);
        assert_eq!(plan.point_stages, 0);
        assert_eq!(plan.stages.len(), 1);
    }

    #[test]
    fn spatial_chain_chromatic_aberration_removal_rejects_an_over_budget_raster() {
        let error = gpu_spatial_chain_plan(&chromatic_aberration_removal_pipeline(), 8192, 2160)
            .unwrap_err();
        assert_eq!(
            error,
            GpuSpatialChainBlocker::ScratchBudgetExceeded {
                required_bytes: 566_231_040,
                budget_bytes: 536_870_912,
            }
        );
        assert_eq!(error.code(), "spatial_scratch_budget_exceeded");
    }

    #[test]
    fn spatial_chain_chromatic_aberration_removal_accepts_the_exact_budget_boundary() {
        let plan =
            gpu_spatial_chain_plan(&chromatic_aberration_removal_pipeline(), 8192, 2048).unwrap();
        assert_eq!(plan.scratch_bytes, 536_870_912);
    }

    #[test]
    fn spatial_chain_chromatic_aberration_removal_checks_device_extent_first() {
        let error = gpu_spatial_chain_plan(&chromatic_aberration_removal_pipeline(), 8193, 2160)
            .unwrap_err();
        assert_eq!(
            error,
            GpuSpatialChainBlocker::ExtentExceedsDeviceLimit {
                width: 8193,
                height: 2160,
                limit: 8192
            }
        );
    }

    #[test]
    fn spatial_chain_chromatic_aberration_removal_reports_the_halo_on_both_axes() {
        let plan =
            gpu_spatial_chain_plan(&chromatic_aberration_removal_pipeline(), 4096, 2160).unwrap();
        assert!(matches!(
            plan.stages.as_slice(),
            [GpuSpatialStagePlan::Spatial {
                pass: EffectGpuSpatialPass::ChromaticAberrationRemoval { .. },
                scratch_textures: 1,
                horizontal_halo: 16,
                vertical_halo: 16,
            }]
        ));
    }

    #[test]
    fn spatial_chain_basic_3d_fits_dci_4k_with_full_frame_halo() {
        let grade = mondrian_effects::Basic3DGrade::new(24.0, -37.0, 20.0, 0.8).unwrap();
        let mut graph = EffectGraphBuilderState::new();
        graph.append_unary(EffectRenderOp::Basic3D { grade });
        let graph = compile_reference_render_graph(graph.finish()).unwrap();
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        let plan = gpu_spatial_chain_plan(&pipeline, 4096, 2160).unwrap();
        assert_eq!(plan.scratch_bytes, 283_115_520);
        assert_eq!(plan.scratch_textures, 2);
        assert_eq!(plan.spatial_passes, 1);
        assert_eq!(plan.point_stages, 0);
        assert!(matches!(
            plan.stages.as_slice(),
            [GpuSpatialStagePlan::Spatial {
                pass: EffectGpuSpatialPass::Basic3D { .. },
                scratch_textures: 1,
                horizontal_halo: u32::MAX,
                vertical_halo: u32::MAX,
            }]
        ));
    }

    #[test]
    fn spatial_chain_camera_view_fits_dci_4k_with_full_frame_halo() {
        let grade = mondrian_effects::CameraViewGrade::new(
            [22.0, -10.0, 16.0],
            580.0,
            2.0,
            110.0,
            [1.0; 4],
            0.5,
        )
        .unwrap();
        let mut graph = EffectGraphBuilderState::new();
        graph.append_unary(EffectRenderOp::CameraView { grade });
        let graph = compile_reference_render_graph(graph.finish()).unwrap();
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        let plan = gpu_spatial_chain_plan(&pipeline, 4096, 2160).unwrap();
        assert_eq!(plan.scratch_bytes, 283_115_520);
        assert_eq!(plan.scratch_textures, 2);
        assert_eq!(plan.spatial_passes, 1);
        assert_eq!(plan.point_stages, 0);
        assert!(matches!(
            plan.stages.as_slice(),
            [GpuSpatialStagePlan::Spatial {
                pass: EffectGpuSpatialPass::CameraView { .. },
                scratch_textures: 1,
                horizontal_halo: u32::MAX,
                vertical_halo: u32::MAX,
            }]
        ));
    }

    #[test]
    fn spatial_chain_blanking_fill_budgets_only_the_textures_its_mode_uses() {
        use mondrian_effects::{BlankingFillGrade, BlankingFillMode, BlankingFillParameters};

        for (mode, passes, scratch_bytes) in [
            (BlankingFillMode::Stretch, 2_u32, 424_673_280),
            (BlankingFillMode::Warp, 1, 283_115_520),
        ] {
            let grade = BlankingFillGrade::new(BlankingFillParameters {
                mode,
                ..BlankingFillParameters::default()
            })
            .unwrap();
            let mut graph = EffectGraphBuilderState::new();
            graph.append_unary(EffectRenderOp::BlankingFill { grade });
            let graph = compile_reference_render_graph(graph.finish()).unwrap();
            let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
            let plan = gpu_spatial_chain_plan(&pipeline, 4096, 2160).unwrap();
            assert_eq!(plan.scratch_bytes, scratch_bytes);
            assert_eq!(plan.scratch_textures, (passes + 1) as usize);
            assert_eq!(plan.spatial_passes, passes);
            assert_eq!(plan.point_stages, 0);
            assert!(matches!(
                plan.stages.as_slice(),
                [GpuSpatialStagePlan::Spatial {
                    pass: EffectGpuSpatialPass::BlankingFill { .. },
                    scratch_textures,
                    horizontal_halo: u32::MAX,
                    vertical_halo: u32::MAX,
                }] if *scratch_textures == passes
            ));
        }
    }
}
