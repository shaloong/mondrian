//! Shared command recording for production passes and bounded GPU telemetry.
//!
//! A recorder attaches counters to actual passes. On pass-boundary-only GPUs,
//! a measured one-workgroup pass anchors copies before the first work pass so
//! their execution is included in the frame interval. Trailing copies and
//! incomplete stage boundaries cannot silently become a complete measurement.
//! Recording always continues when telemetry runs out of queries.

use crate::profile::{GpuTimestampStageMarker, GpuTimestampToken};

/// Commands used by shared GPU execution, with optional pass instrumentation.
pub trait GpuCommandEncoder {
    /// Begin the caller's actual render pass.
    fn begin_render_pass<'a>(
        &'a mut self,
        descriptor: &wgpu::RenderPassDescriptor<'_>,
    ) -> wgpu::RenderPass<'a>;

    /// Begin the caller's actual compute pass.
    fn begin_compute_pass<'a>(
        &'a mut self,
        descriptor: &wgpu::ComputePassDescriptor<'_>,
    ) -> wgpu::ComputePass<'a>;

    /// Copy bytes between buffers.
    fn copy_buffer_to_buffer(
        &mut self,
        source: &wgpu::Buffer,
        source_offset: wgpu::BufferAddress,
        destination: &wgpu::Buffer,
        destination_offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    );

    /// Copy a buffer into a texture.
    fn copy_buffer_to_texture(
        &mut self,
        source: wgpu::TexelCopyBufferInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        size: wgpu::Extent3d,
    );

    /// Copy a texture into a buffer.
    fn copy_texture_to_buffer(
        &mut self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyBufferInfo<'_>,
        size: wgpu::Extent3d,
    );

    /// Copy between textures.
    fn copy_texture_to_texture(
        &mut self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        size: wgpu::Extent3d,
    );

    /// Clear a buffer range.
    fn clear_buffer(
        &mut self,
        buffer: &wgpu::Buffer,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    );

    /// Bind a whole-buffer map to completion of the actual recorded submission.
    fn map_buffer_on_submit(
        &self,
        buffer: &wgpu::Buffer,
        mode: wgpu::MapMode,
        callback: Box<dyn FnOnce(Result<(), wgpu::BufferAsyncError>) + Send + 'static>,
    );

    /// Record a semantic stage boundary; plain encoders need no instrumentation.
    fn mark_stage(&mut self, stage: GpuTimestampStageMarker);

    /// Write an encoder counter when that recording mode is supported.
    fn write_timestamp(&mut self, query_set: &wgpu::QuerySet, index: u32) -> Result<(), String>;

    /// Resolve query data in an explicitly completed measurement's encoder.
    fn resolve_query_set(
        &mut self,
        query_set: &wgpu::QuerySet,
        range: std::ops::Range<u32>,
        destination: &wgpu::Buffer,
        offset: wgpu::BufferAddress,
    ) -> Result<(), String>;

    /// Clear selected texture subresources.
    fn clear_texture(&mut self, texture: &wgpu::Texture, range: &wgpu::ImageSubresourceRange);
}

