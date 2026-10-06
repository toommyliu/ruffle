use crate::blend::{ComplexBlend, DirectBlend, TrivialBlend};
use crate::layouts::BindLayouts;
use crate::shaders::Shaders;
use crate::{MaskState, PosColorVertex, PosUvVertex, PosVertex};
use enum_map::{EnumMap, enum_map};
use std::sync::OnceLock;
use wgpu::{BlendState, PrimitiveTopology, vertex_attr_array};

pub const VERTEX_BUFFERS_DESCRIPTION_POS: [Option<wgpu::VertexBufferLayout>; 1] =
    [Some(wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<PosVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vertex_attr_array![
            0 => Float32x2,
        ],
    })];

pub const VERTEX_BUFFERS_DESCRIPTION_POS_UV: [Option<wgpu::VertexBufferLayout>; 1] =
    [Some(wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<PosUvVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vertex_attr_array![
            0 => Float32x2,
            1 => Float32x3,
        ],
    })];

pub const VERTEX_BUFFERS_DESCRIPTION_COLOR: [Option<wgpu::VertexBufferLayout>; 1] =
    [Some(wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<PosColorVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vertex_attr_array![
            0 => Float32x2,
            1 => Unorm8x4,
        ],
    })];

#[derive(Debug)]
pub struct ShapePipeline {
    name: String,
    device: wgpu::Device,
    layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    msaa_sample_count: u32,
    vertex_buffers: &'static [Option<wgpu::VertexBufferLayout<'static>>],
    blend: BlendState,
    primitive_topology: PrimitiveTopology,
    pipelines: EnumMap<MaskState, OnceLock<wgpu::RenderPipeline>>,
    stencilless: OnceLock<wgpu::RenderPipeline>,
}

#[derive(Debug)]
pub struct Pipelines {
    color: EnumMap<TrivialBlend, ShapePipeline>,
    pub lines: ShapePipeline,
    /// Renders a bitmap without any blending, and does
    /// not write to the alpha channel. This is used for
    /// drawing a finished Stage3D buffer onto the background.
    pub bitmap_opaque: wgpu::RenderPipeline,
    /// Like `bitmap_opaque`, but with a no-op `DepthStencilState`.
    /// This is used when we're inside a `RenderPass` that is
    /// using a stencil buffer, but we don't want to write to it
    /// or use it in any way.
    pub bitmap_opaque_dummy_stencil: wgpu::RenderPipeline,
    bitmap: EnumMap<TrivialBlend, ShapePipeline>,
    gradients: EnumMap<TrivialBlend, ShapePipeline>,
    pub complex_blends: EnumMap<ComplexBlend, ShapePipeline>,
    pub alpha_mask: ShapePipeline,
    /// With dual-source blending, the whole blend; without it, the first of
    /// two draws.
    multiply: MultiplyPipelines,
    multiply_second_step: Option<MultiplyPipelines>,
    complex_direct: MultiplyPipelines,
}

#[derive(Debug)]
pub struct MultiplyPipelines {
    pub color: ShapePipeline,
    pub gradient: ShapePipeline,
    pub bitmap: ShapePipeline,
}

impl ShapePipeline {
    pub fn pipeline_for(&self, mask_state: MaskState) -> &wgpu::RenderPipeline {
        self.pipelines[mask_state].get_or_init(|| self.create(Some(mask_state)))
    }

    pub fn stencilless_pipeline(&self) -> &wgpu::RenderPipeline {
        self.stencilless.get_or_init(|| self.create(None))
    }

    fn create(&self, mask_state: Option<MaskState>) -> wgpu::RenderPipeline {
        let (depth_stencil, write_mask) = match mask_state {
            None => (None, wgpu::ColorWrites::ALL),
            Some(mask_state) => {
                let (face, write_mask) = mask_stencil_state(mask_state);
                let depth_stencil = wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Stencil8,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState {
                        front: face,
                        back: face,
                        read_mask: !0,
                        write_mask: !0,
                    },
                    bias: Default::default(),
                };
                (Some(depth_stencil), write_mask)
            }
        };
        self.device
            .create_render_pipeline(&create_pipeline_descriptor(
                create_debug_label!("{} pipeline {:?}", self.name, mask_state).as_deref(),
                &self.shader,
                &self.shader,
                &self.layout,
                depth_stencil,
                &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: Some(self.blend),
                    write_mask,
                })],
                self.vertex_buffers,
                self.msaa_sample_count,
                &[],
                self.primitive_topology,
            ))
    }
}

