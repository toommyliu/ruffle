use crate::buffer_builder::BufferBuilder;
use crate::buffer_pool::{BufferPool, TexturePool};
use crate::context3d::WgpuContext3D;
use crate::dynamic_transforms::DynamicTransforms;
use crate::filters::FilterSource;
use crate::mesh::{CommonGradient, Mesh, PendingDraw};
use crate::mesh_arena::BufferArena;
use crate::pixel_bender::{ShaderMode, run_pixelbender_shader_impl};
use crate::surface::target::CommandTarget;
use crate::surface::{LayerRef, Surface};
use crate::target::{MaybeOwnedBuffer, TextureTarget};
use crate::target::{RenderTargetFrame, TextureBufferInfo};
use crate::utils::{BufferDimensions, run_copy_pipeline};
use crate::{
    Descriptors, DroppedTextures, Error, PosColorVertex, PosUvVertex, QueueSyncHandle,
    RenderTarget, SwapChainTarget, Texture, as_texture, format_list, get_backend_names,
};
use image::imageops::FilterType;
use ruffle_render::backend::{
    BitmapCacheEntry, Context3D, Context3DProfile, PixelBenderOutput, PixelBenderTarget,
};
use ruffle_render::backend::{RenderBackend, ShapeHandle, ViewportDimensions};
use ruffle_render::bitmap::{
    Bitmap, BitmapFormat, BitmapHandle, BitmapSource, PixelRegion, RgbaBufRead, SyncHandle,
};
use ruffle_render::commands::{Command, CommandList};
use ruffle_render::error::Error as BitmapError;
use ruffle_render::filters::Filter;
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::DistilledShape;
use ruffle_render::tessellator::ShapeTessellator;
use std::any::Any;
use std::borrow::Cow;
use std::cell::Cell;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use swf::Color;
use tracing::instrument;
use wgpu::SubmissionIndex;
use wgpu_profiler::{GpuProfiler, GpuProfilerSettings, Scope};

/// Creates a wgpu instance with Ruffle's required configuration.
///
/// This disables indirect call validation because wgpu's validation runs a compute
/// shader that uses `array<u32>`, which requires the `DYNAMIC_ARRAY_SIZE` feature.
/// However, wgpu runs this shader without first checking if the device supports
/// that feature, causing device creation to fail on GPUs that lack it.
/// Since Ruffle doesn't use indirect draws, disabling this validation has no
/// functional impact.
///
/// See <https://github.com/gfx-rs/wgpu/issues/8799>
pub fn create_wgpu_instance(
    backends: wgpu::Backends,
    backend_options: wgpu::BackendOptions,
    display: Option<Box<dyn wgpu::wgt::WgpuHasDisplayHandle>>,
) -> wgpu::Instance {
    let descriptor = match display {
        Some(display) => wgpu::InstanceDescriptor::new_with_display_handle(display),
        None => wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::default()
            .difference(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL)
            .with_env(),
        backend_options,
        ..descriptor
    })
}

pub struct WgpuRenderBackend<T: RenderTarget> {
    pub(crate) descriptors: Arc<Descriptors>,
    target: T,
    surface: Surface,
    meshes: Vec<Mesh>,
    shape_tessellator: ShapeTessellator,
    // This is currently unused - we just store it to report in
    // `get_viewport_dimensions`
    viewport_scale_factor: f64,
    texture_pool: TexturePool,
    offscreen_texture_pool: TexturePool,
    pub(crate) offscreen_buffer_pool: Arc<BufferPool<wgpu::Buffer, BufferDimensions>>,
    dynamic_transforms: DynamicTransforms,
    active_frame: ActiveFrame,
    profiler: GpuProfiler,
    mesh_buffers: MeshBuffers,
    dropped_textures: Arc<DroppedTextures>,
}

#[derive(Debug)]
struct MeshBuffers {
    vertices: BufferArena,
    indices: BufferArena,
    uniforms: BufferArena,
}

impl MeshBuffers {
    fn new(limits: &wgpu::Limits) -> Self {
        Self {
            vertices: BufferArena::new(
                "Mesh vertices",
                wgpu::BufferUsages::VERTEX,
                MESH_VERTEX_ALIGNMENT,
                4 << 20,
            ),
            indices: BufferArena::new(
                "Mesh indices",
                wgpu::BufferUsages::INDEX,
                wgpu::COPY_BUFFER_ALIGNMENT,
                2 << 20,
            ),
            uniforms: BufferArena::new(
                "Mesh uniforms",
                wgpu::BufferUsages::UNIFORM,
                limits.min_uniform_buffer_offset_alignment.into(),
                256 << 10,
            ),
        }
    }

    fn end_frame(&self) {
        self.vertices.end_frame();
        self.indices.end_frame();
        self.uniforms.end_frame();
    }
}