impl GpuCommandEncoder for wgpu::CommandEncoder {
    fn begin_render_pass<'a>(
        &'a mut self,
        descriptor: &wgpu::RenderPassDescriptor<'_>,
    ) -> wgpu::RenderPass<'a> {
        self.begin_render_pass(descriptor)
    }

    fn begin_compute_pass<'a>(
        &'a mut self,
        descriptor: &wgpu::ComputePassDescriptor<'_>,
    ) -> wgpu::ComputePass<'a> {
        self.begin_compute_pass(descriptor)
    }

    fn copy_buffer_to_buffer(
        &mut self,
        source: &wgpu::Buffer,
        source_offset: wgpu::BufferAddress,
        destination: &wgpu::Buffer,
        destination_offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    ) {
        self.copy_buffer_to_buffer(source, source_offset, destination, destination_offset, size);
    }

    fn copy_buffer_to_texture(
        &mut self,
        source: wgpu::TexelCopyBufferInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        size: wgpu::Extent3d,
    ) {
        self.copy_buffer_to_texture(source, destination, size);
    }

    fn copy_texture_to_buffer(
        &mut self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyBufferInfo<'_>,
        size: wgpu::Extent3d,
    ) {
        self.copy_texture_to_buffer(source, destination, size);
    }

    fn copy_texture_to_texture(
        &mut self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        size: wgpu::Extent3d,
    ) {
        self.copy_texture_to_texture(source, destination, size);
    }

    fn clear_buffer(
        &mut self,
        buffer: &wgpu::Buffer,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    ) {
        self.clear_buffer(buffer, offset, size);
    }

    fn map_buffer_on_submit(
        &self,
        buffer: &wgpu::Buffer,
        mode: wgpu::MapMode,
        callback: Box<dyn FnOnce(Result<(), wgpu::BufferAsyncError>) + Send + 'static>,
    ) {
        self.map_buffer_on_submit(buffer, mode, .., callback);
    }

    fn mark_stage(&mut self, _stage: GpuTimestampStageMarker) {}

    fn write_timestamp(&mut self, query_set: &wgpu::QuerySet, index: u32) -> Result<(), String> {
        self.write_timestamp(query_set, index);
        Ok(())
    }

    fn resolve_query_set(
        &mut self,
        query_set: &wgpu::QuerySet,
        range: std::ops::Range<u32>,
        destination: &wgpu::Buffer,
        offset: wgpu::BufferAddress,
    ) -> Result<(), String> {
        self.resolve_query_set(query_set, range, destination, offset);
        Ok(())
    }

    fn clear_texture(&mut self, texture: &wgpu::Texture, range: &wgpu::ImageSubresourceRange) {
        self.clear_texture(texture, range);
    }
}

/// Why recorded commands cannot yield a complete pass-boundary measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum GpuPassTimestampCoverageError {
    /// No real render or compute pass was recorded.
    NoPass,
    /// A copy or clear precedes the first measured boundary.
    LeadingCommands,
    /// A copy or clear follows the last pass.
    TrailingCommands,
    /// A stage ended after commands without a timestamp boundary.
    UnmeasuredStageBoundary,
    /// There were more passes than the bounded query allocation permits.
    QueryCapacity,
    /// A pass already owned its timestamp writes.
    ConflictingTimestampWrites,
    /// Viewer stage markers were missing, duplicated or out of order.
    StageOrder,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuPassTimestampLayout {
    pub query_count: u32,
    pub boundaries: [u32; 8],
}

/// Consuming receipt for one scope; only its originating ring can accept it.
pub struct GpuPassTimestampReceipt {
    pub(crate) token: GpuTimestampToken,
    pub(crate) query_set: wgpu::QuerySet,
    pub(crate) layout: Result<GpuPassTimestampLayout, GpuPassTimestampCoverageError>,
}

impl GpuPassTimestampReceipt {
    /// Whether all recorded work and ordered Viewer stages have GPU boundaries.
    pub fn coverage(&self) -> Result<(), GpuPassTimestampCoverageError> {
        self.layout.map(|_| ())
    }
}

/// Records existing production commands with counters at real pass boundaries.
///
/// The measurement covers only this borrowed scope. Commands recorded before
/// creation or after consuming `finish` belong to a different scope. Complete
/// Viewer callers must wrap their fresh encoder before recording frame work.
pub struct GpuPassTimestampRecorder<'a> {
    encoder: &'a mut wgpu::CommandEncoder,
    query_set: wgpu::QuerySet,
    leading_copy_anchor: GpuLeadingCopyTimestampAnchor,
    token: GpuTimestampToken,
    coverage: PassCoverage,
}

#[derive(Clone)]
pub(crate) struct GpuLeadingCopyTimestampAnchor {
    pipeline: wgpu::ComputePipeline,
    binding: wgpu::BindGroup,
    _output: wgpu::Buffer,
}

