use crate::backend::RenderTargetMode;
use crate::buffer_pool::TexturePool;
use crate::descriptors::Descriptors;
use crate::filters::blur::BlurFilter;
use crate::filters::{
    FilterSource, FilterVertexWithBlur, VERTEX_BUFFERS_DESCRIPTION_FILTERS_WITH_BLUR,
};
use crate::surface::target::CommandTarget;
use crate::utils::SampleCountMap;
use bytemuck::{Pod, Zeroable};
use std::sync::OnceLock;
use swf::GlowFilter as GlowFilterArgs;
use wgpu::util::StagingBelt;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable, PartialEq)]
struct GlowUniform {
    color: [f32; 4],
    strength: f32,
    inner: u32,            // a wasteful bool, but we need to be aligned anyway
    knockout: u32,         // a wasteful bool, but we need to be aligned anyway
    composite_source: u32, // undocumented flash feature, another bool
}

impl GlowUniform {
    fn new(filter: &GlowFilterArgs) -> Self {
        Self {
            color: [
                f32::from(filter.color.r) / 255.0,
                f32::from(filter.color.g) / 255.0,
                f32::from(filter.color.b) / 255.0,
                f32::from(filter.color.a) / 255.0,
            ],
            strength: filter.strength.to_f32(),
            inner: if filter.is_inner() { 1 } else { 0 },
            knockout: if filter.is_knockout() { 1 } else { 0 },
            composite_source: if filter.composite_source() { 1 } else { 0 },
        }
    }
}

pub struct GlowFilter {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    vertex_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    vertices_size: wgpu::BufferSize,
    uniform_size: wgpu::BufferSize,
    pipeline: SampleCountMap<OnceLock<wgpu::RenderPipeline>>,
}

