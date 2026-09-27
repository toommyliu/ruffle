use super::target::PoolOrArcTexture;
use crate::backend::RenderTargetMode;
use crate::blend::{BlendType, ComplexBlend, DirectBlend, TrivialBlend};
use crate::buffer_builder::BufferBuilder;
use crate::buffer_pool::TexturePool;
use crate::dynamic_transforms::DynamicTransforms;
use crate::mesh::{DrawType, Mesh, as_mesh};
use crate::surface::Surface;
use crate::surface::target::CommandTarget;
use crate::{Descriptors, MaskState, Pipelines, PosUvVertex, Transforms, as_texture};
use ruffle_render::backend::ShapeHandle;
use ruffle_render::bitmap::{BitmapHandle, PixelRegion, PixelSnapping};
use ruffle_render::commands::{Command, CommandHandler, CommandList, RenderBlendMode};
use ruffle_render::lines::{emulate_line, emulate_line_rect};
use ruffle_render::matrix::Matrix;
use ruffle_render::pixel_bender::PixelBenderShaderHandle;
use ruffle_render::quality::StageQuality;
use ruffle_render::transform::Transform;
use std::mem;
use swf::{BlendMode, Color, ColorTransform, Twips};
use wgpu::Backend;
use wgpu_profiler::Scope;

pub struct CommandRenderer<'encoder> {
    pipelines: &'encoder Pipelines,
    descriptors: &'encoder Descriptors,
    num_masks: u32,
    mask_state: MaskState,
    needs_stencil: bool,
    dynamic_vertex_buffer: &'encoder wgpu::Buffer,
    parent_copy: Option<&'encoder wgpu::BindGroup>,
}