impl GpuLeadingCopyTimestampAnchor {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian_gpu_leading_copy_timestamp_anchor"),
            source: wgpu::ShaderSource::Wgsl(
                "@group(0) @binding(0) var<storage, read_write> output: array<u32>;
                 @compute @workgroup_size(1) fn main() { output[0] = 1u; }"
                    .into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mondrian_gpu_leading_copy_timestamp_anchor"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_gpu_leading_copy_timestamp_anchor_output"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_leading_copy_timestamp_anchor_binding"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: output.as_entire_binding() }],
        });
        Self { pipeline, binding, _output: output }
    }
}

struct PassCoverage {
    capacity: u32,
    next_query: u32,
    boundaries: [u32; 8],
    next_stage: u32,
    trailing_commands: bool,
    error: Option<GpuPassTimestampCoverageError>,
}

impl PassCoverage {
    fn new(capacity: u32) -> Self {
        Self {
            capacity,
            next_query: 0,
            boundaries: [0; 8],
            next_stage: 1,
            trailing_commands: false,
            error: None,
        }
    }

    fn reject(&mut self, error: GpuPassTimestampCoverageError) {
        self.error.get_or_insert(error);
    }

    fn pass(&mut self, conflicting: bool) -> Option<(u32, u32)> {
        if conflicting {
            self.reject(GpuPassTimestampCoverageError::ConflictingTimestampWrites);
            return None;
        }
        if self.next_query + 2 > self.capacity {
            self.reject(GpuPassTimestampCoverageError::QueryCapacity);
            return None;
        }
        let start = self.next_query;
        self.next_query += 2;
        self.trailing_commands = false;
        Some((start, start + 1))
    }

    fn unmeasured_command(&mut self) {
        if self.next_query == 0 {
            self.reject(GpuPassTimestampCoverageError::LeadingCommands);
        }
        self.trailing_commands = true;
    }

    fn mark(&mut self, stage: GpuTimestampStageMarker) {
        if stage.query_index() != self.next_stage {
            self.reject(GpuPassTimestampCoverageError::StageOrder);
            return;
        }
        if self.trailing_commands {
            self.reject(GpuPassTimestampCoverageError::UnmeasuredStageBoundary);
        }
        self.boundaries[self.next_stage as usize] = self.next_query.saturating_sub(1);
        self.next_stage += 1;
    }

    fn finish(mut self) -> Result<GpuPassTimestampLayout, GpuPassTimestampCoverageError> {
        if self.next_query == 0 {
            self.reject(GpuPassTimestampCoverageError::NoPass);
        }
        if self.trailing_commands {
            self.reject(GpuPassTimestampCoverageError::TrailingCommands);
        }
        if self.next_stage != 7 {
            self.reject(GpuPassTimestampCoverageError::StageOrder);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        self.boundaries[7] = self.next_query - 1;
        Ok(GpuPassTimestampLayout {
            query_count: self.next_query,
            boundaries: self.boundaries,
        })
    }
}

impl<'a> GpuPassTimestampRecorder<'a> {
    pub(crate) fn new(
        encoder: &'a mut wgpu::CommandEncoder,
        query_set: wgpu::QuerySet,
        leading_copy_anchor: GpuLeadingCopyTimestampAnchor,
        token: GpuTimestampToken,
    ) -> Self {
        let capacity = query_set.count();
        Self {
            encoder,
            query_set,
            leading_copy_anchor,
            token,
            coverage: PassCoverage::new(capacity),
        }
    }

    fn anchor_leading_copy(&mut self) {
        if self.coverage.next_query != 0 {
            return;
        }
        // Metal exposes timestamp writes at pass boundaries on this device.
        // A one-workgroup storage write makes the pass execute on Metal; an
        // empty pass may be elided and leave its beginning query at zero.
        // Subsequent real passes close the interval after the copy.
        // The anchor consumes bounded query capacity, and the ordinary stage
        // coverage checks still reject a copy without a following work pass.
        if let Some((start, end)) = self.coverage.pass(false) {
            let mut pass = self.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mondrian_gpu_leading_copy_timestamp_anchor"),
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: &self.query_set,
                    beginning_of_pass_write_index: Some(start),
                    end_of_pass_write_index: Some(end),
                }),
            });
            pass.set_pipeline(&self.leading_copy_anchor.pipeline);
            pass.set_bind_group(0, &self.leading_copy_anchor.binding, &[]);
            pass.dispatch_workgroups(1, 1, 1);
            drop(pass);
        }
    }

    /// Record an ordered boundary without adding an empty render/compute pass.
    pub fn mark_stage(&mut self, stage: GpuTimestampStageMarker) {
        self.coverage.mark(stage);
    }

    /// End this scope while retaining its query slot until GPU completion.
    pub fn finish(self) -> GpuPassTimestampReceipt {
        GpuPassTimestampReceipt {
            token: self.token,
            query_set: self.query_set,
            layout: self.coverage.finish(),
        }
    }
}