fn mask_stencil_state(mask_state: MaskState) -> (wgpu::StencilFaceState, wgpu::ColorWrites) {
    let face = |compare, pass_op| wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op,
    };
    match mask_state {
        MaskState::NoMask => (
            face(wgpu::CompareFunction::Always, wgpu::StencilOperation::Keep),
            wgpu::ColorWrites::ALL,
        ),
        MaskState::DrawMaskStencil => (
            face(
                wgpu::CompareFunction::Equal,
                wgpu::StencilOperation::IncrementClamp,
            ),
            wgpu::ColorWrites::empty(),
        ),
        MaskState::DrawMaskedContent => (
            face(wgpu::CompareFunction::Equal, wgpu::StencilOperation::Keep),
            wgpu::ColorWrites::ALL,
        ),
        MaskState::ClearMaskStencil => (
            face(
                wgpu::CompareFunction::Equal,
                wgpu::StencilOperation::DecrementClamp,
            ),
            wgpu::ColorWrites::empty(),
        ),
    }
}

impl Pipelines {
    pub fn color(&self, blend: DirectBlend) -> &ShapePipeline {
        match blend {
            DirectBlend::Trivial(blend) => &self.color[blend],
            DirectBlend::Multiply => &self.multiply.color,
            DirectBlend::Complex(_) => &self.complex_direct.color,
        }
    }

    pub fn gradient(&self, blend: DirectBlend) -> &ShapePipeline {
        match blend {
            DirectBlend::Trivial(blend) => &self.gradients[blend],
            DirectBlend::Multiply => &self.multiply.gradient,
            DirectBlend::Complex(_) => &self.complex_direct.gradient,
        }
    }

    pub fn bitmap(&self, blend: DirectBlend) -> &ShapePipeline {
        match blend {
            DirectBlend::Trivial(blend) => &self.bitmap[blend],
            DirectBlend::Multiply => &self.multiply.bitmap,
            DirectBlend::Complex(_) => &self.complex_direct.bitmap,
        }
    }

    /// The pipelines of a second draw `blend` needs, after the one `color`,
    /// `gradient` or `bitmap` gives.
    pub fn second_step(&self, blend: DirectBlend) -> Option<&MultiplyPipelines> {
        match blend {
            DirectBlend::Multiply => self.multiply_second_step.as_ref(),
            _ => None,
        }
    }