impl<'encoder> CommandRenderer<'encoder> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pipelines: &'encoder Pipelines,
        descriptors: &'encoder Descriptors,
        dynamic_vertex_buffer: &'encoder wgpu::Buffer,
        num_masks: u32,
        mask_state: MaskState,
        needs_stencil: bool,
        parent_copy: Option<&'encoder wgpu::BindGroup>,
    ) -> Self {
        Self {
            pipelines,
            num_masks,
            mask_state,
            descriptors,
            needs_stencil,
            dynamic_vertex_buffer,
            parent_copy,
        }
    }

    fn bind_parent_copy(&self, render_pass: &mut wgpu::RenderPass<'encoder>, blend: DirectBlend) {
        if let DirectBlend::Complex(mode) = blend {
            let offset = DirectBlend::complex_direct_mode(mode)
                * self.descriptors.complex_direct_mode_stride;
            render_pass.set_bind_group(
                3,
                self.parent_copy
                    .expect("A complex blend draw must follow a parent copy"),
                &[offset],
            );
        }
    }

    pub fn execute(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        command: &'encoder DrawCommand,
    ) {
        if self.needs_stencil {
            match self.mask_state {
                MaskState::NoMask => {}
                MaskState::DrawMaskStencil => {
                    render_pass.set_stencil_reference(self.num_masks - 1);
                }
                MaskState::DrawMaskedContent => {
                    render_pass.set_stencil_reference(self.num_masks);
                }
                MaskState::ClearMaskStencil => {
                    render_pass.set_stencil_reference(self.num_masks);
                }
            }
        }

        match command {
            DrawCommand::RenderBitmap {
                bitmap,
                instance_index,
                vertex_offset,
                smoothing,
                blend_mode,
                render_stage3d,
            } => self.render_bitmap(
                render_pass,
                bitmap,
                *instance_index,
                *smoothing,
                *blend_mode,
                *render_stage3d,
                *vertex_offset,
            ),
            DrawCommand::RenderTexture {
                _texture,
                binds,
                instance_index,
                blend_mode,
            } => self.render_texture(render_pass, *instance_index, binds, *blend_mode),
            DrawCommand::RenderShape {
                shape,
                instance_index,
                blend,
            } => self.render_shape(render_pass, shape, *instance_index, *blend),
            DrawCommand::DrawRect { instance_index } => {
                self.draw_rect(render_pass, *instance_index)
            }
            DrawCommand::DrawLine { instance_index } => {
                self.draw_lines::<false>(render_pass, *instance_index)
            }
            DrawCommand::DrawLineRect { instance_index } => {
                self.draw_lines::<true>(render_pass, *instance_index)
            }
            DrawCommand::PushMask => self.push_mask(render_pass),
            DrawCommand::ActivateMask => self.activate_mask(render_pass),
            DrawCommand::DeactivateMask => self.deactivate_mask(render_pass),
            DrawCommand::PopMask => self.pop_mask(render_pass),
            DrawCommand::RenderAlphaMask {
                maskee,
                mask,
                binds,
                instance_index,
            } => self.render_alpha_mask(render_pass, maskee, mask, binds, *instance_index),
        }
    }

    pub fn prep_color(&self, render_pass: &mut wgpu::RenderPass<'encoder>, blend: DirectBlend) {
        let pipelines = self.pipelines.color(blend);
        if self.needs_stencil {
            render_pass.set_pipeline(pipelines.pipeline_for(self.mask_state));
        } else {
            render_pass.set_pipeline(pipelines.stencilless_pipeline());
        }
        self.bind_parent_copy(render_pass, blend);
    }

    pub fn prep_lines(&self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        if self.needs_stencil {
            render_pass.set_pipeline(self.pipelines.lines.pipeline_for(self.mask_state));
        } else {
            render_pass.set_pipeline(self.pipelines.lines.stencilless_pipeline());
        }
    }

    pub fn prep_gradient(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
        blend: DirectBlend,
    ) {
        let pipelines = self.pipelines.gradient(blend);
        if self.needs_stencil {
            render_pass.set_pipeline(pipelines.pipeline_for(self.mask_state));
        } else {
            render_pass.set_pipeline(pipelines.stencilless_pipeline());
        }

        render_pass.set_bind_group(2, bind_group, &[]);
        self.bind_parent_copy(render_pass, blend);
    }

    pub fn prep_bitmap(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
        blend: DirectBlend,
        render_stage3d: bool,
    ) {
        match (self.needs_stencil, render_stage3d) {
            (true, true) => {
                render_pass.set_pipeline(&self.pipelines.bitmap_opaque_dummy_stencil);
            }
            (true, false) => {
                render_pass
                    .set_pipeline(self.pipelines.bitmap(blend).pipeline_for(self.mask_state));
            }
            (false, true) => {
                render_pass.set_pipeline(&self.pipelines.bitmap_opaque);
            }
            (false, false) => {
                render_pass.set_pipeline(self.pipelines.bitmap(blend).stencilless_pipeline());
            }
        }

        render_pass.set_bind_group(2, bind_group, &[]);
        self.bind_parent_copy(render_pass, blend);
    }

    pub fn prep_alpha_mask(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
    ) {
        if self.needs_stencil {
            render_pass.set_pipeline(self.pipelines.alpha_mask.pipeline_for(self.mask_state));
        } else {
            render_pass.set_pipeline(self.pipelines.alpha_mask.stencilless_pipeline());
        }

        render_pass.set_bind_group(2, bind_group, &[]);
    }

    pub fn draw(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        vertices: wgpu::BufferSlice<'encoder>,
        (indices, index_format): (wgpu::BufferSlice<'encoder>, wgpu::IndexFormat),
        num_indices: u32,
        instance_index: u32,
    ) {
        render_pass.set_vertex_buffer(0, vertices);
        render_pass.set_index_buffer(indices, index_format);

        render_pass.draw_indexed(0..num_indices, 0, instance_index..(instance_index + 1));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_bitmap(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bitmap: &'encoder BitmapHandle,
        instance_index: u32,
        smoothing: bool,
        blend_mode: DirectBlend,
        render_stage3d: bool,
        vertex_offset: Option<wgpu::BufferAddress>,
    ) {
        let texture = as_texture(bitmap);

        let descriptors = self.descriptors;
        let bind = texture.bind_group(
            false,
            smoothing,
            &descriptors.device,
            &descriptors.bind_layouts.bitmap,
            bitmap.clone(),
            &descriptors.bitmap_samplers,
        );
        self.prep_bitmap(render_pass, &bind.bind_group, blend_mode, render_stage3d);

        let vertex_slice = if let Some(vertex_offset) = vertex_offset {
            self.dynamic_vertex_buffer.slice(vertex_offset..)
        } else {
            self.descriptors.quad.vertices_pos_uv.slice(..)
        };

        self.draw(
            render_pass,
            vertex_slice,
            (
                self.descriptors.quad.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            ),
            6,
            instance_index,
        );
    }

    pub fn render_texture(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        instance_index: u32,
        bind_group: &'encoder wgpu::BindGroup,
        blend_mode: TrivialBlend,
    ) {
        self.prep_bitmap(
            render_pass,
            bind_group,
            DirectBlend::Trivial(blend_mode),
            false,
        );

        self.draw(
            render_pass,
            self.descriptors.quad.vertices_pos_uv.slice(..),
            (
                self.descriptors.quad.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            ),
            6,
            instance_index,
        );
    }

    pub fn render_shape(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        shape: &'encoder ShapeHandle,
        instance_index: u32,
        blend: DirectBlend,
    ) {
        let mesh = as_mesh(shape);
        for draw in &mesh.draws {
            let num_indices = if self.mask_state != MaskState::DrawMaskStencil
                && self.mask_state != MaskState::ClearMaskStencil
            {
                draw.num_indices
            } else {
                // Omit strokes when drawing a mask stencil.
                draw.num_mask_indices
            };
            if num_indices == 0 {
                continue;
            }

            match &draw.draw_type {
                DrawType::Color => {
                    self.prep_color(render_pass, blend);
                }
                DrawType::Gradient { bind_group, .. } => {
                    self.prep_gradient(render_pass, bind_group, blend);
                }
                DrawType::Bitmap { binds, .. } => {
                    self.prep_bitmap(render_pass, &binds.bind_group, blend, false);
                }
            }

            self.draw(
                render_pass,
                mesh.vertex_buffer.slice(draw.vertices.clone()),
                (
                    mesh.index_buffer.slice(draw.indices.clone()),
                    mesh.index_format,
                ),
                num_indices,
                instance_index,
            );
        }
    }

    pub fn render_alpha_mask(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        _maskee: &PoolOrArcTexture,
        _mask: &PoolOrArcTexture,
        bind_group: &'encoder wgpu::BindGroup,
        instance_index: u32,
    ) {
        if cfg!(feature = "render_debug_labels") {
            render_pass.push_debug_group("render_alpha_mask");
        }

        self.prep_alpha_mask(render_pass, bind_group);

        self.draw(
            render_pass,
            self.descriptors.quad.vertices_pos.slice(..),
            (
                self.descriptors.quad.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            ),
            6,
            instance_index,
        );

        if cfg!(feature = "render_debug_labels") {
            render_pass.pop_debug_group();
        }
    }

    pub fn draw_rect(&self, render_pass: &mut wgpu::RenderPass<'encoder>, instance_index: u32) {
        self.prep_color(render_pass, DirectBlend::NORMAL);

        self.draw(
            render_pass,
            self.descriptors.quad.vertices_pos_color.slice(..),
            (
                self.descriptors.quad.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            ),
            6,
            instance_index,
        );
    }

    pub fn draw_lines<const RECT: bool>(
        &self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        instance_index: u32,
    ) {
        self.prep_lines(render_pass);

        self.draw(
            render_pass,
            self.descriptors.quad.vertices_pos_color.slice(..),
            (
                if RECT {
                    self.descriptors.quad.indices_line_rect.slice(..)
                } else {
                    self.descriptors.quad.indices_line.slice(..)
                },
                wgpu::IndexFormat::Uint32,
            ),
            if RECT { 5 } else { 2 },
            instance_index,
        );
    }

    pub fn push_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(
            self.mask_state == MaskState::NoMask || self.mask_state == MaskState::DrawMaskedContent
        );
        self.num_masks += 1;
        self.mask_state = MaskState::DrawMaskStencil;
        render_pass.set_stencil_reference(self.num_masks - 1);
    }

    pub fn activate_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(self.num_masks > 0 && self.mask_state == MaskState::DrawMaskStencil);
        self.mask_state = MaskState::DrawMaskedContent;
        render_pass.set_stencil_reference(self.num_masks);
    }

    pub fn deactivate_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(self.num_masks > 0 && self.mask_state == MaskState::DrawMaskedContent);
        self.mask_state = MaskState::ClearMaskStencil;
        render_pass.set_stencil_reference(self.num_masks);
    }

    pub fn pop_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(self.num_masks > 0 && self.mask_state == MaskState::ClearMaskStencil);
        self.num_masks -= 1;
        render_pass.set_stencil_reference(self.num_masks);
        if self.num_masks == 0 {
            self.mask_state = MaskState::NoMask;
        } else {
            self.mask_state = MaskState::DrawMaskedContent;
        };
    }

    pub fn num_masks(&self) -> u32 {
        self.num_masks
    }

    pub fn mask_state(&self) -> MaskState {
        self.mask_state
    }
}

