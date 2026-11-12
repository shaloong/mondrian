//! Bounded compute passes for the bilateral grid. RGB reuses the grid and
//! grouping buffers in command order, then packs directly into the output.

use mondrian_effects::{BilateralBlurGrade, BilateralBlurGrid};
use wgpu::util::DeviceExt as _;

const SLAB: u32 = 8;
const HALO: u32 = 3;
const BLOCK: u32 = 128;
const GROUPS_PER_ROW: u32 = 1024;
const MAX_BINDING: u64 = 128 * 1024 * 1024;
const DISPATCH_BYTES: u64 = 12;
const DISPATCHES_PER_SLAB: u64 = 5;

#[derive(Clone, Copy)]
pub(crate) struct BilateralGpuPlan {
    grid: BilateralBlurGrid,
    pixels: u32,
    blocks: u32,
    pages: u32,
    slabs: u32,
    channels: u64,
    filtering: bool,
    pub(crate) working_bytes: u64,
    pub(crate) scratch_bytes: u64,
    largest_binding: u64,
}

impl BilateralGpuPlan {
    pub(crate) fn new(grade: BilateralBlurGrade, width: u32, height: u32) -> Option<Self> {
        let pixels = width.checked_mul(height).filter(|&n| n > 0)?;
        let grid = grade.grid(width, height);
        let blocks = pixels.div_ceil(BLOCK);
        let pages = blocks.div_ceil(256);
        let slabs = grid.dimensions[2].div_ceil(SLAB);
        let channels = if grade.colorize() { 3 } else { 1 };
        let mut plan = Self {
            grid,
            pixels,
            blocks,
            pages,
            slabs,
            channels,
            filtering: grade.filters_neighbors(),
            working_bytes: 0,
            scratch_bytes: 0,
            largest_binding: 0,
        };
        let mut bindings = vec![plan.plane_bytes(); channels as usize];
        if plan.filtering {
            bindings.extend([
                plan.plane_bytes(),
                plan.plane_bytes(),
                plan.prefix_bytes(),
                plan.page_bytes(),
                plan.starts_bytes(),
                plan.grid_bytes(),
                plan.grid_bytes(),
                plan.indirect_bytes(),
                u64::from(slabs) * 4,
            ]);
        }
        plan.largest_binding = *bindings.iter().max()?;
        // Charge the actual source and destination. Uniform buffers are separate
        // allocations, so reserve their 256-byte GPU binding alignment.
        plan.scratch_bytes = bindings.into_iter().sum::<u64>()
            + channels * (1 + if plan.filtering { u64::from(slabs) } else { 0 }) * 256
            + if plan.filtering { 256 } else { 0 };
        plan.working_bytes = plan.scratch_bytes + u64::from(pixels) * 32;
        Some(plan)
    }

    pub(crate) fn fits(self, binding_limit: u64) -> bool {
        self.largest_binding <= binding_limit.min(MAX_BINDING)
    }

    fn plane_bytes(self) -> u64 {
        u64::from(self.pixels) * 4
    }
    fn prefix_bytes(self) -> u64 {
        4 * u64::from(self.blocks) * u64::from(self.slabs)
    }
    fn page_bytes(self) -> u64 {
        4 * u64::from(self.pages) * u64::from(self.slabs)
    }
    fn starts_bytes(self) -> u64 {
        u64::from(self.slabs + 1) * 4
    }
    fn indirect_bytes(self) -> u64 {
        u64::from(self.slabs) * DISPATCH_BYTES * DISPATCHES_PER_SLAB
    }
    fn grid_bytes(self) -> u64 {
        let [x, y, z] = self.grid.dimensions;
        u64::from(x) * u64::from(y) * u64::from(z.min(15)) * 8
    }
}

fn buffer_type(ty: wgpu::BufferBindingType) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn storage(read_only: bool) -> wgpu::BindingType {
    buffer_type(wgpu::BufferBindingType::Storage { read_only })
}

fn sampled() -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable: false },
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    }
}

fn layout(
    device: &wgpu::Device,
    label: &str,
    visibility: wgpu::ShaderStages,
    bindings: &[(u32, wgpu::BindingType)],
) -> wgpu::BindGroupLayout {
    let entries: Vec<_> = bindings
        .iter()
        .map(|&(binding, ty)| wgpu::BindGroupLayoutEntry { binding, visibility, ty, count: None })
        .collect();
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    })
}