    pub fn new(
        device: &wgpu::Device,
        shaders: &Shaders,
        format: wgpu::TextureFormat,
        msaa_sample_count: u32,
        bind_layouts: &BindLayouts,
    ) -> Self {
        let colort_bindings = vec![Some(&bind_layouts.globals), Some(&bind_layouts.transforms)];

        let color_pipelines = EnumMap::from_fn(|blend: TrivialBlend| {
            create_shape_pipeline(
                &format!("Color ({blend:?})"),
                device,
                format,
                &shaders.color_shader,
                msaa_sample_count,
                &VERTEX_BUFFERS_DESCRIPTION_COLOR,
                &colort_bindings,
                blend.blend_state(),
                0,
                PrimitiveTopology::TriangleList,
            )
        });

        let lines_pipelines = create_shape_pipeline(
            "Lines",
            device,
            format,
            &shaders.color_shader,
            msaa_sample_count,
            &VERTEX_BUFFERS_DESCRIPTION_COLOR,
            &colort_bindings,
            BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            0,
            PrimitiveTopology::LineStrip,
        );

        let gradient_bindings = vec![
            Some(&bind_layouts.globals),
            Some(&bind_layouts.transforms),
            Some(&bind_layouts.gradient),
        ];

        let gradient_pipelines = EnumMap::from_fn(|blend: TrivialBlend| {
            create_shape_pipeline(
                &format!("Gradient ({blend:?})"),
                device,
                format,
                &shaders.gradient_shader,
                msaa_sample_count,
                &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                &gradient_bindings,
                blend.blend_state(),
                0,
                PrimitiveTopology::TriangleList,
            )
        });

        let complex_blend_bindings =
            vec![Some(&bind_layouts.globals), None, Some(&bind_layouts.blend)];

        let complex_blend_pipelines = enum_map! {
            blend => create_shape_pipeline(
                &format!("Complex Blend: {blend:?}"),
                device,
                format,
                &shaders.blend_shaders[blend],
                msaa_sample_count,
                &VERTEX_BUFFERS_DESCRIPTION_POS,
                &complex_blend_bindings,
                BlendState::REPLACE,
                0,
                PrimitiveTopology::TriangleList,
            )
        };

        let bitmap_blend_bindings = vec![
            Some(&bind_layouts.globals),
            Some(&bind_layouts.transforms),
            Some(&bind_layouts.bitmap),
        ];

        let bitmap_pipelines = EnumMap::from_fn(|blend: TrivialBlend| {
            create_shape_pipeline(
                &format!("Bitmap ({blend:?})"),
                device,
                format,
                &shaders.bitmap_shader,
                msaa_sample_count,
                &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                &bitmap_blend_bindings,
                blend.blend_state(),
                0,
                PrimitiveTopology::TriangleList,
            )
        });

        let bitmap_opaque_pipeline_layout_label =
            create_debug_label!("Opaque bitmap pipeline layout");
        let bitmap_opaque_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: bitmap_opaque_pipeline_layout_label.as_deref(),
                bind_group_layouts: &bitmap_blend_bindings,
                immediate_size: 0,
            });

        let bitmap_opaque = device.create_render_pipeline(&create_pipeline_descriptor(
            create_debug_label!("Bitmap opaque copy").as_deref(),
            &shaders.bitmap_shader,
            &shaders.bitmap_shader,
            &bitmap_opaque_pipeline_layout,
            None,
            &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
            &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
            msaa_sample_count,
            &[("late_saturate", 1.0)],
            PrimitiveTopology::TriangleList,
        ));

        let bitmap_opaque_dummy_depth = device.create_render_pipeline(&create_pipeline_descriptor(
            create_debug_label!("Bitmap opaque copy").as_deref(),
            &shaders.bitmap_shader,
            &shaders.bitmap_shader,
            &bitmap_opaque_pipeline_layout,
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Stencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState {
                    front: wgpu::StencilFaceState::IGNORE,
                    back: wgpu::StencilFaceState::IGNORE,
                    read_mask: 0,
                    write_mask: 0,
                },
                bias: wgpu::DepthBiasState::default(),
            }),
            &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
            &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
            msaa_sample_count,
            &[],
            PrimitiveTopology::TriangleList,
        ));

        let multiply_pipelines = |shaders: [&wgpu::ShaderModule; 3], blend_state, step: &str| {
            let pipeline = |name: &str, shader, vertex_buffers, bindings| {
                create_shape_pipeline(
                    &format!("{name} (Multiply{step})"),
                    device,
                    format,
                    shader,
                    msaa_sample_count,
                    vertex_buffers,
                    bindings,
                    blend_state,
                    0,
                    PrimitiveTopology::TriangleList,
                )
            };
            let [color, gradient, bitmap] = shaders;
            MultiplyPipelines {
                color: pipeline(
                    "Color",
                    color,
                    &VERTEX_BUFFERS_DESCRIPTION_COLOR,
                    &colort_bindings,
                ),
                gradient: pipeline(
                    "Gradient",
                    gradient,
                    &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                    &gradient_bindings,
                ),
                bitmap: pipeline(
                    "Bitmap",
                    bitmap,
                    &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                    &bitmap_blend_bindings,
                ),
            }
        };
        let (multiply, multiply_second_step) = match &shaders.multiply {
            Some(multiply) => (
                multiply_pipelines(
                    [&multiply.color, &multiply.gradient, &multiply.bitmap],
                    DirectBlend::multiply_blend_state(),
                    "",
                ),
                None,
            ),
            None => {
                let [first, second] = DirectBlend::multiply_step_blend_states();
                let shaders = [
                    &shaders.color_shader,
                    &shaders.gradient_shader,
                    &shaders.bitmap_shader,
                ];
                (
                    multiply_pipelines(shaders, first, ", first step"),
                    Some(multiply_pipelines(shaders, second, ", second step")),
                )
            }
        };

        let complex_direct = {
            let parent = Some(&bind_layouts.parent_copy);
            let color_bindings = vec![
                Some(&bind_layouts.globals),
                Some(&bind_layouts.transforms),
                None,
                parent,
            ];
            let gradient_bindings = vec![
                Some(&bind_layouts.globals),
                Some(&bind_layouts.transforms),
                Some(&bind_layouts.gradient),
                parent,
            ];
            let bitmap_bindings = vec![
                Some(&bind_layouts.globals),
                Some(&bind_layouts.transforms),
                Some(&bind_layouts.bitmap),
                parent,
            ];
            let shaders = &shaders.complex_direct;
            let pipeline = |name: &str, shader, vertex_buffers, bindings: &[_]| {
                create_shape_pipeline(
                    &format!("{name} (complex blend)"),
                    device,
                    format,
                    shader,
                    msaa_sample_count,
                    vertex_buffers,
                    bindings,
                    BlendState::REPLACE,
                    0,
                    PrimitiveTopology::TriangleList,
                )
            };
            MultiplyPipelines {
                color: pipeline(
                    "Color",
                    &shaders.color,
                    &VERTEX_BUFFERS_DESCRIPTION_COLOR,
                    &color_bindings,
                ),
                gradient: pipeline(
                    "Gradient",
                    &shaders.gradient,
                    &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                    &gradient_bindings,
                ),
                bitmap: pipeline(
                    "Bitmap",
                    &shaders.bitmap,
                    &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                    &bitmap_bindings,
                ),
            }
        };

        let alpha_mask_bindings = vec![
            Some(&bind_layouts.globals),
            Some(&bind_layouts.transforms),
            Some(&bind_layouts.alpha_mask),
        ];

        let alpha_mask_pipeline = create_shape_pipeline(
            "Alpha Mask",
            device,
            format,
            &shaders.alpha_mask_shader,
            msaa_sample_count,
            &VERTEX_BUFFERS_DESCRIPTION_POS,
            &alpha_mask_bindings,
            BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            0,
            PrimitiveTopology::TriangleList,
        );

        Self {
            color: color_pipelines,
            lines: lines_pipelines,
            bitmap: bitmap_pipelines,
            bitmap_opaque,
            bitmap_opaque_dummy_stencil: bitmap_opaque_dummy_depth,
            gradients: gradient_pipelines,
            complex_blends: complex_blend_pipelines,
            alpha_mask: alpha_mask_pipeline,
            multiply,
            multiply_second_step,
            complex_direct,
        }
    }
}

