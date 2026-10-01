use crate::Texture;
use crate::backend::RenderTargetMode;
use crate::buffer_pool::TexturePool;
use crate::descriptors::Descriptors;
use crate::filters::{FilterSource, FilterVertex, VERTEX_BUFFERS_DESCRIPTION_FILTERS};
use crate::surface::target::CommandTarget;
use crate::utils::SampleCountMap;
use bytemuck::{Pod, Zeroable};
use std::sync::OnceLock;
use swf::BlurFilter as BlurFilterArgs;
use wgpu::util::{DeviceExt, StagingBelt};
use wgpu::{BufferSlice, CommandEncoder, RenderPipeline, TextureView};

/// This is a 1:1 match of `struct Filter` in `blur.wgsl`. See that, and the usage below, for more info.
/// Since WebGL requires 16 byte struct size (alignment), some of these fields (namely m2 and last_weight)
/// are passed in precomputed, even though they are trivial to get (addition/multiplication by constant).
/// The struct would have to be padded with dummy data otherwise anyway - these are at least useful.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable, PartialEq)]
struct BlurUniform {
    direction: [f32; 2],
    full_size: f32,
    m: f32,
    m2: f32,
    first_weight: f32,
    last_offset: f32,
    last_weight: f32,
    uv_bounds: [f32; 4],
}

/// One direction of `struct Filter` in `blur_2d.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable, PartialEq)]
struct BlurAxis {
    full_size: f32,
    m2: f32,
    first_weight: f32,
    last_offset: f32,
    last_weight: f32,
    step: f32,
    _padding: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable, PartialEq)]
struct Blur2dUniform {
    x: BlurAxis,
    y: BlurAxis,
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
    uv_bounds: [f32; 4],
}

struct Kernel {
    full_size: f32,
    m: f32,
    first_weight: f32,
    last_offset: f32,
    last_weight: f32,
}

impl Kernel {
    fn new(strength: f32) -> Option<Self> {
        // Full width of the kernel (left edge to right edge)
        let full_size = strength.min(255.0);
        if full_size <= 1.0 {
            // A width of 1 or less is a noop (it'd just sample itself and nothing else)
            return None;
        }

        // See this article for additional information on the fractional blur algorithm, as this
        // implementation was inspired by it: https://fgiesen.wordpress.com/2012/08/01/fast-blurs-2/

        // This is how much the blur "extends past" the center pixel to either side.
        let radius = (full_size - 1.0) / 2.0;

        // This is how many simple double-1 weighted pixel pairs we can sample in the center.
        // Note how we're not using floor() here. This is to guarantee that alpha is not 0 when
        // radius is a whole number: That would cause the division below to end the universe,
        // and more importantly, also waste at least one sampling of the texture (the first one).
        // This way, alpha is 1 instead in those cases (with m being one smaller), and the last
        // two samplings can be fused into one, at the right place and with the right weight.
        let m = radius.ceil() - 1.0;
        // Not the transparency kind. It's almost the fractional part of radius.
        // If radius is a whole number, however, it's 1 instead of 0.
        // The rounding is done to imitate the fixed-point calculations in Flash Player,
        // improving emulation accuracy somewhat.
        let alpha = ((radius - m) * 255.0).floor() / 255.0;

        // These control how and where the last pair of pixels are to be sampled,
        // so that the next-to-last will end up with an effective weight of 1.0,
        // and the last one with a weight of alpha. Note that the offset is relative
        // to the center of the next-to-last sampled pixel, in the range of 0 to 0.5.
        let last_offset = 1.0 / ((1.0 / alpha) + 1.0);
        let last_weight = alpha + 1.0;
        Some(Self {
            full_size,
            m,
            first_weight: alpha,
            last_offset,
            last_weight,
        })
    }

    fn axis(&self, texels: f32) -> BlurAxis {
        BlurAxis {
            full_size: self.full_size,
            m2: self.m * 2.0,
            first_weight: self.first_weight,
            last_offset: self.last_offset,
            last_weight: self.last_weight,
            step: 1.0 / texels,
            _padding: [0.0; 2],
        }
    }
}

const MAX_SINGLE_PASS_SAMPLES: f32 = 30.0;

fn single_pass_kernels(filter: &BlurFilterArgs) -> Option<[Kernel; 2]> {
    let kernel_x = Kernel::new(filter.blur_x.to_f32())?;
    let kernel_y = Kernel::new(filter.blur_y.to_f32())?;
    // Every vertical tap blurs a row: first, pairs, last pair (two rows).
    let samples = (kernel_y.m * 2.0 + 3.0) * (kernel_x.m + 2.0);
    (filter.num_passes() == 1 && samples <= MAX_SINGLE_PASS_SAMPLES).then_some([kernel_x, kernel_y])
}