impl WgpuRenderBackend<SwapChainTarget> {
    #[cfg(target_family = "wasm")]
    pub async fn for_canvas(
        canvas: web_sys::HtmlCanvasElement,
        webgpu: bool,
    ) -> Result<Self, Error> {
        let backends = if webgpu {
            wgpu::Backends::BROWSER_WEBGPU
        } else {
            wgpu::Backends::GL
        };
        let instance = create_wgpu_instance(
            backends,
            wgpu::BackendOptions {
                gl: wgpu::GlBackendOptions {
                    // See <https://github.com/gfx-rs/wgpu/releases/tag/v25.0.0>
                    fence_behavior: wgpu::GlFenceBehavior::AutoFinish,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        let surface = instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas))?;
        let (adapter, device, queue) = request_adapter_and_device(
            backends,
            &instance,
            Some(&surface),
            wgpu::PowerPreference::HighPerformance,
        )
        .await?;
        let descriptors = Descriptors::new(instance, adapter, device, queue);
        let target =
            SwapChainTarget::new(surface, &descriptors.adapter, (1, 1), &descriptors.device);
        Self::new(Arc::new(descriptors), target)
    }

    /// # Safety
    ///  See [`wgpu::SurfaceTargetUnsafe`] variants for safety requirements.
    ///
    /// Since wgpu 29, a display handle is needed at instance creation time:
    /// pass one via `display`, or make sure the `window` target carries a raw
    /// display handle (note that `SurfaceTargetUnsafe::from_window` does not
    /// provide one). Prefer passing `display` - some backends (e.g. GL via
    /// EGL) select their platform when the instance is created, before the
    /// target's display handle is seen.
    #[cfg(not(target_family = "wasm"))]
    pub unsafe fn for_window_unsafe(
        window: wgpu::SurfaceTargetUnsafe,
        size: (u32, u32),
        backend: wgpu::Backends,
        power_preference: wgpu::PowerPreference,
        display: Option<Box<dyn wgpu::wgt::WgpuHasDisplayHandle>>,
    ) -> Result<Self, Error> {
        if wgpu::Backends::SECONDARY.contains(backend) {
            tracing::warn!(
                "{} graphics backend support may not be fully supported.",
                format_list(&get_backend_names(backend), "and")
            );
        }
        let instance = create_wgpu_instance(backend, wgpu::BackendOptions::default(), display);
        let surface = unsafe { instance.create_surface_unsafe(window)? };
        let (adapter, device, queue) = futures::executor::block_on(request_adapter_and_device(
            backend,
            &instance,
            Some(&surface),
            power_preference,
        ))?;
        let descriptors = Descriptors::new(instance, adapter, device, queue);
        let target = SwapChainTarget::new(surface, &descriptors.adapter, size, &descriptors.device);
        Self::new(Arc::new(descriptors), target)
    }

    /// # Safety
    ///  See [`wgpu::SurfaceTargetUnsafe`] variants for safety requirements.
    #[cfg(not(target_family = "wasm"))]
    pub unsafe fn recreate_surface_unsafe(
        &mut self,
        window: wgpu::SurfaceTargetUnsafe,
        size: (u32, u32),
    ) -> Result<(), Error> {
        let descriptors = &self.descriptors;
        let surface = unsafe { descriptors.wgpu_instance.create_surface_unsafe(window)? };
        self.target =
            SwapChainTarget::new(surface, &descriptors.adapter, size, &descriptors.device);
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
impl WgpuRenderBackend<crate::target::TextureTarget> {
    pub fn for_offscreen(
        size: (u32, u32),
        backend: wgpu::Backends,
        power_preference: wgpu::PowerPreference,
    ) -> Result<Self, Error> {
        if wgpu::Backends::SECONDARY.contains(backend) {
            tracing::warn!(
                "{} graphics backend support may not be fully supported.",
                format_list(&get_backend_names(backend), "and")
            );
        }
        let instance = create_wgpu_instance(backend, wgpu::BackendOptions::default(), None);
        let (adapter, device, queue) = futures::executor::block_on(request_adapter_and_device(
            backend,
            &instance,
            None,
            power_preference,
        ))?;
        let descriptors = Descriptors::new(instance, adapter, device, queue);
        let target = crate::target::TextureTarget::new(&descriptors.device, size)?;
        Self::new(Arc::new(descriptors), target)
    }

    pub fn capture_frame(&self) -> Option<image::RgbaImage> {
        use crate::utils::buffer_to_image;
        if let Some(buffer) = &self.target.buffer {
            let (buffer, dimensions) = buffer.buffer.inner();
            Some(buffer_to_image(
                &self.descriptors.device,
                buffer,
                dimensions,
                None,
                self.target.size,
            ))
        } else {
            None
        }
    }
}

impl<T: RenderTarget> WgpuRenderBackend<T> {
    pub fn new(descriptors: Arc<Descriptors>, target: T) -> Result<Self, Error> {
        if target.width() > descriptors.limits.max_texture_dimension_2d
            || target.height() > descriptors.limits.max_texture_dimension_2d
        {
            return Err(format!(
                "Render target texture cannot be larger than {}px on either dimension (requested {} x {})",
                descriptors.limits.max_texture_dimension_2d,
                target.width(),
                target.height()
            )
                .into());
        }

        let surface = Surface::new(
            &descriptors,
            StageQuality::Low,
            target.width(),
            target.height(),
            target.format(),
        );

        let offscreen_buffer_pool = BufferPool::new(Box::new(
            |descriptors: &Descriptors, dimensions: &BufferDimensions| {
                descriptors.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: dimensions.size(),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            },
        ));

        let transforms = DynamicTransforms::new(&descriptors);
        let active_frame = ActiveFrame::new(&descriptors);

        let profiler_settings = GpuProfilerSettings {
            enable_timer_queries: cfg!(feature = "profile-with-tracy"),
            enable_debug_groups: cfg!(feature = "render_debug_labels"),
            ..Default::default()
        };
        #[cfg(feature = "profile-with-tracy")]
        let profiler = GpuProfiler::new_with_tracy_client(
            profiler_settings,
            descriptors.backend,
            &descriptors.device,
            &descriptors.queue,
        )?;
        #[cfg(not(feature = "profile-with-tracy"))]
        let profiler = GpuProfiler::new(&descriptors.device, profiler_settings)?;
        let mesh_buffers = MeshBuffers::new(&descriptors.limits);

        Ok(Self {
            descriptors,
            target,
            surface,
            meshes: Vec::new(),
            shape_tessellator: ShapeTessellator::new(),
            viewport_scale_factor: 1.0,
            texture_pool: TexturePool::new(),
            offscreen_texture_pool: TexturePool::new(),
            offscreen_buffer_pool: Arc::new(offscreen_buffer_pool),
            dynamic_transforms: transforms,
            active_frame,
            profiler,
            mesh_buffers,
            dropped_textures: Default::default(),
        })
    }

    fn register_shape_internal(
        &mut self,
        shape: DistilledShape,
        bitmap_source: &dyn BitmapSource,
        scale: f32,
    ) -> Mesh {
        crate::stats::count(&crate::stats::MESHES_CREATED);
        let shape_id = shape.id;
        let flat = shape.flat;
        let lyon_mesh =
            self.shape_tessellator
                .tessellate_shape_with_scale(shape, bitmap_source, scale);
        let bounds = vertex_bounds(&lyon_mesh);

        let mut draws = Vec::with_capacity(lyon_mesh.draws.len());
        let mut uniform_buffer = BufferBuilder::new(
            &self.descriptors.limits,
            self.descriptors.limits.min_uniform_buffer_offset_alignment,
        );
        let mut vertex_buffer = BufferBuilder::new(&self.descriptors.limits, 0);
        let mut index_buffer = BufferBuilder::new(&self.descriptors.limits, 0);
        let mut gradients = Vec::with_capacity(lyon_mesh.gradients.len());

        for gradient in lyon_mesh.gradients {
            gradients.push(CommonGradient::new(
                &self.descriptors,
                gradient,
                &mut uniform_buffer,
            ));
        }

        let index_format = if lyon_mesh
            .draws
            .iter()
            .all(|draw| draw.vertices.len() <= usize::from(u16::MAX) + 1)
        {
            wgpu::IndexFormat::Uint16
        } else {
            wgpu::IndexFormat::Uint32
        };
        for draw in lyon_mesh.draws {
            let draw_id = draws.len();
            if let Some(draw) = PendingDraw::new(
                self,
                bitmap_source,
                draw,
                shape_id,
                draw_id,
                &mut vertex_buffer,
                &mut index_buffer,
                index_format,
            ) {
                draws.push(draw);
            }
        }

        let device = &self.descriptors.device;
        let queue = &self.descriptors.queue;
        let uniforms = (!uniform_buffer.bytes().is_empty()).then(|| {
            self.mesh_buffers
                .uniforms
                .allocate(device, queue, uniform_buffer.bytes())
        });
        let vertices = self
            .mesh_buffers
            .vertices
            .allocate(device, queue, vertex_buffer.bytes());
        let indices = self
            .mesh_buffers
            .indices
            .allocate(device, queue, index_buffer.bytes());

        let draws = draws
            .into_iter()
            .map(|d| {
                d.finish(
                    &self.descriptors,
                    vertices.offset(),
                    indices.offset(),
                    uniforms.as_ref(),
                    &gradients,
                )
            })
            .collect();

        Mesh {
            draws,
            vertex_buffer: vertices.buffer().clone(),
            index_buffer: indices.buffer().clone(),
            index_format,
            _allocations: [Some(vertices), Some(indices), uniforms],
            #[cfg(feature = "stats")]
            _gradients: gradients,
            flat,
            bounds,
        }
    }

    fn clamp_bitmap(&self, bitmap: &mut Bitmap) -> bool {
        let max_size = self.descriptors.limits.max_texture_dimension_2d;
        if bitmap.width() > max_size || bitmap.height() > max_size {
            let image =
                image::RgbaImage::from_raw(bitmap.width(), bitmap.height(), bitmap.data().to_vec())
                    .expect("Width and height of bitmap must match bitmap data");

            let ratio = bitmap.width() as f32 / bitmap.height() as f32;
            let mut width = bitmap.width();
            let mut height = bitmap.height();
            if width > max_size {
                width = max_size;
                height = (max_size as f32 / ratio) as u32;
            }
            if height > max_size {
                height = max_size;
                width = (max_size as f32 * ratio) as u32;
            }
            let resized = image::imageops::resize(&image, width, height, FilterType::CatmullRom);
            *bitmap = Bitmap::new(width, height, BitmapFormat::Rgba, resized.into_raw());
            true
        } else {
            false
        }
    }

    pub fn descriptors(&self) -> &Arc<Descriptors> {
        &self.descriptors
    }

    pub fn target(&self) -> &T {
        &self.target
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.descriptors.device
    }

    pub fn make_queue_sync_handle(
        &self,
        target: TextureTarget,
        index: Option<SubmissionIndex>,
        destination: BitmapHandle,
        copy_area: PixelRegion,
    ) -> Box<QueueSyncHandle> {
        match target.take_buffer() {
            None => Box::new(QueueSyncHandle::NotCopied {
                handle: destination,
                copy_area,
                descriptors: self.descriptors.clone(),
                pool: self.offscreen_buffer_pool.clone(),
            }),
            Some(TextureBufferInfo {
                buffer: MaybeOwnedBuffer::Borrowed(buffer, copy_dimensions),
                ..
            }) => Box::new(QueueSyncHandle::AlreadyCopied {
                index,
                buffer,
                copy_dimensions,
                descriptors: self.descriptors.clone(),
            }),
            Some(TextureBufferInfo {
                buffer: MaybeOwnedBuffer::Owned(..),
                ..
            }) => unreachable!("Buffer must be Borrowed as it was set to be Borrowed earlier"),
        }
    }

    fn draw_cache_atlas(&mut self, atlas: CacheAtlas) {
        let mut entries = atlas.entries;
        entries.sort_by_key(|entry| std::cmp::Reverse(as_texture(&entry.handle).texture.height()));
        let mut page = Vec::new();
        let (mut x, mut y, mut shelf) = (0, 0, 0);
        for entry in entries {
            let texture = &as_texture(&entry.handle).texture;
            let width = texture.width() + 2 * CACHE_ATLAS_GAP;
            let height = texture.height() + 2 * CACHE_ATLAS_GAP;
            if x + width > CACHE_ATLAS_SIZE {
                (x, y, shelf) = (0, y + shelf, 0);
            }
            if y + height > CACHE_ATLAS_SIZE {
                self.draw_cache_atlas_page(std::mem::take(&mut page));
                (x, y, shelf) = (0, 0, 0);
            }
            page.push((entry, (x + CACHE_ATLAS_GAP, y + CACHE_ATLAS_GAP)));
            x += width;
            shelf = shelf.max(height);
        }
        if !page.is_empty() {
            self.draw_cache_atlas_page(page);
        }
    }

    fn draw_cache_atlas_page(&mut self, mut page: Vec<(BitmapCacheEntry, (u32, u32))>) {
        let mut commands = CommandList::new();
        for (entry, (x, y)) in &mut page {
            offset_commands(
                &mut entry.commands,
                swf::Twips::from_pixels_i32(*x as i32),
                swf::Twips::from_pixels_i32(*y as i32),
            );
            commands.commands.append(&mut entry.commands.commands);
        }
        let surface = Surface::new(
            &self.descriptors,
            self.surface.quality(),
            CACHE_ATLAS_SIZE,
            CACHE_ATLAS_SIZE,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let atlas = surface.draw_commands(
            RenderTargetMode::FreshWithColor(wgpu::Color::TRANSPARENT),
            &self.descriptors,
            &self.meshes,
            commands,
            &mut self.active_frame.staging_belt,
            &self.dynamic_transforms,
            &mut self
                .profiler
                .scope("Draw to CAB atlas", &mut self.active_frame.command_encoder),
            LayerRef::None,
            &mut self.offscreen_texture_pool,
        );
        atlas.ensure_cleared(&mut self.active_frame.command_encoder);

        let blur = &self.descriptors.filters.blur;
        let batched =
            |filter: &swf::BlurFilter| blur.is_single_pass(filter) || blur.is_two_pass(filter);
        let (blurred, page): (Vec<_>, Vec<_>) = page.into_iter().partition(|(entry, _)| {
            matches!(entry.filters.as_slice(), [Filter::BlurFilter(filter)] if batched(filter))
        });
        let (glowed, page): (Vec<_>, Vec<_>) = page.into_iter().partition(|(entry, _)| {
            matches!(entry.filters.as_slice(), [Filter::GlowFilter(filter)] if batched(&filter.inner_blur_filter()))
        });
        let inner_blurs: Vec<_> = glowed
            .iter()
            .map(|(entry, _)| match &entry.filters[0] {
                Filter::GlowFilter(filter) => filter.inner_blur_filter(),
                _ => unreachable!("Only glowing entries"),
            })
            .collect();
        let blurs = blurred
            .iter()
            .map(|(entry, point)| match &entry.filters[0] {
                Filter::BlurFilter(filter) => (
                    atlas_part(&atlas, *point, as_texture(&entry.handle)),
                    filter,
                ),
                _ => unreachable!("Only blurred entries"),
            })
            .chain(
                glowed
                    .iter()
                    .zip(&inner_blurs)
                    .map(|((entry, point), filter)| {
                        (
                            atlas_part(&atlas, *point, as_texture(&entry.handle)),
                            filter,
                        )
                    }),
            );
        let (single, double): (Vec<_>, Vec<_>) =
            blurs.partition(|(_, filter)| blur.is_single_pass(filter));
        if single.is_empty() && double.is_empty() {
            return self.filter_parts(&atlas, page);
        }

        let new_target = |this: &mut Self| {
            CommandTarget::new(
                &this.descriptors,
                &mut this.offscreen_texture_pool,
                atlas.color_texture().size(),
                atlas.color_texture().format(),
                1,
                RenderTargetMode::FreshWithColor(wgpu::Color::TRANSPARENT),
                &mut this.active_frame.command_encoder,
            )
        };
        let blur_target = new_target(self);
        if !double.is_empty() {
            let steps = new_target(self);
            self.descriptors.filters.blur.apply_two_pass_in_place(
                &self.descriptors,
                &mut self
                    .profiler
                    .scope("Blur CAB atlas", &mut self.active_frame.command_encoder),
                [&steps, &blur_target],
                &double,
            );
        }
        if !single.is_empty() {
            self.descriptors.filters.blur.apply_single_pass_in_place(
                &self.descriptors,
                &mut self
                    .profiler
                    .scope("Blur CAB atlas", &mut self.active_frame.command_encoder),
                &blur_target,
                &single,
            );
        }
        self.copy_parts(blur_target.color_texture(), &blurred);

        if !glowed.is_empty() {
            let glow_target = new_target(self);
            let glows: Vec<_> = glowed
                .iter()
                .map(|(entry, point)| {
                    let Filter::GlowFilter(filter) = &entry.filters[0] else {
                        unreachable!("Only glowing entries")
                    };
                    let source = atlas_part(&atlas, *point, as_texture(&entry.handle));
                    let blurred = FilterSource {
                        texture: blur_target.color_texture(),
                        view: blur_target.color_view(),
                        ..source
                    };
                    (source, blurred, filter)
                })
                .collect();
            self.descriptors.filters.glow.apply_in_place(
                &self.descriptors,
                &mut self
                    .profiler
                    .scope("Glow CAB atlas", &mut self.active_frame.command_encoder),
                &glow_target,
                &glows,
            );
            self.copy_parts(glow_target.color_texture(), &glowed);
        }

        self.filter_parts(&atlas, page);
    }

    fn filter_parts(&mut self, atlas: &CommandTarget, page: Vec<(BitmapCacheEntry, (u32, u32))>) {
        for (entry, point) in page {
            let texture = as_texture(&entry.handle);
            if entry.filters.is_empty() {
                copy_part(
                    &mut self.active_frame.command_encoder,
                    atlas.color_texture(),
                    point,
                    texture,
                );
            } else {
                filter_into_cache(
                    &self.descriptors,
                    &mut self
                        .profiler
                        .scope("Filters", &mut self.active_frame.command_encoder),
                    &mut self.offscreen_texture_pool,
                    &mut self.active_frame.staging_belt,
                    atlas_part(atlas, point, texture),
                    entry.filters,
                    texture,
                );
            }
            self.active_frame.maybe_flush(&self.descriptors);
        }
    }

    fn copy_parts(&mut self, from: &wgpu::Texture, entries: &[(BitmapCacheEntry, (u32, u32))]) {
        for (entry, point) in entries {
            copy_part(
                &mut self.active_frame.command_encoder,
                from,
                *point,
                as_texture(&entry.handle),
            );
        }
        self.active_frame.maybe_flush(&self.descriptors);
    }
}

const CACHE_ATLAS_SIZE: u32 = 1024;

// A cache's texture cuts off anything drawn past its bounds; in the atlas, this
// keeps a stray pixel or two out of the neighbouring caches.
const CACHE_ATLAS_GAP: u32 = 2;

#[derive(Default)]
struct CacheAtlas {
    entries: Vec<BitmapCacheEntry>,
}

impl CacheAtlas {
    fn try_add(&mut self, entry: BitmapCacheEntry) -> Result<(), BitmapCacheEntry> {
        if entry.clear.a != 0
            || entry.commands.commands.iter().any(|command| {
                matches!(
                    command,
                    Command::Blend(..) | Command::RenderAlphaMask { .. }
                )
            })
        {
            return Err(entry);
        }
        if !entry.filters.first().is_none_or(|filter| {
            matches!(
                filter,
                Filter::BlurFilter(_) | Filter::GlowFilter(_) | Filter::ColorMatrixFilter(_)
            )
        }) {
            return Err(entry);
        }
        let texture = &as_texture(&entry.handle).texture;
        if texture.width() + 2 * CACHE_ATLAS_GAP > CACHE_ATLAS_SIZE
            || texture.height() + 2 * CACHE_ATLAS_GAP > CACHE_ATLAS_SIZE
        {
            return Err(entry);
        }
        self.entries.push(entry);
        Ok(())
    }

    fn is_drawn_by(&self, commands: &CommandList) -> bool {
        draws_bitmap(commands, &|bitmap| {
            self.entries.iter().any(|entry| entry.handle == *bitmap)
        })
    }

    fn draws(&self, handle: &BitmapHandle) -> bool {
        self.entries
            .iter()
            .any(|entry| draws_bitmap(&entry.commands, &|bitmap| bitmap == handle))
    }
}

fn draws_bitmap(commands: &CommandList, matches: &impl Fn(&BitmapHandle) -> bool) -> bool {
    commands.commands.iter().any(|command| match command {
        Command::RenderBitmap { bitmap, .. } | Command::RenderStage3D { bitmap, .. } => {
            matches(bitmap)
        }
        Command::RenderAlphaMask {
            maskee_commands,
            mask_commands,
        } => draws_bitmap(maskee_commands, matches) || draws_bitmap(mask_commands, matches),
        Command::Blend(commands, _) => draws_bitmap(commands, matches),
        _ => false,
    })
}

fn offset_commands(commands: &mut CommandList, x: swf::Twips, y: swf::Twips) {
    for command in &mut commands.commands {
        match command {
            Command::RenderBitmap { transform, .. }
            | Command::RenderStage3D { transform, .. }
            | Command::RenderShape { transform, .. } => {
                transform.matrix.tx += x;
                transform.matrix.ty += y;
            }
            Command::DrawRect { matrix, .. }
            | Command::DrawLine { matrix, .. }
            | Command::DrawLineRect { matrix, .. } => {
                matrix.tx += x;
                matrix.ty += y;
            }
            Command::RenderAlphaMask {
                maskee_commands,
                mask_commands,
            } => {
                offset_commands(maskee_commands, x, y);
                offset_commands(mask_commands, x, y);
            }
            Command::Blend(commands, _) => offset_commands(commands, x, y),
            Command::PushMask
            | Command::ActivateMask
            | Command::DeactivateMask
            | Command::PopMask => {}
        }
    }
}

fn atlas_part<'a>(
    atlas: &'a CommandTarget,
    point: (u32, u32),
    cache: &Texture,
) -> FilterSource<'a> {
    FilterSource {
        texture: atlas.color_texture(),
        view: atlas.color_view(),
        point,
        size: (cache.texture.width(), cache.texture.height()),
        clamp_to_rect: true,
    }
}

fn copy_part(
    encoder: &mut wgpu::CommandEncoder,
    from: &wgpu::Texture,
    point: (u32, u32),
    cache: &Texture,
) {
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: from,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: point.0,
                y: point.1,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        cache.texture.as_image_copy(),
        cache.texture.size(),
    );
}

fn filter_into_cache(
    descriptors: &Descriptors,
    scope: &mut Scope<'_, wgpu::CommandEncoder>,
    texture_pool: &mut TexturePool,
    staging_belt: &mut wgpu::util::StagingBelt,
    source: FilterSource,
    filters: Vec<Filter>,
    cache: &Texture,
) {
    let last = filters.len() - 1;
    let mut filters = filters.into_iter().enumerate();
    let Some((_, first)) = filters.next() else {
        return;
    };
    let mut target = descriptors.filters.apply(
        descriptors,
        &mut scope.scope(first.name()),
        texture_pool,
        staging_belt,
        source,
        first,
        (last == 0).then_some(cache),
    );
    for (i, filter) in filters {
        target = descriptors.filters.apply(
            descriptors,
            &mut scope.scope(filter.name()),
            texture_pool,
            staging_belt,
            FilterSource::for_entire_texture(target.color_texture(), target.color_view()),
            filter,
            (i == last).then_some(cache),
        );
    }
    let filtered = target.color_texture();
    if *filtered == cache.texture {
        return;
    }
    if filtered.sample_count() == cache.texture.sample_count()
        && filtered.format() == cache.texture.format()
        && filtered.size() == cache.texture.size()
    {
        scope.copy_texture_to_texture(
            filtered.as_image_copy(),
            cache.texture.as_image_copy(),
            cache.texture.size(),
        );
    } else {
        run_copy_pipeline(
            descriptors,
            cache.texture.format(),
            cache.view(),
            target.color_view(),
            target.globals(),
            target.color_texture().sample_count(),
            &mut scope.scope("Copy filtered to CAB"),
        );
    }
}

impl<T: RenderTarget + 'static> RenderBackend for WgpuRenderBackend<T> {
    fn set_viewport_dimensions(&mut self, dimensions: ViewportDimensions) {
        // Avoid panics from creating 0-sized framebuffers.
        // TODO: find a way to bubble an error when the size is too large
        let width = std::cmp::max(
            std::cmp::min(
                dimensions.width,
                self.descriptors.limits.max_texture_dimension_2d,
            ),
            1,
        );
        let height = std::cmp::max(
            std::cmp::min(
                dimensions.height,
                self.descriptors.limits.max_texture_dimension_2d,
            ),
            1,
        );
        self.target.resize(&self.descriptors.device, width, height);

        self.surface = Surface::new(
            &self.descriptors,
            self.surface.quality(),
            width,
            height,
            self.target.format(),
        );

        self.viewport_scale_factor = dimensions.scale_factor;
        // Only `submit_frame` uses this pool, and it submits what it records.
        self.texture_pool.destroy_idle_textures();
        self.texture_pool = TexturePool::new();
    }