impl GpuCommandEncoder for GpuPassTimestampRecorder<'_> {
    fn begin_render_pass<'a>(
        &'a mut self,
        descriptor: &wgpu::RenderPassDescriptor<'_>,
    ) -> wgpu::RenderPass<'a> {
        let mut measured = descriptor.clone();
        if let Some((start, end)) = self.coverage.pass(descriptor.timestamp_writes.is_some()) {
            measured.timestamp_writes = Some(wgpu::RenderPassTimestampWrites {
                query_set: &self.query_set,
                beginning_of_pass_write_index: Some(start),
                end_of_pass_write_index: Some(end),
            });
        }
        self.encoder.begin_render_pass(&measured)
    }

    fn begin_compute_pass<'a>(
        &'a mut self,
        descriptor: &wgpu::ComputePassDescriptor<'_>,
    ) -> wgpu::ComputePass<'a> {
        let mut measured = descriptor.clone();
        if let Some((start, end)) = self.coverage.pass(descriptor.timestamp_writes.is_some()) {
            measured.timestamp_writes = Some(wgpu::ComputePassTimestampWrites {
                query_set: &self.query_set,
                beginning_of_pass_write_index: Some(start),
                end_of_pass_write_index: Some(end),
            });
        }
        self.encoder.begin_compute_pass(&measured)
    }

    fn copy_buffer_to_buffer(
        &mut self,
        source: &wgpu::Buffer,
        source_offset: wgpu::BufferAddress,
        destination: &wgpu::Buffer,
        destination_offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    ) {
        self.anchor_leading_copy();
        self.coverage.unmeasured_command();
        self.encoder.copy_buffer_to_buffer(
            source,
            source_offset,
            destination,
            destination_offset,
            size,
        );
    }

    fn copy_buffer_to_texture(
        &mut self,
        source: wgpu::TexelCopyBufferInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        size: wgpu::Extent3d,
    ) {
        self.anchor_leading_copy();
        self.coverage.unmeasured_command();
        self.encoder.copy_buffer_to_texture(source, destination, size);
    }

    fn copy_texture_to_buffer(
        &mut self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyBufferInfo<'_>,
        size: wgpu::Extent3d,
    ) {
        self.anchor_leading_copy();
        self.coverage.unmeasured_command();
        self.encoder.copy_texture_to_buffer(source, destination, size);
    }

    fn copy_texture_to_texture(
        &mut self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        size: wgpu::Extent3d,
    ) {
        self.anchor_leading_copy();
        self.coverage.unmeasured_command();
        self.encoder.copy_texture_to_texture(source, destination, size);
    }

    fn clear_buffer(
        &mut self,
        buffer: &wgpu::Buffer,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    ) {
        self.anchor_leading_copy();
        self.coverage.unmeasured_command();
        self.encoder.clear_buffer(buffer, offset, size);
    }

    fn map_buffer_on_submit(
        &self,
        buffer: &wgpu::Buffer,
        mode: wgpu::MapMode,
        callback: Box<dyn FnOnce(Result<(), wgpu::BufferAsyncError>) + Send + 'static>,
    ) {
        self.encoder.map_buffer_on_submit(buffer, mode, .., callback);
    }

    fn mark_stage(&mut self, stage: GpuTimestampStageMarker) {
        self.coverage.mark(stage);
    }

    fn write_timestamp(&mut self, _query_set: &wgpu::QuerySet, _index: u32) -> Result<(), String> {
        self.coverage.reject(GpuPassTimestampCoverageError::ConflictingTimestampWrites);
        Err("encoder timestamp writes are unavailable inside a measured pass scope".to_owned())
    }

    fn resolve_query_set(
        &mut self,
        _query_set: &wgpu::QuerySet,
        _range: std::ops::Range<u32>,
        _destination: &wgpu::Buffer,
        _offset: wgpu::BufferAddress,
    ) -> Result<(), String> {
        self.coverage.reject(GpuPassTimestampCoverageError::ConflictingTimestampWrites);
        Err("query resolution must occur after measured pass scope completion".to_owned())
    }

    fn clear_texture(&mut self, texture: &wgpu::Texture, range: &wgpu::ImageSubresourceRange) {
        self.anchor_leading_copy();
        self.coverage.unmeasured_command();
        self.encoder.clear_texture(texture, range);
    }
}