pub enum Chunk {
    Draw {
        chunk: Vec<DrawCommand>,
        needs_stencil: bool,
        transforms: BufferBuilder,
        vertices: BufferBuilder,
    },
    Blend {
        texture: PoolOrArcTexture,
        blend_mode: ChunkBlendMode,
        needs_stencil: bool,
    },
    CopyParent {
        regions: Vec<PixelRegion>,
    },
}

#[derive(Debug)]
pub enum ChunkBlendMode {
    Complex(ComplexBlend),
    Shader(PixelBenderShaderHandle),
}

#[derive(Debug)]
pub enum DrawCommand {
    RenderBitmap {
        bitmap: BitmapHandle,
        instance_index: u32,
        vertex_offset: Option<wgpu::BufferAddress>,
        smoothing: bool,
        blend_mode: DirectBlend,
        render_stage3d: bool,
    },
    RenderTexture {
        _texture: PoolOrArcTexture,
        binds: wgpu::BindGroup,
        instance_index: u32,
        blend_mode: TrivialBlend,
    },
    RenderAlphaMask {
        maskee: PoolOrArcTexture,
        mask: PoolOrArcTexture,
        binds: wgpu::BindGroup,
        instance_index: u32,
    },
    RenderShape {
        shape: ShapeHandle,
        instance_index: u32,
        blend: DirectBlend,
    },
    DrawRect {
        instance_index: u32,
    },
    DrawLine {
        instance_index: u32,
    },
    DrawLineRect {
        instance_index: u32,
    },
    PushMask,
    ActivateMask,
    DeactivateMask,
    PopMask,
}

impl DrawCommand {
    fn set_indices(&mut self, index: u32, offset: Option<wgpu::BufferAddress>) {
        match self {
            DrawCommand::RenderBitmap {
                instance_index,
                vertex_offset,
                ..
            } => {
                *instance_index = index;
                *vertex_offset = offset;
            }
            DrawCommand::RenderTexture { instance_index, .. }
            | DrawCommand::RenderAlphaMask { instance_index, .. }
            | DrawCommand::RenderShape { instance_index, .. }
            | DrawCommand::DrawRect { instance_index }
            | DrawCommand::DrawLine { instance_index }
            | DrawCommand::DrawLineRect { instance_index } => *instance_index = index,
            DrawCommand::PushMask
            | DrawCommand::ActivateMask
            | DrawCommand::DeactivateMask
            | DrawCommand::PopMask => {}
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            DrawCommand::RenderBitmap { .. } => "render bitmap",
            DrawCommand::RenderShape { .. } => "render shape",
            DrawCommand::RenderTexture { .. } => "render texture",
            DrawCommand::DrawRect { .. } => "draw rect",
            DrawCommand::DrawLine { .. } => "draw line",
            DrawCommand::DrawLineRect { .. } => "draw line rect",
            DrawCommand::PushMask => "push mask",
            DrawCommand::ActivateMask => "activate mask",
            DrawCommand::DeactivateMask => "deactivate mask",
            DrawCommand::PopMask => "pop mask",
            DrawCommand::RenderAlphaMask { .. } => "render alpha mask",
        }
    }
}

#[derive(Copy, Clone)]
pub enum LayerRef<'a> {
    None,
    Current,
    Parent(&'a CommandTarget),
}

#[derive(Clone, Copy)]
struct Footprint {
    region: PixelRegion,
    reads_below: bool,
}

struct PendingDraw {
    transform: Transforms,
    vertices: Option<[PosUvVertex; 4]>,
    command: DrawCommand,
}

const BATCH_TILE_SIZE: u32 = 16;

/// A draw goes one level above every earlier draw it overlaps if it reads
/// what's below, and at the highest of their levels otherwise. Emitting the
/// draws level by level then renders the same image as drawing them in order.
#[derive(Default)]
struct DrawBatch {
    tiles: Vec<u16>,
    columns: u32,
    levels: Vec<BatchLevel>,
}

#[derive(Default)]
struct BatchLevel {
    copies: Vec<PixelRegion>,
    items: Vec<BatchItem>,
}