fn bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffers: &[(u32, &wgpu::Buffer)],
    source: Option<(u32, &wgpu::TextureView)>,
) -> wgpu::BindGroup {
    let mut entries: Vec<_> = buffers
        .iter()
        .map(|&(binding, buffer)| wgpu::BindGroupEntry {
            binding,
            resource: buffer.as_entire_binding(),
        })
        .collect();
    if let Some((binding, view)) = source {
        entries.push(wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bilateral-grid"),
        layout,
        entries: &entries,
    })
}

struct Kernel {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl Kernel {
    fn new(
        device: &wgpu::Device,
        module: &wgpu::ShaderModule,
        entry: &str,
        bindings: &[(u32, wgpu::BindingType)],
    ) -> Self {
        let layout = layout(device, entry, wgpu::ShaderStages::COMPUTE, bindings);
        let pipeline = compute_pipeline(device, module, entry, &layout);
        Self { layout, pipeline }
    }

    fn bind(
        &self,
        device: &wgpu::Device,
        buffers: &[(u32, &wgpu::Buffer)],
        source: Option<(u32, &wgpu::TextureView)>,
    ) -> wgpu::BindGroup {
        bind(device, &self.layout, buffers, source)
    }

    fn record(&self, pass: &mut wgpu::ComputePass<'_>, binding: &wgpu::BindGroup) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, binding, &[]);
    }
}

fn compute_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    entry: &str,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::ComputePipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(entry),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry),
        layout: Some(&pipeline_layout),
        module,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    })
}

pub(crate) struct BilateralGpu {
    classify: Kernel,
    plan: Kernel,
    gather: Kernel,
    xy: Kernel,
    z: Kernel,
    slice: Kernel,
    seed: Kernel,
    group_layout: wgpu::BindGroupLayout,
    group: [wgpu::ComputePipeline; 5],
    pack_layout: wgpu::BindGroupLayout,
    pack: wgpu::RenderPipeline,
}