impl Blur2dUniform {
    fn new(source: &FilterSource, [kernel_x, kernel_y]: &[Kernel; 2]) -> Self {
        let texture_width = source.texture.width() as f32;
        let texture_height = source.texture.height() as f32;
        Self {
            x: kernel_x.axis(texture_width),
            y: kernel_y.axis(texture_height),
            uv_scale: [
                source.size.0 as f32 / texture_width,
                source.size.1 as f32 / texture_height,
            ],
            uv_offset: [
                source.point.0 as f32 / texture_width,
                source.point.1 as f32 / texture_height,
            ],
            uv_bounds: source.uv_bounds(),
        }
    }
}

pub struct BlurFilter {
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group_layout_2d: wgpu::BindGroupLayout,
    pipeline_layout_2d: wgpu::PipelineLayout,
    uniform_buffer_2d: wgpu::Buffer,
    pipelines_2d: SampleCountMap<OnceLock<wgpu::RenderPipeline>>,
    pipeline_layout: wgpu::PipelineLayout,
    vertex_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    vertices_size: wgpu::BufferSize,
    uniform_size: wgpu::BufferSize,
    pipelines: SampleCountMap<OnceLock<wgpu::RenderPipeline>>,
}

impl BlurFilter {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let sampling = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let uniform_size = std::mem::size_of::<BlurUniform>() as u64;
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                texture,
                sampling,
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(uniform_size),
                    },
                    count: None,
                },
            ],
            label: create_debug_label!("Blur filter binds (with buffer)").as_deref(),
        });

        let uniform_size_2d = std::mem::size_of::<Blur2dUniform>() as u64;
        let bind_group_layout_2d =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[
                    texture,
                    sampling,
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: wgpu::BufferSize::new(uniform_size_2d),
                        },
                        count: None,
                    },
                ],
                label: create_debug_label!("Single pass blur filter binds").as_deref(),
            });
        let uniform_buffer_2d = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: uniform_size_2d,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let pipeline_layout_2d = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_group_layout_2d)],
            immediate_size: 0,
        });

        let vertices_size = std::mem::size_of::<[FilterVertex; 4]>() as u64;
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
            bind_group_layout_2d,
            pipeline_layout_2d,
            uniform_buffer_2d,
            pipelines_2d: Default::default(),
            pipelines: Default::default(),
            pipeline_layout,
            vertex_buffer,
            uniform_buffer,
            bind_group_layout,
            vertices_size: wgpu::BufferSize::new(vertices_size).expect("Definitely not zero."),
            uniform_size: wgpu::BufferSize::new(uniform_size).expect("Definitely not zero."),
        }
    }

    fn pipeline(&self, descriptors: &Descriptors, msaa_sample_count: u32) -> &wgpu::RenderPipeline {
        self.pipelines.get_or_init(msaa_sample_count, || {
            make_pipeline(
                descriptors,
                &self.pipeline_layout,
                &descriptors.shaders.blur_filter,
                msaa_sample_count,
            )
        })
    }

    fn pipeline_2d(
        &self,
        descriptors: &Descriptors,
        msaa_sample_count: u32,
    ) -> &wgpu::RenderPipeline {
        self.pipelines_2d.get_or_init(msaa_sample_count, || {
            make_pipeline(
                descriptors,
                &self.pipeline_layout_2d,
                &descriptors.shaders.blur_2d_filter,
                msaa_sample_count,
            )
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
        filter: &BlurFilterArgs,
        destination: Option<&Texture>,
    ) -> Option<CommandTarget> {
        let sample_count = source.texture.sample_count();
        let format = source.texture.format();
        let pipeline = self.pipeline(descriptors, sample_count);
        let destination = destination.filter(|destination| {
            destination.texture.size()
                == wgpu::Extent3d {
                    width: source.size.0,
                    height: source.size.1,
                    depth_or_array_layers: 1,
                }
                && destination.texture.format() == format
                && destination.texture.sample_count() == sample_count
        });
        if let Some(kernels) = single_pass_kernels(filter) {
            return Some(self.apply_single_pass(
                descriptors,
                texture_pool,
                draw_encoder,
                staging_belt,
                source,
                &kernels,
                destination.filter(|destination| destination.texture != *source.texture),
            ));
        }
        let kernel_x = Kernel::new(filter.blur_x.to_f32());
        let kernel_y = Kernel::new(filter.blur_y.to_f32());
        let directions = usize::from(kernel_x.is_some()) + usize::from(kernel_y.is_some());
        let total_passes = filter.num_passes() as usize * directions;
        let mut passes = 0;

        let mut flip = CommandTarget::new(
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
        let mut flop = CommandTarget::new(
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
            .write_buffer(draw_encoder, &self.vertex_buffer, 0, self.vertices_size)
            .copy_from_slice(bytemuck::cast_slice(&[source.vertices()]));

        let mut first = true;
        for _ in 0..(filter.num_passes() as usize) {
            for i in 0..2 {
                let horizontal = i % 2 == 0;
                let Some(kernel) = (if horizontal { &kernel_x } else { &kernel_y }) else {
                    continue;
                };

                passes += 1;
                // The last pass can render straight into the destination, unless
                // it reads from it (a single pass, reading the source).
                let into_destination = destination.filter(|destination| {
                    passes == total_passes && !(first && destination.texture == *source.texture)
                });

                let (previous_view, previous_vertices, previous_width, previous_height, uv_bounds) =
                    if first {
                        first = false;
                        (
                            source.view,
                            self.vertex_buffer.slice(..),
                            source.texture.width() as f32,
                            source.texture.height() as f32,
                            source.uv_bounds(),
                        )
                    } else {
                        (
                            flip.color_view(),
                            descriptors.quad.filter_vertices.slice(..),
                            flip.width() as f32,
                            flip.height() as f32,
                            FilterSource::for_entire_texture(
                                flip.color_texture(),
                                flip.color_view(),
                            )
                            .uv_bounds(),
                        )
                    };

                let uniform = BlurUniform {
                    direction: if horizontal {
                        [1.0 / previous_width, 0.0]
                    } else {
                        [0.0, 1.0 / previous_height]
                    },
                    full_size: kernel.full_size,
                    m: kernel.m,
                    m2: kernel.m * 2.0,
                    first_weight: kernel.first_weight,
                    last_offset: kernel.last_offset,
                    last_weight: kernel.last_weight,
                    uv_bounds,
                };
                staging_belt
                    .write_buffer(draw_encoder, &self.uniform_buffer, 0, self.uniform_size)
                    .copy_from_slice(bytemuck::cast_slice(&[uniform]));

                if let Some(destination) = into_destination {
                    let target = CommandTarget::new(
                        descriptors,
                        texture_pool,
                        destination.texture.size(),
                        format,
                        sample_count,
                        RenderTargetMode::ExistingWithColor(
                            destination.texture.clone(),
                            destination.view().clone(),
                            wgpu::Color::TRANSPARENT,
                        ),
                        draw_encoder,
                    );
                    self.render_with_uniform_buffers(
                        descriptors,
                        draw_encoder,
                        pipeline,
                        &target,
                        previous_view,
                        previous_vertices,
                    );
                    return Some(target);
                }

                self.render_with_uniform_buffers(
                    descriptors,
                    draw_encoder,
                    pipeline,
                    &flop,
                    previous_view,
                    previous_vertices,
                );

                std::mem::swap(&mut flip, &mut flop);
            }
        }

        if first {
            // Nothing happened, don't return an empty unused texture
            None
        } else {
            Some(flip)
        }
    }

    /// Both passes of a one-quality blur at once, with `blur_2d.wgsl`, into
    /// `destination` if given (it mustn't be the source) or a new target.
    #[expect(clippy::too_many_arguments)]
    fn apply_single_pass(
        &self,
        descriptors: &Descriptors,
        texture_pool: &mut TexturePool,
        draw_encoder: &mut wgpu::CommandEncoder,
        staging_belt: &mut StagingBelt,
        source: &FilterSource,
        kernels: &[Kernel; 2],
        destination: Option<&Texture>,
    ) -> CommandTarget {
        let sample_count = source.texture.sample_count();
        let format = source.texture.format();
        let size = wgpu::Extent3d {
            width: source.size.0,
            height: source.size.1,
            depth_or_array_layers: 1,
        };
        let target = CommandTarget::new(
            descriptors,
            texture_pool,
            size,
            format,
            sample_count,
            match destination {
                Some(destination) => RenderTargetMode::ExistingWithColor(
                    destination.texture.clone(),
                    destination.view().clone(),
                    wgpu::Color::TRANSPARENT,
                ),
                None => RenderTargetMode::FreshWithColor(wgpu::Color::TRANSPARENT),
            },
            draw_encoder,
        );
        let uniform = Blur2dUniform::new(source, kernels);
        staging_belt
            .write_buffer(
                draw_encoder,
                &self.uniform_buffer_2d,
                0,
                wgpu::BufferSize::new(std::mem::size_of::<Blur2dUniform>() as u64)
                    .expect("Definitely not zero."),
            )
            .copy_from_slice(bytemuck::cast_slice(&[uniform]));

        let bind_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Single pass blur group").as_deref(),
                layout: &self.bind_group_layout_2d,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            descriptors.bitmap_samplers.get_sampler(false, true),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buffer_2d.as_entire_binding(),
                    },
                ],
            });

        crate::backend::count_render_pass(crate::stats::PassKind::Filter);
        let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: create_debug_label!("Single pass blur filter").as_deref(),
            color_attachments: &[target.color_attachments()],
            ..Default::default()
        });
        render_pass.set_pipeline(self.pipeline_2d(descriptors, sample_count));
        render_pass.set_bind_group(0, &bind_group, &[0]);
        render_pass.set_vertex_buffer(0, descriptors.quad.filter_vertices.slice(..));
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        render_pass.draw_indexed(0..6, 0, 0..1);
        drop(render_pass);
        target
    }

    pub fn is_single_pass(&self, filter: &BlurFilterArgs) -> bool {
        single_pass_kernels(filter).is_some()
    }

    /// Blurs parts of one texture, each with a filter that `is_single_pass`,
    /// into the same places in `target`, in one render pass.
    pub fn apply_single_pass_in_place(
        &self,
        descriptors: &Descriptors,
        draw_encoder: &mut wgpu::CommandEncoder,
        target: &CommandTarget,
        blurs: &[(FilterSource, &BlurFilterArgs)],
    ) {
        let Some((first, _)) = blurs.first() else {
            return;
        };
        let size = std::mem::size_of::<Blur2dUniform>();
        let stride =
            size.next_multiple_of(descriptors.limits.min_uniform_buffer_offset_alignment as usize);
        let mut uniforms = vec![0; stride * blurs.len()];
        for ((source, filter), uniform) in blurs.iter().zip(uniforms.chunks_mut(stride)) {
            let kernels = single_pass_kernels(filter).expect("Only single-pass blurs");
            uniform[..size]
                .copy_from_slice(bytemuck::bytes_of(&Blur2dUniform::new(source, &kernels)));
        }
        let uniforms = descriptors
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: create_debug_label!("Single pass blurs").as_deref(),
                contents: &uniforms,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Single pass blurs group").as_deref(),
                layout: &self.bind_group_layout_2d,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(first.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            descriptors.bitmap_samplers.get_sampler(false, true),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniforms,
                            offset: 0,
                            size: wgpu::BufferSize::new(size as u64),
                        }),
                    },
                ],
            });

        crate::backend::count_render_pass(crate::stats::PassKind::Filter);
        let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: create_debug_label!("Single pass blur filters").as_deref(),
            color_attachments: &[target.color_attachments()],
            ..Default::default()
        });
        render_pass.set_pipeline(self.pipeline_2d(descriptors, first.texture.sample_count()));
        render_pass.set_vertex_buffer(0, descriptors.quad.filter_vertices.slice(..));
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        for (i, (source, _)) in blurs.iter().enumerate() {
            render_pass.set_viewport(
                source.point.0 as f32,
                source.point.1 as f32,
                source.size.0 as f32,
                source.size.1 as f32,
                0.0,
                1.0,
            );
            render_pass.set_bind_group(0, &bind_group, &[(i * stride) as u32]);
            render_pass.draw_indexed(0..6, 0, 0..1);
        }
    }

    fn render_with_uniform_buffers(
        &self,
        descriptors: &Descriptors,
        draw_encoder: &mut CommandEncoder,
        pipeline: &RenderPipeline,
        destination: &CommandTarget,
        source: &TextureView,
        vertices: BufferSlice,
    ) {
        let filter_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Filter group").as_deref(),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            descriptors.bitmap_samplers.get_sampler(false, true),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                ],
            });

        crate::backend::count_render_pass(crate::stats::PassKind::Filter);
        let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: create_debug_label!("Blur filter").as_deref(),
            color_attachments: &[destination.color_attachments()],
            ..Default::default()
        });
        render_pass.set_pipeline(pipeline);

        render_pass.set_bind_group(0, &filter_group, &[]);

        render_pass.set_vertex_buffer(0, vertices);
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        render_pass.draw_indexed(0..6, 0, 0..1);
    }
}

fn make_pipeline(
    descriptors: &Descriptors,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    msaa_sample_count: u32,
) -> wgpu::RenderPipeline {
    let label = create_debug_label!("Blur Filter ({} msaa)", msaa_sample_count);
    descriptors
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: label.as_deref(),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("main_vertex"),
                buffers: &VERTEX_BUFFERS_DESCRIPTION_FILTERS,
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
                module,
                entry_point: Some("main_fragment"),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        })
}