// Draws are nearly every item, so boxing them would only add allocations.
#[expect(clippy::large_enum_variant)]
enum BatchItem {
    Draw(PendingDraw),
    Blend(Chunk),
    /// Everything from a `push_mask` with no mask active to its `pop_mask`.
    /// It stays together and in order, and leaves the stencil buffer as it
    /// found it, so it can move like a single draw.
    Masked(Vec<MaskedItem>),
}

enum MaskedItem {
    Draw(PendingDraw),
    Mask(DrawCommand),
    Copy(PixelRegion),
    Blend(Chunk),
}

#[derive(Default)]
struct MaskedBlock {
    items: Vec<MaskedItem>,
    regions: Vec<PixelRegion>,
    reads_below: bool,
}

impl DrawBatch {
    fn add(
        &mut self,
        width: u32,
        height: u32,
        regions: &[PixelRegion],
        reads_below: bool,
        copy: Option<PixelRegion>,
        item: BatchItem,
    ) {
        if self.tiles.is_empty() {
            self.columns = width.div_ceil(BATCH_TILE_SIZE).max(1);
            let rows = height.div_ceil(BATCH_TILE_SIZE).max(1);
            self.tiles = vec![0; (self.columns * rows) as usize];
        }
        let columns = self.columns;
        let tile_indices = |region: &PixelRegion| {
            let x = (region.x_min / BATCH_TILE_SIZE)..=((region.x_max - 1) / BATCH_TILE_SIZE);
            let y = (region.y_min / BATCH_TILE_SIZE)..=((region.y_max - 1) / BATCH_TILE_SIZE);
            y.flat_map(move |row| {
                x.clone()
                    .map(move |column| (row * columns + column) as usize)
            })
        };
        let touched = || regions.iter().filter(|region| !region.is_empty());
        let top = touched()
            .flat_map(tile_indices)
            .map(|i| self.tiles[i])
            .max();
        let level = match top {
            None => 0,
            Some(top) if reads_below => top,
            Some(top) => top.saturating_sub(1),
        };
        for i in touched().flat_map(tile_indices) {
            self.tiles[i] = self.tiles[i].max(level + 1);
        }
        let level = usize::from(level);
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, Default::default);
        }
        let entry = &mut self.levels[level];
        entry
            .copies
            .extend(copy.filter(|region| !region.is_empty()));
        entry.items.push(item);
    }

    fn take_levels(&mut self) -> Vec<BatchLevel> {
        self.tiles.fill(0);
        mem::take(&mut self.levels)
    }
}

/// Replaces every blend with a RenderBitmap, with the subcommands rendered out to a temporary texture
/// Every complex blend will be its own item, but every other draw will be chunked together
#[expect(clippy::too_many_arguments)]
pub fn chunk_blends<'encoder, 'global: 'encoder>(
    commands: CommandList,
    descriptors: &'encoder Descriptors,
    staging_belt: &'encoder mut wgpu::util::StagingBelt,
    dynamic_transforms: &'encoder DynamicTransforms,
    draw_encoder: &'encoder mut Scope<'global, wgpu::CommandEncoder>,
    meshes: &'encoder Vec<Mesh>,
    quality: StageQuality,
    width: u32,
    height: u32,
    nearest_layer: LayerRef,
    texture_pool: &'encoder mut TexturePool,
) -> Vec<Chunk> {
    WgpuCommandHandler::new(
        descriptors,
        staging_belt,
        dynamic_transforms,
        draw_encoder,
        meshes,
        quality,
        width,
        height,
        nearest_layer,
        texture_pool,
    )
    .chunk_blends(commands)
}

struct WgpuCommandHandler<'encoder, 'global: 'encoder> {
    descriptors: &'encoder Descriptors,
    quality: StageQuality,
    width: u32,
    height: u32,
    nearest_layer: LayerRef<'encoder>,
    meshes: &'encoder Vec<Mesh>,
    staging_belt: &'encoder mut wgpu::util::StagingBelt,
    dynamic_transforms: &'encoder DynamicTransforms,
    draw_encoder: &'encoder mut Scope<'global, wgpu::CommandEncoder>,
    texture_pool: &'encoder mut TexturePool,
    emulate_lines: bool,

    result: Vec<Chunk>,
    current: Vec<DrawCommand>,
    transforms: BufferBuilder,
    vertices: BufferBuilder,
    needs_stencil: bool,
    num_masks: i32,
    batch: DrawBatch,
    masked: Option<MaskedBlock>,
}