impl BilateralGpu {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = |label, source: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        };
        let signal = include_str!("bilateral_gpu/signal.wgsl");
        let splat = shader(
            "bilateral splat",
            &format!("{signal}\n{}", include_str!("bilateral_gpu/splat.wgsl")),
        );
        let filter = shader(
            "bilateral filter",
            &format!("{signal}\n{}", include_str!("bilateral_gpu/filter.wgsl")),
        );
        let group_shader = shader("bilateral group", include_str!("bilateral_gpu/group.wgsl"));
        let uniform = (0, buffer_type(wgpu::BufferBindingType::Uniform));
        let group_layout = layout(
            device,
            "bilateral group",
            wgpu::ShaderStages::COMPUTE,
            &[
                uniform,
                (1, storage(true)),
                (2, storage(false)),
                (3, storage(false)),
                (4, storage(false)),
                (5, storage(false)),
            ],
        );
        let group = [
            "classify_blocks",
            "scan_block_pages",
            "scan_pages",
            "scan_slabs",
            "scatter_stable",
        ]
        .map(|entry| compute_pipeline(device, &group_shader, entry, &group_layout));
        let pack_layout = layout(
            device,
            "bilateral pack",
            wgpu::ShaderStages::FRAGMENT,
            &[
                uniform,
                (3, sampled()),
                (12, storage(true)),
                (13, storage(true)),
                (15, storage(true)),
            ],
        );
        let pack_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bilateral pack"),
            bind_group_layouts: &[Some(&pack_layout)],
            immediate_size: 0,
        });
        let pack = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bilateral pack"),
            layout: Some(&pack_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &filter,
                entry_point: Some("pack_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &filter,
                entry_point: Some("pack_fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            classify: Kernel::new(
                device,
                &splat,
                "classify_samples",
                &[uniform, (1, sampled()), (5, storage(false))],
            ),
            plan: Kernel::new(
                device,
                &filter,
                "plan_windows",
                &[
                    uniform,
                    (5, storage(true)),
                    (7, storage(false)),
                    (10, storage(false)),
                ],
            ),
            gather: Kernel::new(
                device,
                &splat,
                "gather_columns",
                &[
                    uniform,
                    (1, sampled()),
                    (2, storage(true)),
                    (3, storage(true)),
                    (4, storage(false)),
                    (6, storage(true)),
                    (7, storage(true)),
                    (8, storage(true)),
                    (9, storage(true)),
                ],
            ),
            xy: Kernel::new(
                device,
                &filter,
                "blur_xy_tile",
                &[
                    uniform,
                    (1, storage(true)),
                    (2, storage(false)),
                    (11, storage(true)),
                ],
            ),
            z: Kernel::new(
                device,
                &filter,
                "blur_ring_z",
                &[uniform, (1, storage(true)), (2, storage(false))],
            ),
            slice: Kernel::new(
                device,
                &filter,
                "slice_window",
                &[
                    uniform,
                    (1, storage(true)),
                    (3, sampled()),
                    (4, storage(false)),
                    (5, storage(true)),
                    (6, storage(true)),
                    (9, storage(true)),
                ],
            ),
            seed: Kernel::new(
                device,
                &filter,
                "seed_output",
                &[uniform, (3, sampled()), (4, storage(false))],
            ),
            group_layout,
            group,
            pack_layout,
            pack,
        }
    }

    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        source: &wgpu::Texture,
        output: &wgpu::Texture,
        grade: BilateralBlurGrade,
        plan: BilateralGpuPlan,
    ) {
        let source_view = source.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let planes: Vec<_> = (0..plan.channels)
            .map(|_| storage_buffer(device, plan.plane_bytes(), wgpu::BufferUsages::empty()))
            .collect();
        let workspace = plan.filtering.then(|| Workspace::new(device, plan, &self.group_layout));
        let first_channel = if grade.colorize() { 0 } else { 3 };
        let initial = settings(
            source.width(),
            source.height(),
            plan.grid,
            first_channel,
            [0; 3],
        );
        let first_uniform = uniform(device, &initial);
        for (index, plane) in planes.iter().enumerate() {
            let mut config = initial;
            config[7] = first_channel + index as u32;
            let channel_uniform = if index == 0 {
                first_uniform.clone()
            } else {
                uniform(device, &config)
            };
            let seed = self.seed.bind(
                device,
                &[(0, &channel_uniform), (4, plane)],
                Some((3, &source_view)),
            );
            if let Some(workspace) = &workspace {
                encoder.clear_buffer(&workspace.prefix, 0, None);
                let mut pass = encoder.begin_compute_pass(&Default::default());
                let classify = self.classify.bind(
                    device,
                    &[(0, &channel_uniform), (5, &workspace.guide)],
                    Some((1, &source_view)),
                );
                self.classify.record(&mut pass, &classify);
                dispatch(&mut pass, plan.pixels);
                let groups_x = plan.blocks.min(8192);
                let group_sizes = [
                    (groups_x, plan.blocks.div_ceil(groups_x)),
                    (plan.pages, plan.slabs),
                    (plan.slabs.div_ceil(32), 1),
                    (1, 1),
                    (groups_x, plan.blocks.div_ceil(groups_x)),
                ];
                for (pipeline, (x, y)) in self.group.iter().zip(group_sizes) {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &workspace.group, &[]);
                    pass.dispatch_workgroups(x, y, 1);
                }
                let schedule = self.plan.bind(
                    device,
                    &[
                        (0, &channel_uniform),
                        (5, &workspace.starts),
                        (7, &workspace.indirect),
                        (10, &workspace.fresh),
                    ],
                    None,
                );
                self.plan.record(&mut pass, &schedule);
                dispatch(&mut pass, plan.slabs);
                self.seed.record(&mut pass, &seed);
                dispatch(&mut pass, plan.pixels);
                self.record_windows(
                    device,
                    &mut pass,
                    &source_view,
                    plane,
                    workspace,
                    plan,
                    config,
                );
            } else {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                self.seed.record(&mut pass, &seed);
                dispatch(&mut pass, plan.pixels);
            }
        }
        let green = planes.get(1).unwrap_or(&planes[0]);
        let blue = planes.get(2).unwrap_or(&planes[0]);
        let binding = bind(
            device,
            &self.pack_layout,
            &[
                (0, &first_uniform),
                (12, green),
                (13, blue),
                (15, &planes[0]),
            ],
            Some((3, &source_view)),
        );
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bilateral pack"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pack);
        pass.set_bind_group(0, &binding, &[]);
        pass.draw(0..3, 0..1);
    }

    fn record_windows(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::ComputePass<'_>,
        source: &wgpu::TextureView,
        output: &wgpu::Buffer,
        ws: &Workspace,
        plan: BilateralGpuPlan,
        mut config: [u32; 24],
    ) {
        for slab in 0..plan.slabs {
            let first = (slab * SLAB).saturating_sub(HALO);
            let end = ((slab + 1) * SLAB + 1 + HALO).min(plan.grid.dimensions[2]);
            config[12..15].copy_from_slice(&[first, end - first, slab]);
            let uniform = uniform(device, &config);
            let offset = u64::from(slab) * DISPATCHES_PER_SLAB * DISPATCH_BYTES;
            let gather = self.gather.bind(
                device,
                &[
                    (0, &uniform),
                    (2, &ws.starts),
                    (3, &ws.ids),
                    (4, &ws.moments),
                    (6, &ws.prefix),
                    (7, &ws.pages),
                    (8, &ws.guide),
                    (9, &ws.fresh),
                ],
                Some((1, source)),
            );
            let xy = self.xy.bind(
                device,
                &[
                    (0, &uniform),
                    (1, &ws.moments),
                    (2, &ws.ring),
                    (11, &ws.fresh),
                ],
                None,
            );
            let z = self.z.bind(
                device,
                &[(0, &uniform), (1, &ws.ring), (2, &ws.moments)],
                None,
            );
            let slice = self.slice.bind(
                device,
                &[
                    (0, &uniform),
                    (1, &ws.moments),
                    (4, output),
                    (5, &ws.starts),
                    (6, &ws.ids),
                    (9, &ws.guide),
                ],
                Some((3, source)),
            );
            for (kernel, binding, command) in [
                (&self.gather, &gather, 2),
                (&self.xy, &xy, 4),
                (&self.z, &z, 3),
                (&self.slice, &slice, 1),
            ] {
                kernel.record(pass, binding);
                pass.dispatch_workgroups_indirect(&ws.indirect, offset + command * DISPATCH_BYTES);
            }
        }
    }
}