    fn create_context3d(
        &mut self,
        profile: Context3DProfile,
    ) -> Result<Box<dyn Context3D>, BitmapError> {
        Ok(Box::new(WgpuContext3D::new(
            self.descriptors.clone(),
            profile,
        )))
    }

    fn debug_info(&self) -> Cow<'static, str> {
        let mut result = vec![];
        result.push("Renderer: wgpu".to_string());

        let info = self.descriptors.adapter.get_info();
        result.push(format!("Adapter Backend: {:?}", info.backend));
        result.push(format!("Adapter Name: {:?}", info.name));
        result.push(format!("Adapter Device Type: {:?}", info.device_type));
        result.push(format!("Adapter Driver Name: {:?}", info.driver));
        result.push(format!("Adapter Driver Info: {:?}", info.driver_info));

        let enabled_features = self.descriptors.device.features();
        let available_features = self.descriptors.adapter.features() - enabled_features;
        let current_limits = &self.descriptors.limits;

        result.push(format!("Enabled features: {enabled_features:?}"));
        result.push(format!("Available features: {available_features:?}"));
        result.push(format!("Current limits: {current_limits:?}"));
        result.push(format!("Surface quality: {}", self.surface.quality()));
        result.push(format!("Surface samples: {}", self.surface.sample_count()));
        result.push(format!("Surface size: {:?}", self.surface.size()));

