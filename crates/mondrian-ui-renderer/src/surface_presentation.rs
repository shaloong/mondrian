//! Surface-presentation carrier for the self-hosted UI.
//!
//! UI commands are authored as sRGB and enter the GPU as linear-sRGB values.
//! An encoded Viewer texture, by contrast, already carries the code values of
//! the selected monitor target. Wide-gamut and HDR windows therefore render
//! both payloads into one target-primary, display-linear `Rgba16Float`
//! composition before a final cached carrier pass encodes the swapchain.

const COMPOSITION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Validated color carrier used by one configured native surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiSurfacePresentation {
    surface_format: wgpu::TextureFormat,
    surface_color_space: Option<wgpu::SurfaceColorSpace>,
    path: UiSurfacePresentationPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiSurfacePresentationPath {
    /// Compatibility path for renderer-only offscreen attachments.
    LinearAttachment,
    /// Ordinary sRGB surface; the attachment performs the final OETF.
    DirectSrgb,
    /// Linear Display P3 composition followed by an sRGB-transfer surface.
    DisplayP3,
    /// Linear Rec.2020 composition in 100-nit units followed by ST 2084.
    Rec2100Pq,
    /// Linear Rec.2020 composition in 100-nit units followed by HLG OETF/OOTF.
    Rec2100Hlg,
}

/// A native surface cannot carry the requested UI/Viewer presentation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiSurfacePresentationError {
    /// The selected texture format and surface color space are not one of the
    /// qualified carrier pairs.
    UnsupportedSurfaceContract {
        /// Configured swapchain texture format.
        surface_format: wgpu::TextureFormat,
        /// Configured swapchain color space.
        surface_color_space: wgpu::SurfaceColorSpace,
    },
}

impl std::fmt::Display for UiSurfacePresentationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSurfaceContract { surface_format, surface_color_space } => write!(
                formatter,
                "UI surface carrier does not support {surface_format:?} / {surface_color_space:?}"
            ),
        }
    }
}

impl std::error::Error for UiSurfacePresentationError {}

impl UiSurfacePresentation {
    /// Validate a production native-surface carrier.
    pub fn for_surface(
        surface_format: wgpu::TextureFormat,
        surface_color_space: wgpu::SurfaceColorSpace,
    ) -> Result<Self, UiSurfacePresentationError> {
        let path = match surface_color_space {
            wgpu::SurfaceColorSpace::Srgb if surface_format.is_srgb() => {
                UiSurfacePresentationPath::DirectSrgb
            }
            wgpu::SurfaceColorSpace::DisplayP3 if surface_format.is_srgb() => {
                UiSurfacePresentationPath::DisplayP3
            }
            wgpu::SurfaceColorSpace::Bt2100Pq if is_hdr_surface_format(surface_format) => {
                UiSurfacePresentationPath::Rec2100Pq
            }
            wgpu::SurfaceColorSpace::Bt2100Hlg if is_hdr_surface_format(surface_format) => {
                UiSurfacePresentationPath::Rec2100Hlg
            }
            _ => {
                return Err(UiSurfacePresentationError::UnsupportedSurfaceContract {
                    surface_format,
                    surface_color_space,
                });
            }
        };
        Ok(Self {
            surface_format,
            surface_color_space: Some(surface_color_space),
            path,
        })
    }

    pub(crate) fn legacy_for_attachment(surface_format: wgpu::TextureFormat) -> Self {
        if surface_format.is_srgb() {
            Self {
                surface_format,
                surface_color_space: Some(wgpu::SurfaceColorSpace::Srgb),
                path: UiSurfacePresentationPath::DirectSrgb,
            }
        } else {
            Self {
                surface_format,
                surface_color_space: None,
                path: UiSurfacePresentationPath::LinearAttachment,
            }
        }
    }

    /// Configured native attachment format.
    pub fn surface_format(self) -> wgpu::TextureFormat {
        self.surface_format
    }

    /// Configured native surface color space, absent only for renderer tests.
    pub fn surface_color_space(self) -> Option<wgpu::SurfaceColorSpace> {
        self.surface_color_space
    }

    pub(crate) fn ui_attachment_format(self) -> wgpu::TextureFormat {
        if self.requires_carrier() {
            COMPOSITION_FORMAT
        } else {
            self.surface_format
        }
    }

    pub(crate) fn requires_carrier(self) -> bool {
        matches!(
            self.path,
            UiSurfacePresentationPath::DisplayP3
                | UiSurfacePresentationPath::Rec2100Pq
                | UiSurfacePresentationPath::Rec2100Hlg
        )
    }

    pub(crate) fn supports_surface_code_values(self) -> bool {
        !matches!(self.path, UiSurfacePresentationPath::LinearAttachment)
    }

    pub(crate) fn supports_device_code_values(self) -> bool {
        matches!(self.path, UiSurfacePresentationPath::DirectSrgb)
    }

    pub(crate) fn ui_to_surface_rows(self) -> [[f32; 4]; 3] {
        let rows = match self.path {
            UiSurfacePresentationPath::LinearAttachment | UiSurfacePresentationPath::DirectSrgb => {
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            }
            UiSurfacePresentationPath::DisplayP3 => [
                [0.822_592_9, 0.177_533_9, 0.0],
                [0.033_199_5, 0.966_783_5, 0.0],
                [0.017_085_3, 0.072_395_7, 0.910_301_5],
            ],
            UiSurfacePresentationPath::Rec2100Pq | UiSurfacePresentationPath::Rec2100Hlg => [
                [0.627_403_9, 0.329_283, 0.043_313_1],
                [0.069_097_3, 0.919_540_4, 0.011_362_3],
                [0.016_391_4, 0.088_013_3, 0.895_595_3],
            ],
        };
        rows.map(|row| [row[0], row[1], row[2], 0.0])
    }