impl<'encoder, 'global: 'encoder> WgpuCommandHandler<'encoder, 'global> {
    #[expect(clippy::too_many_arguments)]
    fn new(
        descriptors: &'encoder Descriptors,
        staging_belt: &'encoder mut wgpu::util::StagingBelt,
        dynamic_transforms: &'encoder DynamicTransforms,
        draw_encoder: &'encoder mut Scope<'global, wgpu::CommandEncoder>,
        meshes: &'encoder Vec<Mesh>,
        quality: StageQuality,
        width: u32,
        height: u32,
        nearest_layer: LayerRef<'encoder>,
        texture_pool: &'encoder mut TexturePool,
    ) -> Self {
        let transforms = Self::new_transforms(descriptors, dynamic_transforms);
        let vertices = Self::new_vertices(descriptors, dynamic_transforms);

        // DirectX does support drawing lines, but it's very inconsistent.
        // With MSAA, lines have 1.4px thickness, which makes them too thick.
        // Without MSAA, lines have 1px thickness, but their placement is sometimes off.
        let emulate_lines = descriptors.backend == Backend::Dx12;

        Self {
            descriptors,
            quality,
            width,
            height,
            nearest_layer,
            meshes,
            staging_belt,
            dynamic_transforms,
            draw_encoder,
            texture_pool,
            emulate_lines,

            result: vec![],
            current: vec![],
            transforms,
            vertices,
            needs_stencil: false,
            num_masks: 0,
            batch: DrawBatch::default(),
            masked: None,
        }
    }

    fn new_transforms(
        descriptors: &'encoder Descriptors,
        dynamic_transforms: &'encoder DynamicTransforms,
    ) -> BufferBuilder {
        let mut transforms = BufferBuilder::new(&descriptors.limits, 0);
        transforms.set_buffer_limit(dynamic_transforms.buffer.size());
        transforms
    }

    fn new_vertices(
        descriptors: &'encoder Descriptors,
        dynamic_transforms: &'encoder DynamicTransforms,
    ) -> BufferBuilder {
        let mut vertices = BufferBuilder::new(&descriptors.limits, 0);
        vertices.set_buffer_limit(dynamic_transforms.vertex_buffer.size());
        vertices
    }

    /// Replaces every blend with a RenderBitmap, with the subcommands rendered out to a temporary texture
    /// Every complex blend will be its own item, but every other draw will be chunked together
    fn chunk_blends(&mut self, commands: CommandList) -> Vec<Chunk> {
        commands.execute(self);
        self.finish_masked_block();
        self.flush_batch();
        self.flush_current();
        mem::take(&mut self.result)
    }

    fn flush_batch(&mut self) {
        for level in self.batch.take_levels() {
            if !level.copies.is_empty() {
                self.flush_current();
                self.result.push(Chunk::CopyParent {
                    regions: level.copies,
                });
            }
            for item in level.items {
                match item {
                    BatchItem::Draw(draw) => self.push_pending(draw),
                    BatchItem::Blend(chunk) => {
                        self.flush_current();
                        self.result.push(chunk);
                    }
                    BatchItem::Masked(items) => {
                        self.needs_stencil = true;
                        for item in items {
                            match item {
                                MaskedItem::Draw(draw) => self.push_pending(draw),
                                MaskedItem::Mask(command) => self.current.push(command),
                                MaskedItem::Copy(region) => {
                                    self.flush_current();
                                    self.result.push(Chunk::CopyParent {
                                        regions: vec![region],
                                    });
                                }
                                MaskedItem::Blend(chunk) => {
                                    self.flush_current();
                                    self.result.push(chunk);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn push_pending(&mut self, draw: PendingDraw) {
        let mut command = draw.command;
        self.push_draw(
            draw.transform,
            draw.vertices.as_ref().map(|v| &v[..]),
            |instance_index, vertex_offset| {
                command.set_indices(instance_index, vertex_offset);
                command
            },
        );
    }

    fn record_mask(&mut self, command: DrawCommand) {
        self.masked
            .get_or_insert_with(Default::default)
            .items
            .push(MaskedItem::Mask(command));
    }

    fn add_to_batch(
        &mut self,
        width: u32,
        height: u32,
        regions: &[PixelRegion],
        reads_below: bool,
        copy: Option<PixelRegion>,
        item: BatchItem,
    ) {
        self.batch
            .add(width, height, regions, reads_below, copy, item);
    }

    fn finish_masked_block(&mut self) {
        if let Some(block) = self.masked.take() {
            self.add_to_batch(
                self.width,
                self.height,
                &block.regions,
                block.reads_below,
                None,
                BatchItem::Masked(block.items),
            );
        }
    }

    fn footprint(&self, matrix: &Matrix, bounds: swf::Rectangle<Twips>) -> PixelRegion {
        let mut region = PixelRegion::from(*matrix * bounds);
        region.x_min = region.x_min.saturating_sub(1);
        region.y_min = region.y_min.saturating_sub(1);
        region.x_max = region.x_max.saturating_add(1);
        region.y_max = region.y_max.saturating_add(1);
        region.clamp(self.width, self.height);
        region
    }

    /// Compositing a layer of `commands` changes only the pixels they land on:
    /// layers start transparent, and blends leave what's below a transparent
    /// pixel alone (except Alpha and Erase, which change the layer they're in).
    fn commands_footprint(&self, commands: &CommandList) -> PixelRegion {
        let mut union: Option<PixelRegion> = None;
        let mut add = |region: PixelRegion| {
            if !region.is_empty() {
                match &mut union {
                    Some(union) => union.union(region),
                    None => union = Some(region),
                }
            }
        };
        for command in &commands.commands {
            match command {
                Command::RenderShape { shape, transform } => {
                    add(self.footprint(&transform.matrix, as_mesh(shape).bounds))
                }
                Command::RenderBitmap {
                    transform,
                    pixel_snapping,
                    region,
                    ..
                } => {
                    let mut matrix = transform.matrix;
                    pixel_snapping.apply(&mut matrix);
                    matrix *= Matrix::scale(region.width() as f32, region.height() as f32);
                    add(self.quad_footprint(&matrix).region)
                }
                Command::RenderStage3D { bitmap, transform } => {
                    let texture = &as_texture(bitmap).texture;
                    let matrix = transform.matrix
                        * Matrix::scale(texture.width() as f32, texture.height() as f32);
                    add(self.quad_footprint(&matrix).region)
                }
                Command::DrawRect { matrix, .. } => add(self.quad_footprint(matrix).region),
                Command::DrawLine { matrix, .. } | Command::DrawLineRect { matrix, .. } => {
                    let mut matrix = *matrix;
                    matrix.tx += Twips::HALF_PX;
                    matrix.ty += Twips::HALF_PX;
                    add(self.quad_footprint(&matrix).region)
                }
                Command::RenderAlphaMask {
                    maskee_commands, ..
                } => add(self.commands_footprint(maskee_commands)),
                Command::Blend(commands, _) => add(self.commands_footprint(commands)),
                Command::PushMask
                | Command::ActivateMask
                | Command::DeactivateMask
                | Command::PopMask => {}
            }
        }
        union.unwrap_or(PixelRegion::for_whole_size(0, 0))
    }

    fn quad_footprint(&self, matrix: &Matrix) -> Footprint {
        Footprint {
            region: self.footprint(
                matrix,
                swf::Rectangle {
                    x_min: Twips::ZERO,
                    y_min: Twips::ZERO,
                    x_max: Twips::ONE_PX,
                    y_max: Twips::ONE_PX,
                },
            ),
            reads_below: false,
        }
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.result.push(Chunk::Draw {
            chunk: mem::take(&mut self.current),
            needs_stencil: self.needs_stencil,
            transforms: mem::replace(
                &mut self.transforms,
                Self::new_transforms(self.descriptors, self.dynamic_transforms),
            ),
            vertices: mem::replace(
                &mut self.vertices,
                Self::new_vertices(self.descriptors, self.dynamic_transforms),
            ),
        });
    }

    fn add_to_current(
        &mut self,
        matrix: Matrix,
        tz: f64,
        color_transform: ColorTransform,
        footprint: Footprint,
        command_builder: impl FnOnce(u32) -> DrawCommand,
    ) {
        self.add_to_current_with_vertices(
            matrix,
            tz,
            color_transform,
            None,
            footprint,
            |instance_index, _| command_builder(instance_index),
        )
    }

    fn add_to_current_with_vertices(
        &mut self,
        matrix: Matrix,
        tz: f64,
        color_transform: ColorTransform,
        vertices: Option<&[PosUvVertex]>,
        footprint: Footprint,
        command_builder: impl FnOnce(u32, Option<wgpu::BufferAddress>) -> DrawCommand,
    ) {
        let transform = Transforms {
            world_matrix: [
                [matrix.a, matrix.b, 0.0, 0.0],
                [matrix.c, matrix.d, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [
                    matrix.tx.to_pixels() as f32,
                    matrix.ty.to_pixels() as f32,
                    tz as f32,
                    1.0,
                ],
            ],
            mult_color: color_transform.mult_rgba_normalized(),
            add_color: color_transform.add_rgba_normalized(),
        };
        let draw = PendingDraw {
            transform,
            vertices: vertices.map(|v| {
                v.try_into()
                    .expect("Batched draws have a quad's vertices or none")
            }),
            command: command_builder(0, vertices.map(|_| 0)),
        };
        if let Some(block) = &mut self.masked {
            block.regions.push(footprint.region);
            block.items.push(MaskedItem::Draw(draw));
        } else {
            let region = footprint.region;
            self.add_to_batch(
                self.width,
                self.height,
                &[region],
                footprint.reads_below,
                footprint.reads_below.then_some(region),
                BatchItem::Draw(draw),
            );
        }
    }

    fn push_draw(
        &mut self,
        transform: Transforms,
        vertices: Option<&[PosUvVertex]>,
        command_builder: impl FnOnce(u32, Option<wgpu::BufferAddress>) -> DrawCommand,
    ) {
        if let (Ok(transform_range), Ok(vertices_range)) = (
            self.transforms.add(&[transform]),
            vertices.map(|v| self.vertices.add(v)).transpose(),
        ) {
            self.current.push(command_builder(
                (transform_range.start as usize / size_of::<Transforms>()) as u32,
                vertices_range.map(|v| v.start),
            ));
        } else {
            self.flush_current();
            let transform_range = self
                .transforms
                .add(&[transform])
                .expect("Buffer must be able to fit a new thing, it was just emptied");
            let vertices_range = vertices.map(|v| {
                self.vertices
                    .add(v)
                    .expect("Buffer must be able to fit a new thing, it was just emptied")
            });
            self.current.push(command_builder(
                (transform_range.start as usize / size_of::<Transforms>()) as u32,
                vertices_range.map(|v| v.start),
            ));
        }
    }
    fn add_bitmap(
        &mut self,
        bitmap: BitmapHandle,
        transform: Transform,
        smoothing: bool,
        pixel_snapping: PixelSnapping,
        region: PixelRegion,
        blend: DirectBlend,
    ) {
        let texture = as_texture(&bitmap);

        let mut matrix = transform.matrix;
        pixel_snapping.apply(&mut matrix);
        matrix *= Matrix::scale(region.width() as f32, region.height() as f32);

        let vertices: &[PosUvVertex] = {
            let (u0, u1, v0, v1) = (
                region.x_min as f32 / texture.texture.width() as f32,
                region.x_max as f32 / texture.texture.width() as f32,
                region.y_min as f32 / texture.texture.height() as f32,
                region.y_max as f32 / texture.texture.height() as f32,
            );
            &[
                PosUvVertex::new(0.0, 0.0, u0, v0, 1.0),
                PosUvVertex::new(1.0, 0.0, u1, v0, 1.0),
                PosUvVertex::new(1.0, 1.0, u1, v1, 1.0),
                PosUvVertex::new(0.0, 1.0, u0, v1, 1.0),
            ]
        };

        let footprint = Footprint {
            reads_below: matches!(blend, DirectBlend::Complex(_)),
            ..self.quad_footprint(&matrix)
        };
        self.add_to_current_with_vertices(
            matrix,
            transform.tz,
            transform.color_transform,
            Some(vertices),
            footprint,
            |instance_index, vertex_offset| DrawCommand::RenderBitmap {
                bitmap,
                instance_index,
                vertex_offset,
                smoothing,
                blend_mode: blend,
                render_stage3d: false,
            },
        );
    }

    fn add_shape(&mut self, shape: ShapeHandle, transform: Transform, blend: DirectBlend) {
        let footprint = Footprint {
            region: self.footprint(&transform.matrix, as_mesh(&shape).bounds),
            reads_below: matches!(blend, DirectBlend::Complex(_)),
        };
        self.add_to_current(
            transform.matrix,
            transform.tz,
            transform.color_transform,
            footprint,
            |instance_index| DrawCommand::RenderShape {
                shape,
                instance_index,
                blend,
            },
        );
    }

    fn try_blend_directly(
        &mut self,
        commands: &mut CommandList,
        blend_mode: &RenderBlendMode,
    ) -> bool {
        let dual_source_blending = self.descriptors.shaders.multiply.is_some();
        let Some(blend) = DirectBlend::for_layer(blend_mode, dual_source_blending) else {
            return false;
        };
        match commands.commands.as_slice() {
            [Command::RenderShape { shape, .. }] if as_mesh(shape).flat => {}
            [Command::RenderBitmap { .. }] => {}
            _ => return false,
        }
        if let DirectBlend::Complex(_) = blend {
            let Some(region) = self.parent_copy_region(&commands.commands[0]) else {
                return false;
            };
            if region.is_empty() {
                return true;
            }
            if let Some(block) = &mut self.masked {
                block.items.push(MaskedItem::Copy(region));
                block.reads_below = true;
            }
        }
        match commands.commands.pop() {
            Some(Command::RenderShape { shape, transform }) => {
                self.add_shape(shape, transform, blend);
            }
            Some(Command::RenderBitmap {
                bitmap,
                transform,
                smoothing,
                pixel_snapping,
                region,
            }) => {
                self.add_bitmap(bitmap, transform, smoothing, pixel_snapping, region, blend);
            }
            _ => unreachable!("matched above"),
        }
        true
    }

    fn parent_copy_region(&self, command: &Command) -> Option<PixelRegion> {
        let bounds = match command {
            Command::RenderShape { shape, transform } => {
                if transform.perspective_projection.is_some() {
                    return None;
                }
                transform.matrix * as_mesh(shape).bounds
            }
            Command::RenderBitmap {
                transform,
                pixel_snapping,
                region,
                ..
            } => {
                if transform.perspective_projection.is_some() {
                    return None;
                }
                let mut matrix = transform.matrix;
                pixel_snapping.apply(&mut matrix);
                matrix
                    * swf::Rectangle {
                        x_min: Twips::ZERO,
                        y_min: Twips::ZERO,
                        x_max: Twips::from_pixels(region.width().into()),
                        y_max: Twips::from_pixels(region.height().into()),
                    }
            }
            _ => return None,
        };
        let mut region = PixelRegion::from(bounds);
        region.x_min = region.x_min.saturating_sub(1);
        region.y_min = region.y_min.saturating_sub(1);
        region.x_max += 1;
        region.y_max += 1;
        region.clamp(self.width, self.height);
        Some(region)
    }
}

impl CommandHandler for WgpuCommandHandler<'_, '_> {
    fn blend(&mut self, mut commands: CommandList, blend_mode: RenderBlendMode) {
        if self.try_blend_directly(&mut commands, &blend_mode) {
            return;
        }
        let footprint = self.commands_footprint(&commands);
        let surface = Surface::new(
            self.descriptors,
            self.quality,
            self.width,
            self.height,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let target_layer = if let RenderBlendMode::Builtin(BlendMode::Layer) = &blend_mode {
            LayerRef::Current
        } else {
            self.nearest_layer
        };
        let blend_type = BlendType::from(blend_mode);
        let clear_color = blend_type.default_color();
        let target = surface.draw_commands(
            RenderTargetMode::FreshWithColor(clear_color),
            self.descriptors,
            self.meshes,
            commands,
            self.staging_belt,
            self.dynamic_transforms,
            self.draw_encoder,
            target_layer,
            self.texture_pool,
        );
        target.ensure_cleared(self.draw_encoder);
        crate::backend::submit_if_too_many_passes(
            self.descriptors,
            self.staging_belt,
            self.draw_encoder.recorder,
        );

        // We currently do not support shader blends in masks. In order not to
        // break other parts of the scene, we just fall back to a normal blend.
        //
        // TODO Add support for shader blends in masks.
        let is_shader_blend_in_mask =
            self.num_masks > 0 && matches!(blend_type, BlendType::Shader(_));
        let blend_type = if is_shader_blend_in_mask {
            BlendType::Trivial(TrivialBlend::Normal)
        } else {
            blend_type
        };

        match blend_type {
            BlendType::Trivial(blend_mode) => {
                let transform = Transform {
                    matrix: Matrix::scale(target.width() as f32, target.height() as f32),
                    tz: 0.0,
                    color_transform: Default::default(),
                    perspective_projection: None,
                };
                let texture = target.take_color_texture();
                let bind_group =
                    self.descriptors
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            layout: &self.descriptors.bind_layouts.bitmap,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(texture.view()),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(
                                        self.descriptors.bitmap_samplers.get_sampler(false, false),
                                    ),
                                },
                            ],
                            label: None,
                        });
                let footprint = Footprint {
                    region: footprint,
                    reads_below: false,
                };
                self.add_to_current(
                    transform.matrix,
                    transform.tz,
                    transform.color_transform,
                    footprint,
                    |instance_index| DrawCommand::RenderTexture {
                        _texture: texture,
                        binds: bind_group,
                        instance_index,
                        blend_mode,
                    },
                );
            }
            blend_type => {
                // Alpha and Erase change the nearest layer, and shaders may
                // change pixels the layer doesn't cover.
                let region = match blend_type {
                    BlendType::Complex(ComplexBlend::Alpha | ComplexBlend::Erase)
                    | BlendType::Shader(_) => None,
                    _ => Some(footprint),
                };
                let chunk_blend_mode = match blend_type {
                    BlendType::Complex(complex) => ChunkBlendMode::Complex(complex),
                    BlendType::Shader(shader) => ChunkBlendMode::Shader(shader),
                    _ => unreachable!(),
                };
                let chunk = Chunk::Blend {
                    texture: target.take_color_texture(),
                    blend_mode: chunk_blend_mode,
                    needs_stencil: self.num_masks > 0,
                };
                if let Some(block) = &mut self.masked {
                    block.regions.push(
                        region.unwrap_or(PixelRegion::for_whole_size(self.width, self.height)),
                    );
                    block.reads_below = true;
                    block.items.push(MaskedItem::Blend(chunk));
                } else if let Some(region) = region {
                    self.add_to_batch(
                        self.width,
                        self.height,
                        &[region],
                        true,
                        None,
                        BatchItem::Blend(chunk),
                    );
                } else {
                    self.flush_batch();
                    self.flush_current();
                    self.result.push(chunk);
                    self.needs_stencil = false;
                }
            }
        }
    }

    fn render_bitmap(
        &mut self,
        bitmap: BitmapHandle,
        transform: Transform,
        smoothing: bool,
        pixel_snapping: PixelSnapping,
        region: PixelRegion,
    ) {
        self.add_bitmap(
            bitmap,
            transform,
            smoothing,
            pixel_snapping,
            region,
            DirectBlend::NORMAL,
        );
    }

    fn render_stage3d(&mut self, bitmap: BitmapHandle, transform: Transform) {
        let mut matrix = transform.matrix;
        {
            let texture = as_texture(&bitmap);
            matrix *= Matrix::scale(
                texture.texture.width() as f32,
                texture.texture.height() as f32,
            );
        }
        let footprint = self.quad_footprint(&matrix);
        self.add_to_current(
            matrix,
            transform.tz,
            transform.color_transform,
            footprint,
            |instance_index| DrawCommand::RenderBitmap {
                bitmap,
                instance_index,
                vertex_offset: None,
                smoothing: false,
                blend_mode: DirectBlend::NORMAL,
                render_stage3d: true,
            },
        );
    }

    fn render_shape(&mut self, shape: ShapeHandle, transform: Transform) {
        self.add_shape(shape, transform, DirectBlend::NORMAL);
    }

    fn draw_rect(&mut self, color: Color, matrix: Matrix) {
        let footprint = self.quad_footprint(&matrix);
        self.add_to_current(
            matrix,
            0.0,
            ColorTransform::multiply_from(color),
            footprint,
            |instance_index| DrawCommand::DrawRect { instance_index },
        );
    }

    fn draw_line(&mut self, color: Color, mut matrix: Matrix) {
        if self.emulate_lines {
            let mut cl = CommandList::new();
            emulate_line(&mut cl, color, matrix);
            cl.execute(self);
        } else {
            matrix.tx += Twips::HALF_PX;
            matrix.ty += Twips::HALF_PX;
            let footprint = self.quad_footprint(&matrix);
            self.add_to_current(
                matrix,
                0.0,
                ColorTransform::multiply_from(color),
                footprint,
                |instance_index| DrawCommand::DrawLine { instance_index },
            );
        }
    }

    fn draw_line_rect(&mut self, color: Color, mut matrix: Matrix) {
        if self.emulate_lines {
            let mut cl = CommandList::new();
            emulate_line_rect(&mut cl, color, matrix);
            cl.execute(self);
        } else {
            matrix.tx += Twips::HALF_PX;
            matrix.ty += Twips::HALF_PX;
            let footprint = self.quad_footprint(&matrix);
            self.add_to_current(
                matrix,
                0.0,
                ColorTransform::multiply_from(color),
                footprint,
                |instance_index| DrawCommand::DrawLineRect { instance_index },
            );
        }
    }

    fn push_mask(&mut self) {
        self.num_masks += 1;
        self.masked
            .get_or_insert_with(Default::default)
            .items
            .push(MaskedItem::Mask(DrawCommand::PushMask));
    }

    fn activate_mask(&mut self) {
        self.record_mask(DrawCommand::ActivateMask);
    }

    fn deactivate_mask(&mut self) {
        self.record_mask(DrawCommand::DeactivateMask);
    }

    fn pop_mask(&mut self) {
        self.num_masks -= 1;
        self.record_mask(DrawCommand::PopMask);
        if self.num_masks == 0 {
            self.finish_masked_block();
        }
    }

    fn render_alpha_mask(&mut self, maskee_commands: CommandList, mask_commands: CommandList) {
        let surface = Surface::new(
            self.descriptors,
            self.quality,
            self.width,
            self.height,
            wgpu::TextureFormat::Rgba8Unorm,
        );

        let maskee = surface.draw_commands(
            RenderTargetMode::FreshWithColor(wgpu::Color::TRANSPARENT),
            self.descriptors,
            self.meshes,
            maskee_commands,
            self.staging_belt,
            self.dynamic_transforms,
            self.draw_encoder,
            LayerRef::None,
            self.texture_pool,
        );
        maskee.ensure_cleared(self.draw_encoder);
        let matrix = Matrix::scale(maskee.width() as f32, maskee.height() as f32);
        let maskee = maskee.take_color_texture();

        let mask = surface.draw_commands(
            RenderTargetMode::FreshWithColor(wgpu::Color::TRANSPARENT),
            self.descriptors,
            self.meshes,
            mask_commands,
            self.staging_belt,
            self.dynamic_transforms,
            self.draw_encoder,
            LayerRef::None,
            self.texture_pool,
        );
        mask.ensure_cleared(self.draw_encoder);
        let mask = mask.take_color_texture();

        let binds = self
            .descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                layout: &self.descriptors.bind_layouts.alpha_mask,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(maskee.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(mask.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            self.descriptors.bitmap_samplers.get_sampler(false, false),
                        ),
                    },
                ],
                label: None,
            });

        let footprint = self.quad_footprint(&matrix);
        self.add_to_current(
            matrix,
            0.0,
            Default::default(),
            footprint,
            |instance_index| DrawCommand::RenderAlphaMask {
                maskee,
                mask,
                binds,
                instance_index,
            },
        );
    }
}