        Cow::Owned(result.join("\n"))
    }

    fn name(&self) -> &'static str {
        if cfg!(target_family = "wasm") {
            let info = self.descriptors.adapter.get_info();
            if info.backend == wgpu::Backend::BrowserWebGpu {
                "webgpu"
            } else {
                "wgpu-webgl"
            }
        } else {
            "wgpu"
        }
    }

    fn set_quality(&mut self, quality: StageQuality) {
        self.surface = Surface::new(
            &self.descriptors,
            quality,
            self.surface.size().width,
            self.surface.size().height,
            self.target.format(),
        );
    }

    fn viewport_dimensions(&self) -> ViewportDimensions {
        ViewportDimensions {
            width: self.target.width(),
            height: self.target.height(),
            scale_factor: self.viewport_scale_factor,
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn register_shape(
        &mut self,
        shape: DistilledShape,
        bitmap_source: &dyn BitmapSource,
    ) -> ShapeHandle {
        let mesh = self.register_shape_internal(shape, bitmap_source, 1.0);
        ShapeHandle(Arc::new(mesh))
    }

    #[instrument(level = "debug", skip_all)]
    fn register_shape_with_scale(
        &mut self,
        shape: DistilledShape,
        bitmap_source: &dyn BitmapSource,
        scale: f32,
    ) -> ShapeHandle {
        let mesh = self.register_shape_internal(shape, bitmap_source, scale);
        ShapeHandle(Arc::new(mesh))
    }

    #[instrument(level = "debug", skip_all)]
    fn submit_frame(
        &mut self,
        clear: Color,
        commands: CommandList,
        cache_entries: Vec<BitmapCacheEntry>,
    ) {
        let Some(frame_output) = self.target.get_next_texture() else {
            // Attempt to recreate the swap chain in this case.
            self.target.resize(
                &self.descriptors.device,
                self.target.width(),
                self.target.height(),
            );
            return;
        };

        // Caches drawn into the atlas are finished later than their turn, so a
        // cache that another one draws mustn't be deferred past it.
        let mut atlas = CacheAtlas::default();
        for entry in cache_entries {
            crate::stats::count(&crate::stats::CACHE_DRAWS);
            if atlas.is_drawn_by(&entry.commands) {
                self.draw_cache_atlas(std::mem::take(&mut atlas));
            }
            let Err(entry) = atlas.try_add(entry) else {
                continue;
            };
            if atlas.draws(&entry.handle) {
                self.draw_cache_atlas(std::mem::take(&mut atlas));
            }
            let texture = as_texture(&entry.handle);
            let surface = Surface::new(
                &self.descriptors,
                self.surface.quality(),
                texture.texture.width(),
                texture.texture.height(),
                wgpu::TextureFormat::Rgba8Unorm,
            );
            if entry.filters.is_empty() {
                surface.draw_commands(
                    RenderTargetMode::ExistingWithColor(
                        texture.texture.clone(),
                        texture.view().clone(),
                        wgpu::Color {
                            r: f64::from(entry.clear.r) / 255.0,
                            g: f64::from(entry.clear.g) / 255.0,
                            b: f64::from(entry.clear.b) / 255.0,
                            a: f64::from(entry.clear.a) / 255.0,
                        },
                    ),
                    &self.descriptors,
                    &self.meshes,
                    entry.commands,
                    &mut self.active_frame.staging_belt,
                    &self.dynamic_transforms,
                    &mut self
                        .profiler
                        .scope("Draw to CAB", &mut self.active_frame.command_encoder),
                    LayerRef::None,
                    &mut self.offscreen_texture_pool,
                );
            } else {
                let mut scope = self
                    .profiler
                    .scope("Filters", &mut self.active_frame.command_encoder);
                // The content goes in a pooled texture rather than the cache,
                // so the last filter can render straight into the cache (a
                // one-pass blur has to read somewhere else).
                let target = surface.draw_commands(
                    RenderTargetMode::FreshWithColor(wgpu::Color {
                        r: f64::from(entry.clear.r) / 255.0,
                        g: f64::from(entry.clear.g) / 255.0,
                        b: f64::from(entry.clear.b) / 255.0,
                        a: f64::from(entry.clear.a) / 255.0,
                    }),
                    &self.descriptors,
                    &self.meshes,
                    entry.commands,
                    &mut self.active_frame.staging_belt,
                    &self.dynamic_transforms,
                    &mut scope.scope("Draw to CAB"),
                    LayerRef::None,
                    &mut self.offscreen_texture_pool,
                );
                filter_into_cache(
                    &self.descriptors,
                    &mut scope,
                    &mut self.offscreen_texture_pool,
                    &mut self.active_frame.staging_belt,
                    FilterSource::for_entire_texture(target.color_texture(), target.color_view()),
                    entry.filters,
                    texture,
                );
            }
            // Periodically flush GPU work to prevent OOM when many cache entries
            // accumulate (e.g. when a large container's cacheAsBitmap is skipped
            // but its hundreds of children each have their own bitmap caches).
            self.active_frame.maybe_flush(&self.descriptors);
        }
        self.draw_cache_atlas(atlas);

        self.surface.draw_commands_and_copy_to(
            frame_output.view(),
            RenderTargetMode::FreshWithColor(wgpu::Color {
                r: f64::from(clear.r) / 255.0,
                g: f64::from(clear.g) / 255.0,
                b: f64::from(clear.b) / 255.0,
                a: f64::from(clear.a) / 255.0,
            }),
            &self.descriptors,
            &mut self.active_frame.staging_belt,
            &self.dynamic_transforms,
            &mut self
                .profiler
                .scope("Frame commands", &mut self.active_frame.command_encoder),
            &self.meshes,
            commands,
            LayerRef::None,
            &mut self.texture_pool,
        );
        self.profiler
            .resolve_queries(&mut self.active_frame.command_encoder);
        self.active_frame.staging_belt.finish();

        self.active_frame
            .submit_for_target(&self.descriptors, &self.target, frame_output);
        self.offscreen_texture_pool
            .end_frame(OFFSCREEN_TEXTURE_MAX_IDLE_FRAMES);
        self.texture_pool.end_frame(TEXTURE_MAX_IDLE_FRAMES);
        self.mesh_buffers.end_frame();
        self.dropped_textures.destroy();
        crate::stats::set_with(&crate::stats::POOL_BYTES, || {
            self.texture_pool.idle_bytes() as i64
        });
        crate::stats::set_with(&crate::stats::OFFSCREEN_POOL_BYTES, || {
            self.offscreen_texture_pool.idle_bytes() as i64
        });
        self.profiler
            .end_frame()
            .expect("Frame should end successfully");
        let timestamp_period = self.descriptors.queue.get_timestamp_period();
        self.profiler.process_finished_frame(timestamp_period);
    }

    #[instrument(level = "debug", skip_all)]
    fn register_bitmap(&mut self, bitmap: Bitmap<'_>) -> Result<BitmapHandle, BitmapError> {
        crate::stats::count(&crate::stats::TEXTURE_UPLOADS);
        let mut bitmap = bitmap.to_rgba();

        self.clamp_bitmap(&mut bitmap);

        let extent = wgpu::Extent3d {
            width: bitmap.width(),
            height: bitmap.height(),
            depth_or_array_layers: 1,
        };

        let texture_label = create_debug_label!("Bitmap");
        let texture = self
            .descriptors
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: texture_label.as_deref(),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
            });

        self.descriptors.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: Default::default(),
                aspect: wgpu::TextureAspect::All,
            },
            bitmap.data(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * extent.width),
                rows_per_image: None,
            },
            extent,
        );

        let bytes = u64::from(extent.width) * u64::from(extent.height) * 4;
        let handle = BitmapHandle(Arc::new(Texture {
            texture,
            view: Default::default(),
            repeating_linear: Default::default(),
            repeating_nearest: Default::default(),
            clamped_linear: Default::default(),
            clamped_nearest: Default::default(),
            copy_count: Cell::new(0),
            dropped_textures: Some(self.dropped_textures.clone()),
            _live: crate::stats::Live::with_bytes(
                &crate::stats::LIVE_BITMAP_TEXTURES,
                &crate::stats::LIVE_BITMAP_BYTES,
                bytes,
            ),
        }));

        Ok(handle)
    }

    #[instrument(level = "debug", skip_all)]
    fn update_texture(
        &mut self,
        handle: &BitmapHandle,
        bitmap: Bitmap<'_>,
        mut region: PixelRegion,
    ) -> Result<(), BitmapError> {
        crate::stats::count(&crate::stats::TEXTURE_UPLOADS);
        if region.width() == 0 || region.height() == 0 {
            // Nothing to do. It's important to bail out now, as the
            // write_texture call panics when the source buffer is of zero size.
            return Ok(());
        }

        let texture = as_texture(handle);

        let mut bitmap = bitmap.to_rgba();
        if self.clamp_bitmap(&mut bitmap) {
            // If we're updating a resized texture, just redo the whole thing.
            // We can't trivially map pixel regions as we use a filter to resize.
            region = PixelRegion::for_whole_size(bitmap.width(), bitmap.height());
        }

        let extent = wgpu::Extent3d {
            width: region.width(),
            height: region.height(),
            depth_or_array_layers: 1,
        };

        self.active_frame.submit_direct(&self.descriptors);
        self.descriptors.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x_min,
                    y: region.y_min,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &bitmap.data()[(region.y_min * texture.texture.width() * 4) as usize
                ..(region.y_max * texture.texture.width() * 4) as usize],
            wgpu::TexelCopyBufferLayout {
                offset: (region.x_min * 4) as wgpu::BufferAddress,
                bytes_per_row: Some(4 * texture.texture.width()),
                rows_per_image: None,
            },
            extent,
        );

        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    fn render_offscreen(
        &mut self,
        handle: BitmapHandle,
        commands: CommandList,
        quality: StageQuality,
        bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        crate::stats::count(&crate::stats::OFFSCREEN_DRAWS);
        let texture = as_texture(&handle);

        let extent = wgpu::Extent3d {
            width: texture.texture.width(),
            height: texture.texture.height(),
            depth_or_array_layers: 1,
        };

        let mut target = TextureTarget {
            size: extent,
            texture: texture.texture.clone(),
            format: wgpu::TextureFormat::Rgba8Unorm,
            buffer: None,
        };

        let frame_output = target
            .get_next_texture()
            .expect("TextureTargetFrame.get_next_texture is infallible");

        let surface = Surface::new(
            &self.descriptors,
            quality,
            texture.texture.width(),
            texture.texture.height(),
            wgpu::TextureFormat::Rgba8Unorm,
        );
        surface.draw_commands_and_copy_to(
            frame_output.view(),
            RenderTargetMode::FreshWithTexture(target.get_texture()),
            &self.descriptors,
            &mut self.active_frame.staging_belt,
            &self.dynamic_transforms,
            &mut self
                .profiler
                .scope("Offscreen commands", &mut self.active_frame.command_encoder),
            &self.meshes,
            commands,
            LayerRef::Current,
            &mut self.offscreen_texture_pool,
        );

        self.active_frame.maybe_flush(&self.descriptors);
        Some(self.make_queue_sync_handle(target, None, handle, bounds))
    }

    fn is_filter_supported(&self, filter: &Filter) -> bool {
        matches!(
            filter,
            Filter::BlurFilter(_)
                | Filter::GlowFilter(_)
                | Filter::DropShadowFilter(_)
                | Filter::ColorMatrixFilter(_)
                | Filter::ShaderFilter(_)
                | Filter::BevelFilter(_)
                | Filter::DisplacementMapFilter(_)
        )
    }

    fn is_offscreen_supported(&self) -> bool {
        true
    }

    fn apply_filter(
        &mut self,
        source: BitmapHandle,
        source_point: (u32, u32),
        source_size: (u32, u32),
        destination: BitmapHandle,
        dest_point: (i32, i32),
        filter: Filter,
    ) -> Option<Box<dyn SyncHandle>> {
        let source_texture = as_texture(&source);
        let dest_texture = as_texture(&destination);

        let copy_area = PixelRegion::for_whole_size(
            dest_texture.texture.width(),
            dest_texture.texture.height(),
        );

        let target = TextureTarget {
            size: wgpu::Extent3d {
                width: dest_texture.texture.width(),
                height: dest_texture.texture.height(),
                depth_or_array_layers: 1,
            },
            texture: dest_texture.texture.clone(),
            format: wgpu::TextureFormat::Rgba8Unorm,
            buffer: None,
        };

        let applied_filter = self.descriptors.filters.apply(
            &self.descriptors,
            &mut self.active_frame.command_encoder,
            &mut self.offscreen_texture_pool,
            &mut self.active_frame.staging_belt,
            FilterSource {
                texture: &source_texture.texture,
                view: source_texture.view(),
                point: source_point,
                size: source_size,
                clamp_to_rect: false,
            },
            filter,
            None,
        );

        let (dest_x, dest_y) = dest_point;

        let src_offset_x = dest_x.min(0).unsigned_abs();
        let src_offset_y = dest_y.min(0).unsigned_abs();

        let final_dest_x = dest_x.max(0) as u32;
        let final_dest_y = dest_y.max(0) as u32;

        let available_width = applied_filter.width().saturating_sub(src_offset_x);
        let available_height = applied_filter.height().saturating_sub(src_offset_y);
        let dest_available_width = dest_texture.texture.width().saturating_sub(final_dest_x);
        let dest_available_height = dest_texture.texture.height().saturating_sub(final_dest_y);

        let copy_width = available_width.min(dest_available_width);
        let copy_height = available_height.min(dest_available_height);

        if copy_width == 0 || copy_height == 0 {
            return None;
        }

        self.active_frame.command_encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: applied_filter.color_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: src_offset_x,
                    y: src_offset_y,
                    z: 0,
                },
                aspect: Default::default(),
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dest_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: final_dest_x,
                    y: final_dest_y,
                    z: 0,
                },
                aspect: Default::default(),
            },
            wgpu::Extent3d {
                width: copy_width,
                height: copy_height,
                depth_or_array_layers: 1,
            },
        );

        self.active_frame.maybe_flush(&self.descriptors);
        Some(self.make_queue_sync_handle(target, None, destination, copy_area))
    }

    fn compile_pixelbender_shader(
        &mut self,
        shader: PixelBenderShader,
    ) -> Result<PixelBenderShaderHandle, BitmapError> {
        self.compile_pixelbender_shader_impl(shader)
    }

    fn run_pixelbender_shader(
        &mut self,
        shader: PixelBenderShaderHandle,
        arguments: &[PixelBenderShaderArgument],
        target: &PixelBenderTarget,
    ) -> Result<PixelBenderOutput, BitmapError> {
        let output_channels = shader
            .0
            .parsed_shader()
            .output_channels()
            .expect("No output parameter");
        let has_padding = output_channels == 3;

        let texture_format =
            crate::pixel_bender::temporary_texture_format_for_channels(output_channels as u32);

        let target_handle = match target {
            PixelBenderTarget::Bitmap(handle) => handle.clone(),
            PixelBenderTarget::Bytes { width, height } => {
                let extent = wgpu::Extent3d {
                    width: *width,
                    height: *height,
                    depth_or_array_layers: 1,
                };
                // FIXME - cache this texture somehow. We might also want to consider using
                // a compute shader
                let texture_label = create_debug_label!("Temporary pixelbender output texture");
                let texture = self
                    .descriptors
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: texture_label.as_deref(),
                        size: extent,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: texture_format,
                        view_formats: &[texture_format],
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST
                            | wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC,
                    });
                BitmapHandle(Arc::new(Texture {
                    texture,
                    view: Default::default(),
                    repeating_linear: Default::default(),
                    repeating_nearest: Default::default(),
                    clamped_linear: Default::default(),
                    clamped_nearest: Default::default(),
                    copy_count: Cell::new(0),
                    dropped_textures: Some(self.dropped_textures.clone()),
                    _live: crate::stats::Live::new(&crate::stats::LIVE_BITMAP_TEXTURES),
                }))
            }
        };

        let target_texture = as_texture(&target_handle);

        let extent = wgpu::Extent3d {
            width: target_texture.texture.width(),
            height: target_texture.texture.height(),
            depth_or_array_layers: 1,
        };

        let copy_dimensions = BufferDimensions::new(
            target_texture.texture.width() as usize,
            target_texture.texture.height() as usize,
            target_texture.texture.format(),
        );
        let buffer_info = Some(TextureBufferInfo {
            buffer: MaybeOwnedBuffer::Borrowed(
                self.offscreen_buffer_pool
                    .take(&self.descriptors, copy_dimensions.clone()),
                copy_dimensions,
            ),
            copy_area: PixelRegion::for_whole_size(
                target_texture.texture.width(),
                target_texture.texture.height(),
            ),
        });

        let mut texture_target = TextureTarget {
            size: extent,
            texture: target_texture.texture.clone(),
            format: target_texture.texture.format(),
            buffer: buffer_info,
        };

        let frame_output = texture_target
            .get_next_texture()
            .expect("TextureTargetFrame.get_next_texture is infallible");

        run_pixelbender_shader_impl(
            &self.descriptors,
            shader,
            ShaderMode::ShaderJob,
            arguments,
            &target_texture.texture,
            &mut self.active_frame.command_encoder,
            Some(wgpu::RenderPassColorAttachment {
                view: frame_output.view(),
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            }),
            1,
            // When running a standalone shader, we always process the entire image
            &FilterSource::for_entire_texture(&target_texture.texture, target_texture.view()),
        )?;

        let index = Some(self.active_frame.submit_for_target(
            &self.descriptors,
            &texture_target,
            frame_output,
        ));

        let sync_handle = self.make_queue_sync_handle(
            texture_target,
            index,
            target_handle,
            PixelRegion::for_whole_size(extent.width, extent.height),
        );

        match target {
            PixelBenderTarget::Bitmap(_) => Ok(PixelBenderOutput::Bitmap(sync_handle)),
            PixelBenderTarget::Bytes { width, .. } => {
                let mut output = None;
                self.resolve_sync_handle(
                    sync_handle,
                    Box::new(|raw_pixels, buffer_width| {
                        let width = *width as usize;

                        if buffer_width as usize
                            != width * output_channels * std::mem::size_of::<f32>()
                        {
                            let mut new_pixels = Vec::new();
                            for row in raw_pixels.chunks(buffer_width as usize) {
                                let actual_row = &row[0..(width * std::mem::size_of::<[f32; 4]>())];

                                for pixel in actual_row
                                    .as_chunks::<{ std::mem::size_of::<[f32; 4]>() }>()
                                    .0
                                {
                                    if has_padding {
                                        // Take the first three channels
                                        new_pixels.extend_from_slice(
                                            &pixel[0..(3 * std::mem::size_of::<f32>())],
                                        );
                                    } else {
                                        // Copy the pixel as-is
                                        new_pixels.extend_from_slice(pixel);
                                    }
                                }
                            }
                            output = Some(new_pixels);
                        } else {
                            output = Some(raw_pixels.to_vec());
                        };
                    }),
                )?;
                Ok(PixelBenderOutput::Bytes(output.unwrap()))
            }
        }
    }

    fn create_empty_texture(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<BitmapHandle, BitmapError> {
        crate::stats::count(&crate::stats::EMPTY_TEXTURES_CREATED);
        let width = width.get();
        let height = height.get();

        if width > self.descriptors.limits.max_texture_dimension_2d
            || height > self.descriptors.limits.max_texture_dimension_2d
        {
            return Err(BitmapError::TooLarge);
        }

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let texture_label = create_debug_label!("Bitmap");
        let texture = self
            .descriptors
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: texture_label.as_deref(),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
            });
        let bytes = u64::from(extent.width) * u64::from(extent.height) * 4;
        Ok(BitmapHandle(Arc::new(Texture {
            texture,
            view: Default::default(),
            repeating_linear: Default::default(),
            repeating_nearest: Default::default(),
            clamped_linear: Default::default(),
            clamped_nearest: Default::default(),
            copy_count: Cell::new(0),
            dropped_textures: Some(self.dropped_textures.clone()),
            _live: crate::stats::Live::with_bytes(
                &crate::stats::LIVE_BITMAP_TEXTURES,
                &crate::stats::LIVE_EMPTY_TEXTURE_BYTES,
                bytes,
            ),
        })))
    }

    fn resolve_sync_handle(
        &mut self,
        handle: Box<dyn SyncHandle>,
        with_rgba: RgbaBufRead,
    ) -> Result<(), ruffle_render::error::Error> {
        let handle = Box::<dyn Any>::downcast::<QueueSyncHandle>(handle).unwrap();
        handle.capture(with_rgba, &mut self.active_frame);
        Ok(())
    }
}

