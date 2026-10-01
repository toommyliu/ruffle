use crate::buffer_pool::ScratchBuffer;
use crate::filters::{FilterVertex, Filters};
use crate::layouts::BindLayouts;
use crate::pipelines::VERTEX_BUFFERS_DESCRIPTION_POS_UV;
use crate::shaders::Shaders;
use crate::{
    BitmapSamplers, Pipelines, PosColorVertex, PosUvVertex, PosVertex, create_buffer_with_data,
};
use fnv::FnvHashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use wgpu::Backend;
use wgpu::util::DeviceExt;

pub struct Descriptors {
    pub wgpu_instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub limits: wgpu::Limits,
    pub backend: Backend,
    /// Draws can offset their vertices with `base_vertex` instead of binding
    /// the vertex buffer at an offset. WebGL 2 can't.
    pub base_vertex: bool,
    pub queue: wgpu::Queue,
    pub bitmap_samplers: BitmapSamplers,
    pub bind_layouts: BindLayouts,
    pub quad: Quad,
    copy_pipeline: Mutex<FnvHashMap<(u32, wgpu::TextureFormat, bool), wgpu::RenderPipeline>>,
    pub shaders: Shaders,
    pipelines: Mutex<FnvHashMap<(u32, wgpu::TextureFormat), Arc<Pipelines>>>,
    format_features: Mutex<FnvHashMap<wgpu::TextureFormat, wgpu::TextureFormatFeatureFlags>>,
    pub filters: Filters,
    pub complex_direct_modes: wgpu::Buffer,
    pub complex_direct_mode_stride: u32,
    pub scratch: Mutex<ScratchBuffer>,
}

impl Debug for Descriptors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Descriptors")
    }
}

impl Descriptors {
    pub fn new(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let limits = device.limits();
        let bind_layouts = BindLayouts::new(&device);
        let bitmap_samplers = BitmapSamplers::new(&device);
        let shaders = Shaders::new(&device);
        let quad = Quad::new(&device);
        let filters = Filters::new(&device);
        let backend = adapter.get_info().backend;
        let base_vertex = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::BASE_VERTEX);
        let complex_direct_mode_stride = limits.min_uniform_buffer_offset_alignment.max(16);
        let mut modes = vec![0u32; 6 * complex_direct_mode_stride as usize / 4];
        for mode in 0..6 {
            modes[mode * complex_direct_mode_stride as usize / 4] = mode as u32;
        }
        let scratch = Mutex::new(ScratchBuffer::new(&device));
        let complex_direct_modes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: create_debug_label!("Complex blend modes").as_deref(),
            contents: bytemuck::cast_slice(&modes),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        Self {
            wgpu_instance: instance,
            adapter,
            device,
            limits,
            backend,
            base_vertex,
            queue,
            bitmap_samplers,
            bind_layouts,
            quad,
            copy_pipeline: Default::default(),
            shaders,
            pipelines: Default::default(),
            format_features: Default::default(),
            filters,
            complex_direct_modes,
            complex_direct_mode_stride,
            scratch,
        }
    }

    /// `with_stencil` makes it usable in a pass with a stencil attachment,
    /// which it leaves alone.
    pub fn copy_pipeline(
        &self,
        format: wgpu::TextureFormat,
        msaa_sample_count: u32,
        with_stencil: bool,
    ) -> wgpu::RenderPipeline {
        let mut pipelines = self
            .copy_pipeline
            .lock()
            .expect("Pipelines should not be already locked");
        pipelines
            .entry((msaa_sample_count, format, with_stencil))
            .or_insert_with(|| {
                let copy_texture_pipeline_layout =
                    &self
                        .device
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: create_debug_label!("Copy pipeline layout").as_deref(),
                            bind_group_layouts: &[
                                Some(&self.bind_layouts.globals),
                                None,
                                Some(&self.bind_layouts.bitmap),
                            ],
                            immediate_size: 0,
                        });
                self.device
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: create_debug_label!("Copy pipeline").as_deref(),
                        layout: Some(copy_texture_pipeline_layout),
                        vertex: wgpu::VertexState {
                            module: &self.shaders.copy_shader,
                            entry_point: Some("main_vertex"),
                            buffers: &VERTEX_BUFFERS_DESCRIPTION_POS_UV,
                            compilation_options: Default::default(),
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &self.shaders.copy_shader,
                            entry_point: Some("main_fragment"),
                            targets: &[Some(wgpu::ColorTargetState {
                                format,
                                // All of our blending has been done by now, so we want
                                // to overwrite the target pixels without any blending
                                blend: Some(wgpu::BlendState::REPLACE),
                                write_mask: Default::default(),
                            })],
                            compilation_options: Default::default(),
                        }),
                        primitive: wgpu::PrimitiveState {
                            topology: wgpu::PrimitiveTopology::TriangleList,
                            strip_index_format: None,
                            front_face: wgpu::FrontFace::Ccw,
                            cull_mode: None,
                            polygon_mode: wgpu::PolygonMode::default(),
                            unclipped_depth: false,
                            conservative: false,
                        },
                        depth_stencil: with_stencil.then(|| wgpu::DepthStencilState {
                            format: wgpu::TextureFormat::Stencil8,
                            depth_write_enabled: Some(false),
                            depth_compare: Some(wgpu::CompareFunction::Always),
                            stencil: wgpu::StencilState {
                                front: wgpu::StencilFaceState::IGNORE,
                                back: wgpu::StencilFaceState::IGNORE,
                                read_mask: 0,
                                write_mask: 0,
                            },
                            bias: Default::default(),
                        }),
                        multisample: wgpu::MultisampleState {
                            count: msaa_sample_count,
                            mask: !0,
                            alpha_to_coverage_enabled: false,
                        },
                        multiview_mask: None,
                        cache: None,
                    })
            })
            .clone()
    }

    /// On the web, asking the adapter for a format's features reads every
    /// feature it supports from JavaScript, one call each.
    pub fn supported_sample_count(
        &self,
        mut sample_count: u32,
        format: wgpu::TextureFormat,
    ) -> u32 {
        let features = *self
            .format_features
            .lock()
            .expect("Format features should not be already locked")
            .entry(format)
            .or_insert_with(|| self.adapter.get_texture_format_features(format).flags);

        // Keep halving the sample count until we get one that's supported - or 1 (no multisampling)
        // It's not guaranteed that supporting 4x means supporting 2x, so there's no "max" option
        // And it's probably safer to round down than up, given it's a performance setting.
        while sample_count > 1 && !features.sample_count_supported(sample_count) {
            sample_count /= 2;
        }
        sample_count
    }

    pub fn pipelines(&self, msaa_sample_count: u32, format: wgpu::TextureFormat) -> Arc<Pipelines> {
        let mut pipelines = self
            .pipelines
            .lock()
            .expect("Pipelines should not be already locked");
        pipelines
            .entry((msaa_sample_count, format))
            .or_insert_with(|| {
                Arc::new(Pipelines::new(
                    &self.device,
                    &self.shaders,
                    format,
                    msaa_sample_count,
                    &self.bind_layouts,
                ))
            })
            .clone()
    }
}