impl GlowFilter {
    pub fn new(device: &wgpu::Device) -> Self {
        let uniform_size = std::mem::size_of::<GlowUniform>() as u64;
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(uniform_size),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
            label: create_debug_label!("Blur filter binds").as_deref(),
        });

        let vertices_size = std::mem::size_of::<[FilterVertexWithBlur; 4]>() as u64;
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: vertices_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: uniform_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        Self {
            pipeline: Default::default(),
            pipeline_layout,
            vertex_buffer,
            uniform_buffer,
            bind_group_layout,
            uniform_size: wgpu::BufferSize::new(uniform_size).expect("Definitely not zero."),
            vertices_size: wgpu::BufferSize::new(vertices_size).expect("Definitely not zero."),
        }
    }

    fn pipeline(&self, descriptors: &Descriptors, msaa_sample_count: u32) -> &wgpu::RenderPipeline {
        self.pipeline.get_or_init(msaa_sample_count, || {
            let label = create_debug_label!("Glow Filter ({} msaa)", msaa_sample_count);
            descriptors
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: label.as_deref(),
                    layout: Some(&self.pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &descriptors.shaders.glow_filter,
                        entry_point: Some("main_vertex"),
                        buffers: &VERTEX_BUFFERS_DESCRIPTION_FILTERS_WITH_BLUR,
                        compilation_options: Default::default(),
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        strip_index_format: None,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        polygon_mode: wgpu::PolygonMode::default(),
                        unclipped_depth: false,
                        conservative: false,
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState {
                        count: msaa_sample_count,
                        mask: !0,
                        alpha_to_coverage_enabled: false,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &descriptors.shaders.glow_filter,
                        entry_point: Some("main_fragment"),
                        targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
                        compilation_options: Default::default(),
                    }),
                    multiview_mask: None,
                    cache: None,
                })
        })
    }

    #[expect(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        descriptors: &Descriptors,
        texture_pool: &mut TexturePool,
        draw_encoder: &mut wgpu::CommandEncoder,
        staging_belt: &mut StagingBelt,
        source: &FilterSource,
        filter: &GlowFilterArgs,
        blur_filter: &BlurFilter,
        blur_offset: (f32, f32),
    ) -> CommandTarget {
        let sample_count = source.texture.sample_count();
        let format = source.texture.format();
        let pipeline = self.pipeline(descriptors, sample_count);
        let blurred = blur_filter.apply(
            descriptors,
            texture_pool,
            draw_encoder,
            staging_belt,
            source,
            &filter.inner_blur_filter(),
            None,
        );
        let blurred_source = match &blurred {
            Some(blurred) => {
                blurred.ensure_cleared(draw_encoder);
                FilterSource::for_entire_texture(blurred.color_texture(), blurred.color_view())
            }
            None => *source,
        };

        let target = CommandTarget::new(
            descriptors,
            texture_pool,
            wgpu::Extent3d {
                width: source.size.0,
                height: source.size.1,
                depth_or_array_layers: 1,
            },
            format,
            sample_count,
            RenderTargetMode::FreshWithColor(wgpu::Color::TRANSPARENT),
            draw_encoder,
        );
        staging_belt
            .write_buffer(draw_encoder, &self.uniform_buffer, 0, self.uniform_size)
            .copy_from_slice(bytemuck::cast_slice(&[GlowUniform::new(filter)]));
        staging_belt
            .write_buffer(draw_encoder, &self.vertex_buffer, 0, self.vertices_size)
            .copy_from_slice(bytemuck::cast_slice(&[
                source.vertices_with_blur_offset(&blurred_source, blur_offset)
            ]));
        let filter_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Filter group").as_deref(),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            descriptors.bitmap_samplers.get_sampler(false, false),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(blurred_source.view),
                    },
                ],
            });
        crate::backend::count_render_pass(crate::stats::PassKind::Filter);
        let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: create_debug_label!("Glow filter").as_deref(),
            color_attachments: &[target.color_attachments()],
            ..Default::default()
        });
        render_pass.set_pipeline(pipeline);

        render_pass.set_bind_group(0, &filter_group, &[0]);

        render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        render_pass.draw_indexed(0..6, 0, 0..1);
        target
    }

    /// Glows parts of one texture whose blurred copies are parts of another,
    /// into the same places in `target`, in one render pass.
    pub fn apply_in_place(
        &self,
        descriptors: &Descriptors,
        draw_encoder: &mut wgpu::CommandEncoder,
        staging_belt: &mut StagingBelt,
        target: &CommandTarget,
        glows: &[(FilterSource, FilterSource, &GlowFilterArgs)],
    ) {
        let Some((first, first_blurred, _)) = glows.first() else {
            return;
        };
        let size = std::mem::size_of::<GlowUniform>();
        let stride =
            size.next_multiple_of(descriptors.limits.min_uniform_buffer_offset_alignment as usize);
        let mut uniforms = vec![0; stride * glows.len()];
        for ((_, _, filter), uniform) in glows.iter().zip(uniforms.chunks_mut(stride)) {
            uniform[..size].copy_from_slice(bytemuck::bytes_of(&GlowUniform::new(filter)));
        }
        let vertices: Vec<_> = glows
            .iter()
            .map(|(source, blurred, _)| source.vertices_with_blur_offset(blurred, (0.0, 0.0)))
            .collect();
        let mut scratch = descriptors.scratch.lock().expect("Scratch lock");
        let (uniforms, uniforms_offset) = scratch.write(
            &descriptors.device,
            staging_belt,
            draw_encoder,
            &uniforms,
            stride as u64,
        );
        let (vertices, vertices_offset) = scratch.write(
            &descriptors.device,
            staging_belt,
            draw_encoder,
            bytemuck::cast_slice(&vertices),
            stride as u64,
        );
        drop(scratch);
        let filter_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Glows group").as_deref(),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(first.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            descriptors.bitmap_samplers.get_sampler(false, false),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniforms,
                            offset: uniforms_offset,
                            size: wgpu::BufferSize::new(size as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(first_blurred.view),
                    },
                ],
            });

        crate::backend::count_render_pass(crate::stats::PassKind::Filter);
        let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: create_debug_label!("Glow filters").as_deref(),
            color_attachments: &[target.color_attachments()],
            ..Default::default()
        });
        render_pass.set_pipeline(self.pipeline(descriptors, first.texture.sample_count()));
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        let vertices_size = std::mem::size_of::<[FilterVertexWithBlur; 4]>() as u64;
        if descriptors.base_vertex {
            render_pass.set_vertex_buffer(0, vertices.slice(vertices_offset..));
        }
        for (i, (source, _, _)) in glows.iter().enumerate() {
            render_pass.set_viewport(
                source.point.0 as f32,
                source.point.1 as f32,
                source.size.0 as f32,
                source.size.1 as f32,
                0.0,
                1.0,
            );
            render_pass.set_bind_group(0, &filter_group, &[(i * stride) as u32]);
            if descriptors.base_vertex {
                render_pass.draw_indexed(0..6, 4 * i as i32, 0..1);
            } else {
                render_pass.set_vertex_buffer(
                    0,
                    vertices.slice(vertices_offset + i as u64 * vertices_size..),
                );
                render_pass.draw_indexed(0..6, 0, 0..1);
            }
        }
    }
}