pub async fn request_adapter_and_device(
    backend: wgpu::Backends,
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'static>>,
    power_preference: wgpu::PowerPreference,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), Error> {
    let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference,
        compatible_surface: surface,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }).await
        .map_err(|_e| {
            let names = get_backend_names(backend);
            if names.is_empty() {
                "Ruffle requires hardware acceleration, but no compatible graphics device was found (no backend provided?)".to_string()
            } else if cfg!(target_vendor = "apple") {
                "Ruffle does not support OpenGL on macOS/iOS.".to_string()
            } else {
                format!("Ruffle requires hardware acceleration, but no compatible graphics device was found supporting {}", format_list(&names, "or"))
            }
        })?;

    let (device, queue) = request_device(&adapter).await?;
    Ok((adapter, device, queue))
}

// We try to request the highest limits we can get away with
async fn request_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    // We start off with the lowest limits we actually need - basically GL-ES 3.0
    let mut limits = wgpu::Limits::downlevel_webgl2_defaults();
    // Then we increase parts of it to the maximum supported by the adapter, to take advantage of
    // more powerful hardware or capabilities
    limits = limits.using_resolution(adapter.limits());
    limits = limits.using_alignment(adapter.limits());
    limits.max_uniform_buffer_binding_size = adapter.limits().max_uniform_buffer_binding_size;
    limits.max_inter_stage_shader_variables = adapter.limits().max_inter_stage_shader_variables;
    // This will be a default limit in a future wgpu version (down from 8).
    // It's required for some WebGL devices to be supported.
    limits.max_color_attachments = 4;

    let mut features = Default::default();

    let optional_features = wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
        | wgpu::Features::TEXTURE_COMPRESSION_BC
        | wgpu::Features::FLOAT32_FILTERABLE
        | wgpu::Features::DUAL_SOURCE_BLENDING
        | GpuProfiler::ALL_WGPU_TIMER_FEATURES;

    features |= optional_features & adapter.features();

    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: features,
            required_limits: limits,
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        })
        .await
}