    pub(crate) fn external_decode_mode(self) -> u32 {
        match self.path {
            UiSurfacePresentationPath::LinearAttachment => 3,
            UiSurfacePresentationPath::DirectSrgb | UiSurfacePresentationPath::DisplayP3 => 0,
            UiSurfacePresentationPath::Rec2100Pq => 1,
            UiSurfacePresentationPath::Rec2100Hlg => 2,
        }
    }

    fn carrier_fragment_entry(self) -> Option<&'static str> {
        match self.path {
            UiSurfacePresentationPath::DisplayP3 => Some("fs_linear"),
            UiSurfacePresentationPath::Rec2100Pq => Some("fs_pq"),
            UiSurfacePresentationPath::Rec2100Hlg => Some("fs_hlg"),
            UiSurfacePresentationPath::LinearAttachment | UiSurfacePresentationPath::DirectSrgb => {
                None
            }
        }
    }
}

fn is_hdr_surface_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgb10a2Unorm
    )
}

struct UiSurfaceCompositionTarget {
    size: (u32, u32),
    _composition_texture: wgpu::Texture,
    composition_view: wgpu::TextureView,
    _msaa_texture: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    carrier_bind_group: wgpu::BindGroup,
}

/// Cached wide-gamut/HDR composition resources and final surface carrier.
pub(crate) struct UiSurfaceCarrier {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    target: Option<UiSurfaceCompositionTarget>,
}

impl UiSurfaceCarrier {
    pub(crate) fn new(device: &wgpu::Device, presentation: UiSurfacePresentation) -> Option<Self> {
        let fragment_entry = presentation.carrier_fragment_entry()?;
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui_surface_carrier_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui_surface_carrier_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui_surface_carrier_shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/ui_surface_carrier.wgsl").into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui_surface_carrier_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fragment_entry),
                targets: &[Some(wgpu::ColorTargetState {
                    format: presentation.surface_format(),
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Some(Self { bind_group_layout, pipeline, target: None })
    }

    pub(crate) fn ensure_target(
        &mut self,
        device: &wgpu::Device,
        size: (u32, u32),
        sample_count: u32,
    ) -> bool {
        if size.0 == 0 || size.1 == 0 {
            self.target = None;
            return false;
        }
        if self.target.as_ref().is_some_and(|target| target.size == size) {
            return false;
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let composition_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui_surface_linear_composition"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COMPOSITION_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let composition_view =
            composition_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let msaa_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui_surface_linear_msaa"),
            size: extent,
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: COMPOSITION_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let msaa_view = msaa_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let carrier_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_surface_carrier_bg"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&composition_view),
            }],
        });
        self.target = Some(UiSurfaceCompositionTarget {
            size,
            _composition_texture: composition_texture,
            composition_view,
            _msaa_texture: msaa_texture,
            msaa_view,
            carrier_bind_group,
        });
        true
    }

    pub(crate) fn attachment_views(&self) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        self.target.as_ref().map(|target| (&target.msaa_view, &target.composition_view))
    }

    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        surface_view: &wgpu::TextureView,
    ) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ui_surface_carrier_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: surface_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &target.carrier_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrier_shader_parses_and_validates() {
        let source = include_str!("../shaders/ui_surface_carrier.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("carrier WGSL should parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("carrier WGSL should validate");
    }

    #[test]
    fn production_surface_pairs_are_closed_and_typed() {
        for (format, color_space, carrier) in [
            (
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::SurfaceColorSpace::Srgb,
                false,
            ),
            (
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::SurfaceColorSpace::DisplayP3,
                true,
            ),
            (
                wgpu::TextureFormat::Rgba16Float,
                wgpu::SurfaceColorSpace::Bt2100Pq,
                true,
            ),
            (
                wgpu::TextureFormat::Rgb10a2Unorm,
                wgpu::SurfaceColorSpace::Bt2100Hlg,
                true,
            ),
        ] {
            let presentation =
                UiSurfacePresentation::for_surface(format, color_space).expect("surface pair");
            assert_eq!(presentation.requires_carrier(), carrier);
            assert_eq!(presentation.surface_format(), format);
            assert_eq!(presentation.surface_color_space(), Some(color_space));
        }
    }

    #[test]
    fn mismatched_surface_transfer_and_storage_fail_closed() {
        for (format, color_space) in [
            (
                wgpu::TextureFormat::Rgba16Float,
                wgpu::SurfaceColorSpace::DisplayP3,
            ),
            (
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::SurfaceColorSpace::Bt2100Pq,
            ),
            (
                wgpu::TextureFormat::Rgb10a2Unorm,
                wgpu::SurfaceColorSpace::Srgb,
            ),
        ] {
            assert!(UiSurfacePresentation::for_surface(format, color_space).is_err());
        }
    }

    #[test]
    fn ui_primary_conversion_preserves_neutral_axis() {
        for presentation in [
            UiSurfacePresentation::for_surface(
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::SurfaceColorSpace::DisplayP3,
            )
            .unwrap(),
            UiSurfacePresentation::for_surface(
                wgpu::TextureFormat::Rgba16Float,
                wgpu::SurfaceColorSpace::Bt2100Pq,
            )
            .unwrap(),
        ] {
            let rows = presentation.ui_to_surface_rows();
            for row in rows {
                assert!((row[0] + row[1] + row[2] - 1.0).abs() < 0.000_5);
            }
        }
    }
}