#[cfg(test)]
mod tests {
    use super::{GpuPassTimestampCoverageError as Error, PassCoverage};
    use crate::profile::GpuTimestampStageMarker;

    fn mark_remaining(coverage: &mut PassCoverage) {
        for marker in [
            GpuTimestampStageMarker::AfterWorkingComposite,
            GpuTimestampStageMarker::AfterSpatial,
            GpuTimestampStageMarker::AfterProgramOutputBoundary,
            GpuTimestampStageMarker::AfterMonitorAdaptation,
            GpuTimestampStageMarker::AfterProgramScopes,
            GpuTimestampStageMarker::AfterSignalMonitoring,
        ] {
            coverage.mark(marker);
        }
    }

    #[test]
    fn coverage_rejects_unmeasured_edges_and_stage_work() {
        let mut leading = PassCoverage::new(8);
        leading.unmeasured_command();
        leading.pass(false);
        mark_remaining(&mut leading);
        assert_eq!(leading.finish().unwrap_err(), Error::LeadingCommands);

        let mut trailing = PassCoverage::new(8);
        trailing.pass(false);
        mark_remaining(&mut trailing);
        trailing.unmeasured_command();
        assert_eq!(trailing.finish().unwrap_err(), Error::TrailingCommands);

        let mut boundary = PassCoverage::new(8);
        boundary.pass(false);
        boundary.unmeasured_command();
        mark_remaining(&mut boundary);
        boundary.pass(false);
        assert_eq!(
            boundary.finish().unwrap_err(),
            Error::UnmeasuredStageBoundary
        );
    }

    #[test]
    fn coverage_preserves_the_span_without_empty_stage_passes() {
        let mut coverage = PassCoverage::new(8);
        coverage.pass(false);
        coverage.unmeasured_command();
        coverage.pass(false);
        mark_remaining(&mut coverage);
        let layout = coverage.finish().unwrap();
        assert_eq!(layout.query_count, 4);
        assert_eq!(layout.boundaries, [0, 3, 3, 3, 3, 3, 3, 3]);
    }

    #[test]
    fn coverage_rejects_missing_passes_order_overflow_and_conflicts() {
        let mut empty = PassCoverage::new(8);
        mark_remaining(&mut empty);
        assert_eq!(empty.finish().unwrap_err(), Error::NoPass);

        let mut missing = PassCoverage::new(8);
        missing.pass(false);
        assert_eq!(missing.finish().unwrap_err(), Error::StageOrder);

        let mut wrong_order = PassCoverage::new(8);
        wrong_order.pass(false);
        wrong_order.mark(GpuTimestampStageMarker::AfterSpatial);
        assert_eq!(wrong_order.finish().unwrap_err(), Error::StageOrder);

        let mut overflow = PassCoverage::new(2);
        overflow.pass(false);
        assert!(overflow.pass(false).is_none());
        mark_remaining(&mut overflow);
        assert_eq!(overflow.finish().unwrap_err(), Error::QueryCapacity);

        let mut conflicting = PassCoverage::new(8);
        assert!(conflicting.pass(true).is_none());
        mark_remaining(&mut conflicting);
        assert_eq!(
            conflicting.finish().unwrap_err(),
            Error::ConflictingTimestampWrites
        );
    }
}