/// Determines how we choose our frame buffer
#[derive(Clone)]
pub enum RenderTargetMode {
    // Construct a new frame buffer, clearng it with the provided color.
    // This is used when rendering to the actual display,
    // or when applying a filter. In both cases, we have a fixed background color,
    // and don't need to blend with anything else
    FreshWithColor(wgpu::Color),
    // Construct a new frame buffer, cleared with an existing texture.
    // we will blend with the previous contents of the texture.
    // This is used in `render_offscreen`, as we need to blend with the previous
    // contents of our `BitmapData` texture
    FreshWithTexture(wgpu::Texture),
    // Use the provided texture as our frame buffer, and clear it with the given color.
    ExistingWithColor(wgpu::Texture, wgpu::TextureView, wgpu::Color),
}

impl RenderTargetMode {
    pub fn color(&self) -> Option<wgpu::Color> {
        match self {
            RenderTargetMode::FreshWithColor(color) => Some(*color),
            RenderTargetMode::FreshWithTexture(_) => None,
            RenderTargetMode::ExistingWithColor(_, _, color) => Some(*color),
        }
    }
}

pub struct ActiveFrame {
    pub staging_belt: wgpu::util::StagingBelt,
    pub command_encoder: wgpu::CommandEncoder,
    draws_since_flush: u32,
}