pub struct Quad {
    pub vertices_pos: wgpu::Buffer,
    pub vertices_pos_uv: wgpu::Buffer,
    pub vertices_pos_color: wgpu::Buffer,
    pub filter_vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub indices_line: wgpu::Buffer,
    pub indices_line_rect: wgpu::Buffer,
}

impl Quad {
    pub fn new(device: &wgpu::Device) -> Self {
        let vertices_pos = [
            PosVertex {
                position: [0.0, 0.0],
            },
            PosVertex {
                position: [1.0, 0.0],
            },
            PosVertex {
                position: [1.0, 1.0],
            },
            PosVertex {
                position: [0.0, 1.0],
            },
        ];
        let vertices_pos_uv = [
            PosUvVertex {
                position: [0.0, 0.0],
                uv: [0.0, 0.0, 1.0],
            },
            PosUvVertex {
                position: [1.0, 0.0],
                uv: [1.0, 0.0, 1.0],
            },
            PosUvVertex {
                position: [1.0, 1.0],
                uv: [1.0, 1.0, 1.0],
            },
            PosUvVertex {
                position: [0.0, 1.0],
                uv: [0.0, 1.0, 1.0],
            },
        ];
        let vertices_pos_color = [
            PosColorVertex {
                position: [0.0, 0.0],
                color: [255, 255, 255, 255],
            },
            PosColorVertex {
                position: [1.0, 0.0],
                color: [255, 255, 255, 255],
            },
            PosColorVertex {
                position: [1.0, 1.0],
                color: [255, 255, 255, 255],
            },
            PosColorVertex {
                position: [0.0, 1.0],
                color: [255, 255, 255, 255],
            },
        ];
        let filter_vertices = [
            FilterVertex {
                position: [0.0, 0.0],
                uv: [0.0, 0.0],
            },
            FilterVertex {
                position: [1.0, 0.0],
                uv: [1.0, 0.0],
            },
            FilterVertex {
                position: [1.0, 1.0],
                uv: [1.0, 1.0],
            },
            FilterVertex {
                position: [0.0, 1.0],
                uv: [0.0, 1.0],
            },
        ];
        let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let indices_line: [u32; 2] = [0, 1];
        let indices_line_rect: [u32; 5] = [0, 1, 2, 3, 0];

        let vbo_pos = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&vertices_pos),
            wgpu::BufferUsages::VERTEX,
            create_debug_label!("Quad vbo (pos)"),
        );

        let vbo_pos_uv = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&vertices_pos_uv),
            wgpu::BufferUsages::VERTEX,
            create_debug_label!("Quad vbo (pos & uv)"),
        );

        let vbo_pos_color = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&vertices_pos_color),
            wgpu::BufferUsages::VERTEX,
            create_debug_label!("Quad vbo (pos & color)"),
        );

        let vbo_filter = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&filter_vertices),
            wgpu::BufferUsages::VERTEX,
            create_debug_label!("Quad vbo (filter)"),
        );

        let ibo = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&indices),
            wgpu::BufferUsages::INDEX,
            create_debug_label!("Quad ibo"),
        );
        let ibo_line = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&indices_line),
            wgpu::BufferUsages::INDEX,
            create_debug_label!("Line ibo"),
        );
        let ibo_line_rect = create_buffer_with_data(
            device,
            bytemuck::cast_slice(&indices_line_rect),
            wgpu::BufferUsages::INDEX,
            create_debug_label!("Line rect ibo"),
        );

        Self {
            vertices_pos: vbo_pos,
            vertices_pos_uv: vbo_pos_uv,
            vertices_pos_color: vbo_pos_color,
            filter_vertices: vbo_filter,
            indices: ibo,
            indices_line: ibo_line,
            indices_line_rect: ibo_line_rect,
        }
    }
}