#[expect(clippy::too_many_arguments)]
fn create_pipeline_descriptor<'a>(
    label: Option<&'a str>,
    vertex_shader: &'a wgpu::ShaderModule,
    fragment_shader: &'a wgpu::ShaderModule,
    pipeline_layout: &'a wgpu::PipelineLayout,
    depth_stencil_state: Option<wgpu::DepthStencilState>,
    color_target_state: &'a [Option<wgpu::ColorTargetState>],
    vertex_buffer_layout: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    msaa_sample_count: u32,
    fragment_constants: &'a [(&str, f64)],
    primitive_topology: PrimitiveTopology,
) -> wgpu::RenderPipelineDescriptor<'a> {
    wgpu::RenderPipelineDescriptor {
        label,
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: vertex_shader,
            entry_point: Some("main_vertex"),
            buffers: vertex_buffer_layout,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: fragment_shader,
            entry_point: Some("main_fragment"),
            targets: color_target_state,
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: fragment_constants,
                ..Default::default()
            },
        }),
        primitive: wgpu::PrimitiveState {
            topology: primitive_topology,
            // All indexed draws in this backend use `Uint32` indices, and wgpu
            // requires this to be set for indexed drawing with strip topologies.
            strip_index_format: matches!(
                primitive_topology,
                PrimitiveTopology::LineStrip | PrimitiveTopology::TriangleStrip
            )
            .then_some(wgpu::IndexFormat::Uint32),
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::default(),
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: depth_stencil_state,
        multisample: wgpu::MultisampleState {
            count: msaa_sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    }
}

#[expect(clippy::too_many_arguments)]
fn create_shape_pipeline(
    name: &str,
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    shader: &wgpu::ShaderModule,
    msaa_sample_count: u32,
    vertex_buffers: &'static [Option<wgpu::VertexBufferLayout<'static>>],
    bind_group_layouts: &[Option<&wgpu::BindGroupLayout>],
    blend: BlendState,
    immediate_size: u32,
    primitive_topology: PrimitiveTopology,
) -> ShapePipeline {
    let pipeline_layout_label = create_debug_label!("{} shape pipeline layout", name);
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: pipeline_layout_label.as_deref(),
        bind_group_layouts,
        immediate_size,
    });
    ShapePipeline {
        name: name.to_owned(),
        device: device.clone(),
        layout,
        shader: shader.clone(),
        format,
        msaa_sample_count,
        vertex_buffers,
        blend,
        primitive_topology,
        pipelines: EnumMap::default(),
        stencilless: OnceLock::new(),
    }
}