/// A multiple of every mesh vertex's size, so that each draw's vertices start
/// a whole number of vertices into their buffer.
const MESH_VERTEX_ALIGNMENT: wgpu::BufferAddress = 60;
const _: () = assert!(
    MESH_VERTEX_ALIGNMENT.is_multiple_of(size_of::<PosColorVertex>() as u64)
        && MESH_VERTEX_ALIGNMENT.is_multiple_of(size_of::<PosUvVertex>() as u64)
        && MESH_VERTEX_ALIGNMENT.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
);

const OFFSCREEN_TEXTURE_MAX_IDLE_FRAMES: u64 = 60;

const TEXTURE_MAX_IDLE_FRAMES: u64 = 600;

static PASSES_SINCE_SUBMIT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn count_render_pass(kind: crate::stats::PassKind) {
    PASSES_SINCE_SUBMIT.fetch_add(1, Ordering::Relaxed);
    crate::stats::count_pass(kind);
}

fn note_submit() {
    PASSES_SINCE_SUBMIT.store(0, Ordering::Relaxed);
    crate::stats::count(&crate::stats::SUBMITS);
}

fn passes_since_submit() -> u64 {
    PASSES_SINCE_SUBMIT.load(Ordering::Relaxed)
}

/// Every render pass keeps two command buffers alive until its encoder is
/// submitted, and Metal refuses to hand out more than 4096 at once (wgpu then
/// reports the device as lost). A crowded scene can record thousands of
/// passes in one frame (each blend layer and filter costs several), so
/// submit what has been recorded once it holds this many passes.
const MAX_PASSES_PER_SUBMIT: u64 = 512;

