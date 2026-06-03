//! Render Graph IR — compiled intermediate representation for GPU render planning.
//!
//! Architecture V2 Phase 4: The compositor compiles a declarative `RenderGraph`
//! from timeline layers, which the GPU backend executes in a single submission.
//! Pass fusion merges adjacent passes with compatible dimensions and operations.

use crate::types::BlendMode;
use serde::{Deserialize, Serialize};

// ── Render graph types ───────────────────────────────────────────────

/// A compiled render graph — a DAG of GPU passes.
///
/// The `FrameCompositor` produces a `RenderGraph` from timeline layers,
/// then executes it via the wgpu backend. All passes are submitted in
/// a single `queue.submit()` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderGraph {
    /// All passes in topological order (execution order).
    pub passes: Vec<RenderPass>,
    /// Output target — the final pass whose output is the composited frame.
    pub output: Option<RenderPassId>,
    /// Total estimated GPU cost (heuristic, for scheduling).
    pub estimated_cost: u32,
}

/// Unique identifier for a render pass within a `RenderGraph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RenderPassId(pub u32);

/// A single render pass in the compiled graph.
///
/// Each pass reads from some number of input resources and writes to
/// one output resource. The GPU backend translates each pass into
/// draw calls or compute dispatches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderPass {
    /// Unique ID within the graph.
    pub id: RenderPassId,
    /// Human-readable label for debugging.
    pub label: String,
    /// Input resources (textures, buffers) this pass reads from.
    pub inputs: Vec<RenderPassId>,
    /// Output resource this pass writes to.
    pub output: RenderResource,
    /// Pass operation — what this pass actually does.
    pub operation: RenderOperation,
    /// Pass dimensions in pixels.
    pub width: u32,
    pub height: u32,
    /// Whether this pass is eligible for fusion with adjacent passes.
    pub fusible: bool,
}

/// What a render pass actually computes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenderOperation {
    /// Composite a single layer onto an accumulation buffer.
    CompositeLayer {
        /// Index into the layer texture array.
        layer_index: usize,
        opacity: f32,
        blend_mode: BlendMode,
    },
    /// Apply an effect to the input.
    ApplyEffect {
        /// Effect type identifier.
        effect_id: String,
        /// Serialized effect parameters.
        params: serde_json::Value,
    },
    /// Copy/blit — no computation, just data movement.
    Blit,
    /// Clear the output to a solid color.
    Clear { r: f32, g: f32, b: f32, a: f32 },
    /// User-defined custom operation (future extension).
    Custom {
        name: String,
        params: serde_json::Value,
    },
}

/// A GPU resource — texture or buffer — that passes read from or write to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderResource {
    /// Unique resource ID within the graph.
    pub id: RenderResourceId,
    /// Resource kind.
    pub kind: RenderResourceKind,
    /// Dimensions (for textures; ignored for buffers).
    pub width: u32,
    pub height: u32,
    /// Texture format (for textures; ignored for buffers).
    pub format: RenderTextureFormat,
}

/// Unique identifier for a GPU resource within a `RenderGraph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RenderResourceId(pub u32);

/// Kind of GPU resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderResourceKind {
    /// 2D RGBA texture.
    Texture,
    /// GPU buffer (uniform, storage, etc.).
    Buffer { size_bytes: u64 },
}

/// Texture format for render graph resources.
/// Mirrors wgpu::TextureFormat but avoids the wgpu dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RenderTextureFormat {
    Rgba8Unorm,
    Rgba16Float,
    Rgba32Float,
    Bgra8Unorm,
}

/// Dependency between two render passes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassDependency {
    /// The pass that produces the resource.
    pub producer: RenderPassId,
    /// The pass that consumes the resource.
    pub consumer: RenderPassId,
    /// The resource being passed between them.
    pub resource: RenderResourceId,
}

// ── Graph compilation ────────────────────────────────────────────────

/// Configuration for render graph compilation.
#[derive(Debug, Clone)]
pub struct RenderGraphConfig {
    /// Whether to enable pass fusion (merge adjacent compatible passes).
    pub enable_pass_fusion: bool,
    /// Maximum texture size for intermediate resources.
    pub max_intermediate_size: u32,
    /// Whether to insert debug labels into the GPU command stream.
    pub debug_labels: bool,
}

impl Default for RenderGraphConfig {
    fn default() -> Self {
        Self {
            enable_pass_fusion: false, // Phase 4.3 — deferred
            max_intermediate_size: 8192,
            debug_labels: cfg!(debug_assertions),
        }
    }
}

/// Compile a set of layer descriptors into a render graph.
///
/// This is the entry point for the render graph compiler. Currently
/// returns a simple linear graph (one pass per layer). Pass fusion
/// (Phase 4.3) will be implemented here.
pub fn compile_render_graph(
    layer_count: usize,
    output_width: u32,
    output_height: u32,
    config: &RenderGraphConfig,
) -> RenderGraph {
    let mut passes = Vec::with_capacity(layer_count);

    for i in 0..layer_count {
        let pass_id = RenderPassId(i as u32);
        let resource_id = RenderResourceId(i as u32);

        passes.push(RenderPass {
            id: pass_id,
            label: format!("composite_layer_{i}"),
            width: output_width,
            height: output_height,
            inputs: if i == 0 {
                vec![]
            } else {
                vec![RenderPassId(i as u32 - 1)]
            },
            output: RenderResource {
                id: resource_id,
                kind: RenderResourceKind::Texture,
                width: output_width,
                height: output_height,
                format: RenderTextureFormat::Rgba8Unorm,
            },
            operation: RenderOperation::CompositeLayer {
                layer_index: i,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
            },
            fusible: config.enable_pass_fusion && i > 0,
        });
    }

    let output = passes.last().map(|p| p.id);
    let estimated_cost = passes.len() as u32 * output_width * output_height / 1_000_000;

    RenderGraph { passes, output, estimated_cost }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph_has_no_passes() {
        let graph = compile_render_graph(0, 1920, 1080, &RenderGraphConfig::default());
        assert!(graph.passes.is_empty());
        assert!(graph.output.is_none());
    }

    #[test]
    fn single_layer_creates_one_pass() {
        let graph = compile_render_graph(1, 1920, 1080, &RenderGraphConfig::default());
        assert_eq!(graph.passes.len(), 1);
        assert_eq!(graph.output, Some(RenderPassId(0)));
        assert_eq!(graph.passes[0].width, 1920);
        assert_eq!(graph.passes[0].height, 1080);
    }

    #[test]
    fn multi_layer_chain_dependencies() {
        let graph = compile_render_graph(3, 640, 480, &RenderGraphConfig::default());
        assert_eq!(graph.passes.len(), 3);
        // Each pass depends on previous
        assert!(graph.passes[0].inputs.is_empty());
        assert_eq!(graph.passes[1].inputs, vec![RenderPassId(0)]);
        assert_eq!(graph.passes[2].inputs, vec![RenderPassId(1)]);
    }

    #[test]
    fn fusion_disabled_by_default() {
        let graph = compile_render_graph(2, 100, 100, &RenderGraphConfig::default());
        assert!(!graph.passes[0].fusible);
        // Second pass is only fusible when config enables it
        let config = RenderGraphConfig { enable_pass_fusion: true, ..Default::default() };
        let graph = compile_render_graph(2, 100, 100, &config);
        assert!(graph.passes[1].fusible);
    }
}
