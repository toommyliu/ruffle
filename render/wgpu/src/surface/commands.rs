use super::target::PoolOrArcTexture;
use crate::backend::RenderTargetMode;
use crate::blend::{BlendType, ComplexBlend, DirectBlend, TrivialBlend};
use crate::buffer_builder::BufferBuilder;
use crate::buffer_pool::TexturePool;
use crate::dynamic_transforms::DynamicTransforms;
use crate::mesh::{DrawType, Mesh, as_mesh};
use crate::surface::Surface;
use crate::surface::target::CommandTarget;
use crate::{
    Descriptors, MaskState, Pipelines, PosColorVertex, PosUvVertex, PosVertex, Transforms,
    as_texture,
};
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
    pipeline: Option<&'encoder wgpu::RenderPipeline>,
    bind_group: Option<&'encoder wgpu::BindGroup>,
    vertex_buffer: Option<wgpu::BufferSlice<'encoder>>,
    index_buffer: Option<(&'encoder wgpu::Buffer, wgpu::IndexFormat)>,
    stencil_reference: Option<u32>,
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
            pipeline: None,
            bind_group: None,
            vertex_buffer: None,
            index_buffer: None,
            stencil_reference: None,
        }
    }

    fn set_pipeline(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        pipeline: &'encoder wgpu::RenderPipeline,
    ) {
        if self.pipeline != Some(pipeline) {
            render_pass.set_pipeline(pipeline);
            self.pipeline = Some(pipeline);
        }
    }

    fn set_bind_group(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
    ) {
        if self.bind_group != Some(bind_group) {
            render_pass.set_bind_group(2, bind_group, &[]);
            self.bind_group = Some(bind_group);
        }
    }

    fn set_stencil_reference(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        reference: u32,
    ) {
        if self.stencil_reference != Some(reference) {
            render_pass.set_stencil_reference(reference);
            self.stencil_reference = Some(reference);
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
                    self.set_stencil_reference(render_pass, self.num_masks - 1);
                }
                MaskState::DrawMaskedContent | MaskState::ClearMaskStencil => {
                    self.set_stencil_reference(render_pass, self.num_masks);
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

    pub fn prep_color(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>, blend: DirectBlend) {
        let pipelines = self.pipelines.color(blend);
        if self.needs_stencil {
            self.set_pipeline(render_pass, pipelines.pipeline_for(self.mask_state));
        } else {
            self.set_pipeline(render_pass, pipelines.stencilless_pipeline());
        }
        self.bind_parent_copy(render_pass, blend);
    }

    pub fn prep_lines(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        if self.needs_stencil {
            self.set_pipeline(
                render_pass,
                self.pipelines.lines.pipeline_for(self.mask_state),
            );
        } else {
            self.set_pipeline(render_pass, self.pipelines.lines.stencilless_pipeline());
        }
    }

    pub fn prep_gradient(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
        blend: DirectBlend,
    ) {
        let pipelines = self.pipelines.gradient(blend);
        if self.needs_stencil {
            self.set_pipeline(render_pass, pipelines.pipeline_for(self.mask_state));
        } else {
            self.set_pipeline(render_pass, pipelines.stencilless_pipeline());
        }

        self.set_bind_group(render_pass, bind_group);
        self.bind_parent_copy(render_pass, blend);
    }

    pub fn prep_bitmap(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
        blend: DirectBlend,
        render_stage3d: bool,
    ) {
        let pipeline = match (self.needs_stencil, render_stage3d) {
            (true, true) => &self.pipelines.bitmap_opaque_dummy_stencil,
            (true, false) => self.pipelines.bitmap(blend).pipeline_for(self.mask_state),
            (false, true) => &self.pipelines.bitmap_opaque,
            (false, false) => self.pipelines.bitmap(blend).stencilless_pipeline(),
        };
        self.set_pipeline(render_pass, pipeline);

        self.set_bind_group(render_pass, bind_group);
        self.bind_parent_copy(render_pass, blend);
    }

    pub fn prep_alpha_mask(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        bind_group: &'encoder wgpu::BindGroup,
    ) {
        if self.needs_stencil {
            self.set_pipeline(
                render_pass,
                self.pipelines.alpha_mask.pipeline_for(self.mask_state),
            );
        } else {
            self.set_pipeline(
                render_pass,
                self.pipelines.alpha_mask.stencilless_pipeline(),
            );
        }

        self.set_bind_group(render_pass, bind_group);
    }

    /// `vertex_offset` is in bytes and a whole number of `V`s; `indices` count
    /// indices from the start of `index_buffer`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw<V>(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        vertex_buffer: &'encoder wgpu::Buffer,
        vertex_offset: wgpu::BufferAddress,
        index_buffer: &'encoder wgpu::Buffer,
        index_format: wgpu::IndexFormat,
        indices: std::ops::Range<u32>,
        instance_index: u32,
    ) {
        let (vertices, base_vertex) = if self.descriptors.base_vertex {
            let base_vertex = vertex_offset / size_of::<V>() as wgpu::BufferAddress;
            (vertex_buffer.slice(..), base_vertex as i32)
        } else {
            (vertex_buffer.slice(vertex_offset..), 0)
        };
        if self.vertex_buffer != Some(vertices) {
            render_pass.set_vertex_buffer(0, vertices);
            self.vertex_buffer = Some(vertices);
        }
        if self.index_buffer != Some((index_buffer, index_format)) {
            render_pass.set_index_buffer(index_buffer.slice(..), index_format);
            self.index_buffer = Some((index_buffer, index_format));
        }

        render_pass.draw_indexed(indices, base_vertex, instance_index..(instance_index + 1));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_bitmap(
        &mut self,
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

        let (vertex_buffer, vertex_offset) = match vertex_offset {
            Some(vertex_offset) => (self.dynamic_vertex_buffer, vertex_offset),
            None => (&descriptors.quad.vertices_pos_uv, 0),
        };

        self.draw::<PosUvVertex>(
            render_pass,
            vertex_buffer,
            vertex_offset,
            &descriptors.quad.indices,
            wgpu::IndexFormat::Uint32,
            0..6,
            instance_index,
        );
    }

    pub fn render_texture(
        &mut self,
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

        self.draw::<PosUvVertex>(
            render_pass,
            &self.descriptors.quad.vertices_pos_uv,
            0,
            &self.descriptors.quad.indices,
            wgpu::IndexFormat::Uint32,
            0..6,
            instance_index,
        );
    }

    pub fn render_shape(
        &mut self,
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

            let first_index = (draw.indices.start
                / wgpu::BufferAddress::from(mesh.index_format.byte_size()))
                as u32;
            let indices = first_index..first_index + num_indices;
            if let DrawType::Color = draw.draw_type {
                self.draw::<PosColorVertex>(
                    render_pass,
                    &mesh.vertex_buffer,
                    draw.vertices.start,
                    &mesh.index_buffer,
                    mesh.index_format,
                    indices,
                    instance_index,
                );
            } else {
                self.draw::<PosUvVertex>(
                    render_pass,
                    &mesh.vertex_buffer,
                    draw.vertices.start,
                    &mesh.index_buffer,
                    mesh.index_format,
                    indices,
                    instance_index,
                );
            }
        }
    }

    pub fn render_alpha_mask(
        &mut self,
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

        self.draw::<PosVertex>(
            render_pass,
            &self.descriptors.quad.vertices_pos,
            0,
            &self.descriptors.quad.indices,
            wgpu::IndexFormat::Uint32,
            0..6,
            instance_index,
        );

        if cfg!(feature = "render_debug_labels") {
            render_pass.pop_debug_group();
        }
    }

    pub fn draw_rect(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>, instance_index: u32) {
        self.prep_color(render_pass, DirectBlend::NORMAL);

        self.draw::<PosColorVertex>(
            render_pass,
            &self.descriptors.quad.vertices_pos_color,
            0,
            &self.descriptors.quad.indices,
            wgpu::IndexFormat::Uint32,
            0..6,
            instance_index,
        );
    }

    pub fn draw_lines<const RECT: bool>(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'encoder>,
        instance_index: u32,
    ) {
        self.prep_lines(render_pass);

        self.draw::<PosColorVertex>(
            render_pass,
            &self.descriptors.quad.vertices_pos_color,
            0,
            if RECT {
                &self.descriptors.quad.indices_line_rect
            } else {
                &self.descriptors.quad.indices_line
            },
            wgpu::IndexFormat::Uint32,
            if RECT { 0..5 } else { 0..2 },
            instance_index,
        );
    }

    pub fn push_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(
            self.mask_state == MaskState::NoMask || self.mask_state == MaskState::DrawMaskedContent
        );
        self.num_masks += 1;
        self.mask_state = MaskState::DrawMaskStencil;
        self.set_stencil_reference(render_pass, self.num_masks - 1);
    }

    pub fn activate_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(self.num_masks > 0 && self.mask_state == MaskState::DrawMaskStencil);
        self.mask_state = MaskState::DrawMaskedContent;
        self.set_stencil_reference(render_pass, self.num_masks);
    }

    pub fn deactivate_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(self.num_masks > 0 && self.mask_state == MaskState::DrawMaskedContent);
        self.mask_state = MaskState::ClearMaskStencil;
        self.set_stencil_reference(render_pass, self.num_masks);
    }

    pub fn pop_mask(&mut self, render_pass: &mut wgpu::RenderPass<'encoder>) {
        debug_assert!(self.num_masks > 0 && self.mask_state == MaskState::ClearMaskStencil);
        self.num_masks -= 1;
        self.set_stencil_reference(render_pass, self.num_masks);
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
        region: Option<LayerRect>,
    },
    CopyParent {
        regions: Vec<PixelRegion>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct LayerRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
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

const LAYER_SIZE_STEP: u32 = 64;

fn reads_other_layers(commands: &CommandList) -> bool {
    commands.commands.iter().any(|command| match command {
        Command::Blend(inner, mode) => {
            matches!(
                mode,
                RenderBlendMode::Builtin(BlendMode::Alpha | BlendMode::Erase)
            ) || reads_other_layers(inner)
        }
        Command::RenderAlphaMask {
            maskee_commands,
            mask_commands,
        } => reads_other_layers(maskee_commands) || reads_other_layers(mask_commands),
        _ => false,
    })
}

fn translate_commands(commands: &mut CommandList, dx: Twips, dy: Twips) {
    for command in &mut commands.commands {
        match command {
            Command::RenderBitmap { transform, .. }
            | Command::RenderShape { transform, .. }
            | Command::RenderStage3D { transform, .. } => {
                transform.matrix.tx += dx;
                transform.matrix.ty += dy;
            }
            Command::DrawRect { matrix, .. }
            | Command::DrawLine { matrix, .. }
            | Command::DrawLineRect { matrix, .. } => {
                matrix.tx += dx;
                matrix.ty += dy;
            }
            Command::Blend(inner, _) => translate_commands(inner, dx, dy),
            Command::RenderAlphaMask {
                maskee_commands,
                mask_commands,
            } => {
                translate_commands(maskee_commands, dx, dy);
                translate_commands(mask_commands, dx, dy);
            }
            Command::PushMask
            | Command::ActivateMask
            | Command::DeactivateMask
            | Command::PopMask => {}
        }
    }
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
        if crate::stats::sequential_draws() {
            self.flush_batch();
        }
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

    fn layer_rect(
        &self,
        blend_mode: &RenderBlendMode,
        commands: &CommandList,
        footprint: PixelRegion,
    ) -> Option<LayerRect> {
        if footprint.is_empty()
            || matches!(
                blend_mode,
                RenderBlendMode::Shader(_)
                    | RenderBlendMode::Builtin(BlendMode::Alpha | BlendMode::Erase)
            )
            || reads_other_layers(commands)
        {
            return None;
        }
        let round = |n: u32| n.div_ceil(LAYER_SIZE_STEP) * LAYER_SIZE_STEP;
        let (width, height) = (round(footprint.width()), round(footprint.height()));
        if u64::from(width) * u64::from(height) * 2 > u64::from(self.width) * u64::from(self.height)
        {
            return None;
        }
        Some(LayerRect {
            x: footprint.x_min,
            y: footprint.y_min,
            width,
            height,
        })
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
            crate::stats::count(&crate::stats::BLEND_LAYERS_DIRECT);
            if region.is_empty() {
                return true;
            }
            if let Some(block) = &mut self.masked {
                block.items.push(MaskedItem::Copy(region));
                block.reads_below = true;
            }
        } else {
            crate::stats::count(&crate::stats::BLEND_LAYERS_DIRECT);
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
        crate::stats::count(
            &crate::stats::BLEND_LAYERS[match &blend_mode {
                RenderBlendMode::Builtin(mode) => *mode as usize,
                RenderBlendMode::Shader(_) => 15,
            }],
        );
        if self.try_blend_directly(&mut commands, &blend_mode) {
            return;
        }
        let footprint = self.commands_footprint(&commands);
        let layer_rect = self.layer_rect(&blend_mode, &commands, footprint);
        if let Some(rect) = layer_rect {
            translate_commands(
                &mut commands,
                -Twips::from_pixels_i32(rect.x as i32),
                -Twips::from_pixels_i32(rect.y as i32),
            );
        }
        let (surface_width, surface_height) =
            layer_rect.map_or((self.width, self.height), |rect| (rect.width, rect.height));
        let surface = Surface::new(
            self.descriptors,
            self.quality,
            surface_width,
            surface_height,
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
                let origin = layer_rect.map_or(Matrix::IDENTITY, |rect| {
                    Matrix::translate(
                        Twips::from_pixels_i32(rect.x as i32),
                        Twips::from_pixels_i32(rect.y as i32),
                    )
                });
                let transform = Transform {
                    matrix: origin * Matrix::scale(target.width() as f32, target.height() as f32),
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
                    region: layer_rect,
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