pub(crate) fn submit_if_too_many_passes(
    descriptors: &Descriptors,
    staging_belt: &mut wgpu::util::StagingBelt,
    encoder: &mut wgpu::CommandEncoder,
) {
    if passes_since_submit() <= MAX_PASSES_PER_SUBMIT {
        return;
    }
    staging_belt.finish();
    let recorded = std::mem::replace(
        encoder,
        descriptors
            .device
            .create_command_encoder(&Default::default()),
    );
    descriptors.queue.submit(Some(recorded.finish()));
    note_submit();
    staging_belt.recall();
}

impl ActiveFrame {
    const MAX_DRAWS_PER_FLUSH: u32 = 100;

    pub fn new(descriptors: &Descriptors) -> Self {
        Self {
            command_encoder: descriptors
                .device
                .create_command_encoder(&Default::default()),
            staging_belt: wgpu::util::StagingBelt::new(descriptors.device.clone(), 65536),
            draws_since_flush: 0,
        }
    }

    pub fn submit_for_target<T: RenderTarget>(
        &mut self,
        descriptors: &Descriptors,
        target: &T,
        frame: T::Frame,
    ) -> SubmissionIndex {
        note_submit();
        self.draws_since_flush = 0;
        self.staging_belt.finish();
        let draw_encoder = std::mem::replace(
            &mut self.command_encoder,
            descriptors
                .device
                .create_command_encoder(&Default::default()),
        );
        let index = target.submit(
            &descriptors.device,
            &descriptors.queue,
            Some(draw_encoder.finish()),
            frame,
        );
        self.staging_belt.recall();
        index
    }

    pub fn submit_direct(&mut self, descriptors: &Descriptors) -> SubmissionIndex {
        note_submit();
        self.draws_since_flush = 0;
        self.staging_belt.finish();
        let draw_encoder = std::mem::replace(
            &mut self.command_encoder,
            descriptors
                .device
                .create_command_encoder(&Default::default()),
        );
        let index = descriptors.queue.submit(Some(draw_encoder.finish()));
        self.staging_belt.recall();
        index
    }

    pub fn maybe_flush(&mut self, descriptors: &Descriptors) {
        // [NA] This is kind of a hack.
        // If we do "too much" during one frame, the submission ends up being way too large and goes OutOfMemory.
        // What it is that we're OOMing on is likely buffers and temporary textures and such from render_offscreen
        // Hard to track that though... so let's just flush it out if we do more than X draws per frame
        self.draws_since_flush += 1;

        if self.draws_since_flush > Self::MAX_DRAWS_PER_FLUSH
            || passes_since_submit() > MAX_PASSES_PER_SUBMIT
        {
            self.submit_direct(descriptors);
        }
    }
}

fn vertex_bounds(mesh: &ruffle_render::tessellator::Mesh) -> swf::Rectangle<swf::Twips> {
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for vertex in mesh.draws.iter().flat_map(|draw| &draw.vertices) {
        min = (min.0.min(vertex.x), min.1.min(vertex.y));
        max = (max.0.max(vertex.x), max.1.max(vertex.y));
    }
    if min.0 > max.0 {
        return Default::default();
    }
    swf::Rectangle {
        x_min: swf::Twips::from_pixels(min.0.into()),
        y_min: swf::Twips::from_pixels(min.1.into()),
        x_max: swf::Twips::from_pixels(max.0.into()),
        y_max: swf::Twips::from_pixels(max.1.into()),
    }
}