fn dispatch(pass: &mut wgpu::ComputePass<'_>, invocations: u32) {
    let groups = invocations.div_ceil(64);
    pass.dispatch_workgroups(
        groups.min(GROUPS_PER_ROW),
        groups.div_ceil(GROUPS_PER_ROW),
        1,
    );
}

fn uniform(device: &wgpu::Device, values: &[u32]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bilateral controls"),
        contents: bytemuck::cast_slice(values),
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

fn settings(
    width: u32,
    height: u32,
    grid: BilateralBlurGrid,
    channel: u32,
    window: [u32; 3],
) -> [u32; 24] {
    let [x, y, z] = grid.dimensions;
    let mut data = [0; 24];
    data[..8].copy_from_slice(&[width, height, x, y, z, SLAB, 0, channel]);
    data[8..12].copy_from_slice(&grid.scale.map(f32::to_bits));
    data[12..15].copy_from_slice(&window);
    data[16..20].copy_from_slice(&grid.luma.map(f32::to_bits));
    data[20..24].copy_from_slice(&grid.spatial_weights.map(f32::to_bits));
    data
}

fn storage_buffer(device: &wgpu::Device, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bilateral workspace"),
        size,
        usage: wgpu::BufferUsages::STORAGE | usage,
        mapped_at_creation: false,
    })
}

struct Workspace {
    guide: wgpu::Buffer,
    prefix: wgpu::Buffer,
    pages: wgpu::Buffer,
    starts: wgpu::Buffer,
    ids: wgpu::Buffer,
    fresh: wgpu::Buffer,
    indirect: wgpu::Buffer,
    moments: wgpu::Buffer,
    ring: wgpu::Buffer,
    group: wgpu::BindGroup,
}

impl Workspace {
    fn new(
        device: &wgpu::Device,
        plan: BilateralGpuPlan,
        group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let buffer = |size| storage_buffer(device, size, wgpu::BufferUsages::empty());
        let guide = buffer(plan.plane_bytes());
        let prefix = storage_buffer(device, plan.prefix_bytes(), wgpu::BufferUsages::COPY_DST);
        let pages = buffer(plan.page_bytes());
        let starts = buffer(plan.starts_bytes());
        let ids = buffer(plan.plane_bytes());
        let group_config = uniform(device, &[plan.pixels, plan.blocks, 0, plan.slabs]);
        let group = bind(
            device,
            group_layout,
            &[
                (0, &group_config),
                (1, &guide),
                (2, &prefix),
                (3, &starts),
                (4, &ids),
                (5, &pages),
            ],
            None,
        );
        Self {
            guide,
            prefix,
            pages,
            starts,
            ids,
            fresh: buffer(u64::from(plan.slabs) * 4),
            indirect: storage_buffer(device, plan.indirect_bytes(), wgpu::BufferUsages::INDIRECT),
            moments: buffer(plan.grid_bytes()),
            ring: buffer(plan.grid_bytes()),
            group,
        }
    }
}
