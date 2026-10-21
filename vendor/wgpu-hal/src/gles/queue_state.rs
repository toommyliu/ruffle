//! Built fresh for each `Queue::submit`, since GL calls outside a submit don't go through it.

use alloc::vec::Vec;

use glow::HasContext;
use naga::FastHashMap;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum AttachmentKey {
    Texture {
        raw: glow::Texture,
        target: u32,
        level: i32,
    },
    Renderbuffer(glow::Renderbuffer),
}

#[derive(Clone, Copy)]
struct Slot {
    /// `None` while what's attached is unknown.
    attached: Option<Option<(u32, AttachmentKey)>>,
    /// Attached by an earlier pass, and detached before the next command unless this pass
    /// attaches the same thing again.
    stale: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct AttributePointer {
    pub buffer: glow::Buffer,
    pub size: i32,
    pub data_type: u32,
    pub integer: bool,
    pub stride: i32,
    pub offset: i32,
}

#[derive(Clone, Copy, Default)]
struct Attribute {
    enabled: Option<bool>,
    disable_pending: bool,
    pointer: Option<AttributePointer>,
    divisor: Option<u32>,
}

fn slot<T: Default + Clone>(list: &mut Vec<T>, index: usize) -> &mut T {
    if list.len() <= index {
        list.resize(index + 1, T::default());
    }
    &mut list[index]
}

fn faces(face: u32) -> &'static [usize] {
    match face {
        glow::FRONT => &[0],
        glow::BACK => &[1],
        _ => &[0, 1],
    }
}

pub(super) struct StateCache {
    draw_fbo: glow::Framebuffer,
    caps: Vec<(u32, bool)>,
    program: Option<Option<glow::Program>>,
    draw_framebuffer: Option<Option<glow::Framebuffer>>,
    read_framebuffer: Option<Option<glow::Framebuffer>>,
    /// `draw_fbo`'s color attachments, then its depth and stencil.
    attachments: Vec<Slot>,
    detach_pending: bool,
    draw_buffers: Option<u8>,
    color_mask: Option<[bool; 4]>,
    blend_equation: Option<(u32, u32)>,
    blend_func: Option<[u32; 4]>,
    depth_mask: Option<bool>,
    depth_func: Option<u32>,
    stencil_func: [Option<(u32, i32, u32)>; 2],
    stencil_write_mask: [Option<u32>; 2],
    stencil_ops: [Option<(u32, u32, u32)>; 2],
    scissor: Option<[i32; 4]>,
    viewport: Option<[i32; 4]>,
    depth_range: Option<(f32, f32)>,
    front_face: Option<u32>,
    cull_face: Option<u32>,
    array_buffer: Option<Option<glow::Buffer>>,
    element_array_buffer: Option<Option<glow::Buffer>>,
    attributes: Vec<Attribute>,
    active_texture: Option<u32>,
    texture_units: Vec<Option<(u32, glow::Texture)>>,
    texture_levels: FastHashMap<glow::Texture, (i32, i32)>,
    samplers: Vec<Option<Option<glow::Sampler>>>,
    uniform_buffers: Vec<Option<(glow::Buffer, i32, i32)>>,
}

impl StateCache {
    pub fn new(draw_fbo: glow::Framebuffer, max_color_attachments: u32) -> Self {
        Self {
            draw_fbo,
            caps: Vec::new(),
            program: None,
            draw_framebuffer: None,
            read_framebuffer: None,
            attachments: alloc::vec![
                Slot {
                    attached: None,
                    stale: false,
                };
                max_color_attachments as usize + 1
            ],
            detach_pending: false,
            draw_buffers: None,
            color_mask: None,
            blend_equation: None,
            blend_func: None,
            depth_mask: None,
            depth_func: None,
            stencil_func: [None; 2],
            stencil_write_mask: [None; 2],
            stencil_ops: [None; 2],
            scissor: None,
            viewport: None,
            depth_range: None,
            front_face: None,
            cull_face: None,
            array_buffer: None,
            element_array_buffer: None,
            attributes: Vec::new(),
            active_texture: None,
            texture_units: Vec::new(),
            texture_levels: FastHashMap::default(),
            samplers: Vec::new(),
            uniform_buffers: Vec::new(),
        }
    }

    pub unsafe fn set_cap(&mut self, gl: &glow::Context, cap: u32, on: bool) {
        match self.caps.iter_mut().find(|(c, _)| *c == cap) {
            Some((_, current)) if *current == on => return,
            Some((_, current)) => *current = on,
            None => self.caps.push((cap, on)),
        }
        if on {
            unsafe { gl.enable(cap) };
        } else {
            unsafe { gl.disable(cap) };
        }
    }

    pub fn forget_cap(&mut self, cap: u32) {
        self.caps.retain(|(c, _)| *c != cap);
    }

    pub unsafe fn use_program(&mut self, gl: &glow::Context, program: Option<glow::Program>) {
        if self.program != Some(program) {
            unsafe { gl.use_program(program) };
            self.program = Some(program);
        }
    }

    pub unsafe fn bind_framebuffer(
        &mut self,
        gl: &glow::Context,
        target: u32,
        framebuffer: Option<glow::Framebuffer>,
    ) {
        let (draw, read) = match target {
            glow::DRAW_FRAMEBUFFER => (true, false),
            glow::READ_FRAMEBUFFER => (false, true),
            _ => (true, true),
        };
        if (!draw || self.draw_framebuffer == Some(framebuffer))
            && (!read || self.read_framebuffer == Some(framebuffer))
        {
            return;
        }
        unsafe { gl.bind_framebuffer(target, framebuffer) };
        if draw {
            self.draw_framebuffer = Some(framebuffer);
        }
        if read {
            self.read_framebuffer = Some(framebuffer);
        }
    }

    pub fn forget_framebuffers(&mut self) {
        self.draw_framebuffer = None;
        self.read_framebuffer = None;
    }

    /// Starts a pass on `draw_fbo`, which must be bound as the draw framebuffer.
    pub fn reset_attachments(&mut self) {
        for slot in &mut self.attachments {
            if slot.attached != Some(None) {
                slot.stale = true;
                self.detach_pending = true;
            }
        }
    }

    /// `draw_fbo` must be bound as the draw framebuffer. `key` is `None` for attachments this
    /// can't compare.
    pub unsafe fn attach(
        &mut self,
        gl: &glow::Context,
        point: u32,
        key: Option<AttachmentKey>,
        attach: impl FnOnce(),
    ) {
        let index = match point {
            glow::DEPTH_ATTACHMENT | glow::STENCIL_ATTACHMENT | glow::DEPTH_STENCIL_ATTACHMENT => {
                self.attachments.len() - 1
            }
            _ => (point - glow::COLOR_ATTACHMENT0) as usize,
        };
        let is_depth_stencil = index == self.attachments.len() - 1;
        let slot = &mut self.attachments[index];
        if let Some(key) = key {
            if slot.attached == Some(Some((point, key))) {
                slot.stale = false;
                return;
            }
        }
        // Depth and stencil share a slot, attached at three different points.
        let replaced_elsewhere = match slot.attached {
            None => true,
            Some(None) => false,
            Some(Some((old_point, _))) => old_point != point,
        };
        if is_depth_stencil && replaced_elsewhere {
            unsafe {
                gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::TEXTURE_2D,
                    None,
                    0,
                )
            };
        }
        attach();
        *slot = Slot {
            attached: key.map(|key| Some((point, key))),
            stale: false,
        };
    }

    pub unsafe fn flush_attachments(&mut self, gl: &glow::Context) {
        if !self.detach_pending {
            return;
        }
        self.detach_pending = false;
        let last = self.attachments.len() - 1;
        for (index, slot) in self.attachments.iter_mut().enumerate() {
            if !slot.stale {
                continue;
            }
            let point = if index == last {
                glow::DEPTH_STENCIL_ATTACHMENT
            } else {
                glow::COLOR_ATTACHMENT0 + index as u32
            };
            unsafe {
                gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, point, glow::TEXTURE_2D, None, 0)
            };
            *slot = Slot {
                attached: Some(None),
                stale: false,
            };
        }
    }

    pub unsafe fn draw_buffers(&mut self, gl: &glow::Context, count: u8, indices: &[u32]) {
        let on_draw_fbo = self.draw_framebuffer == Some(Some(self.draw_fbo));
        if on_draw_fbo && self.draw_buffers == Some(count) {
            return;
        }
        unsafe { gl.draw_buffers(indices) };
        self.draw_buffers = on_draw_fbo.then_some(count);
    }

    pub fn forget_draw_buffers(&mut self) {
        self.draw_buffers = None;
    }

    pub unsafe fn color_mask(&mut self, gl: &glow::Context, mask: [bool; 4]) {
        if self.color_mask != Some(mask) {
            unsafe { gl.color_mask(mask[0], mask[1], mask[2], mask[3]) };
            self.color_mask = Some(mask);
        }
    }

    pub unsafe fn blend_equation(&mut self, gl: &glow::Context, color: u32, alpha: u32) {
        if self.blend_equation == Some((color, alpha)) {
            return;
        }
        if color == alpha {
            unsafe { gl.blend_equation(color) };
        } else {
            unsafe { gl.blend_equation_separate(color, alpha) };
        }
        self.blend_equation = Some((color, alpha));
    }

    pub unsafe fn blend_func(&mut self, gl: &glow::Context, func: [u32; 4]) {
        if self.blend_func == Some(func) {
            return;
        }
        if func[0] == func[2] && func[1] == func[3] {
            unsafe { gl.blend_func(func[0], func[1]) };
        } else {
            unsafe { gl.blend_func_separate(func[0], func[1], func[2], func[3]) };
        }
        self.blend_func = Some(func);
    }

    /// For calls that set one draw buffer's color mask or blending.
    pub fn forget_blending(&mut self) {
        self.color_mask = None;
        self.blend_equation = None;
        self.blend_func = None;
        self.forget_cap(glow::BLEND);
    }

    pub unsafe fn depth_mask(&mut self, gl: &glow::Context, mask: bool) {
        if self.depth_mask != Some(mask) {
            unsafe { gl.depth_mask(mask) };
            self.depth_mask = Some(mask);
        }
    }

    pub unsafe fn depth_func(&mut self, gl: &glow::Context, function: u32) {
        if self.depth_func != Some(function) {
            unsafe { gl.depth_func(function) };
            self.depth_func = Some(function);
        }
    }

    pub unsafe fn stencil_func(
        &mut self,
        gl: &glow::Context,
        face: u32,
        function: u32,
        reference: i32,
        read_mask: u32,
    ) {
        let value = Some((function, reference, read_mask));
        let faces = faces(face);
        if faces.iter().all(|&f| self.stencil_func[f] == value) {
            return;
        }
        unsafe { gl.stencil_func_separate(face, function, reference, read_mask) };
        for &f in faces {
            self.stencil_func[f] = value;
        }
    }

    pub unsafe fn stencil_write_mask(&mut self, gl: &glow::Context, face: u32, mask: u32) {
        let faces = faces(face);
        if faces
            .iter()
            .all(|&f| self.stencil_write_mask[f] == Some(mask))
        {
            return;
        }
        if face == glow::FRONT_AND_BACK {
            unsafe { gl.stencil_mask(mask) };
        } else {
            unsafe { gl.stencil_mask_separate(face, mask) };
        }
        for &f in faces {
            self.stencil_write_mask[f] = Some(mask);
        }
    }

    pub unsafe fn stencil_ops(&mut self, gl: &glow::Context, face: u32, ops: (u32, u32, u32)) {
        let faces = faces(face);
        if faces.iter().all(|&f| self.stencil_ops[f] == Some(ops)) {
            return;
        }
        unsafe { gl.stencil_op_separate(face, ops.0, ops.1, ops.2) };
        for &f in faces {
            self.stencil_ops[f] = Some(ops);
        }
    }

    pub unsafe fn scissor(&mut self, gl: &glow::Context, rect: [i32; 4]) {
        if self.scissor != Some(rect) {
            unsafe { gl.scissor(rect[0], rect[1], rect[2], rect[3]) };
            self.scissor = Some(rect);
        }
    }

    pub unsafe fn viewport(&mut self, gl: &glow::Context, rect: [i32; 4]) {
        if self.viewport != Some(rect) {
            unsafe { gl.viewport(rect[0], rect[1], rect[2], rect[3]) };
            self.viewport = Some(rect);
        }
    }

    pub unsafe fn depth_range(&mut self, gl: &glow::Context, near: f32, far: f32) {
        if self.depth_range != Some((near, far)) {
            unsafe { gl.depth_range_f32(near, far) };
            self.depth_range = Some((near, far));
        }
    }

    pub unsafe fn front_face(&mut self, gl: &glow::Context, face: u32) {
        if self.front_face != Some(face) {
            unsafe { gl.front_face(face) };
            self.front_face = Some(face);
        }
    }

    pub unsafe fn cull_face(&mut self, gl: &glow::Context, face: u32) {
        if self.cull_face != Some(face) {
            unsafe { gl.cull_face(face) };
            self.cull_face = Some(face);
        }
    }

    pub unsafe fn bind_buffer(
        &mut self,
        gl: &glow::Context,
        target: u32,
        buffer: Option<glow::Buffer>,
    ) {
        let cached = match target {
            glow::ARRAY_BUFFER => &mut self.array_buffer,
            glow::ELEMENT_ARRAY_BUFFER => &mut self.element_array_buffer,
            _ => {
                unsafe { gl.bind_buffer(target, buffer) };
                return;
            }
        };
        if *cached != Some(buffer) {
            unsafe { gl.bind_buffer(target, buffer) };
            *cached = Some(buffer);
        }
    }

    pub unsafe fn enable_attribute(&mut self, gl: &glow::Context, location: u32) {
        let attribute = slot(&mut self.attributes, location as usize);
        attribute.disable_pending = false;
        if attribute.enabled != Some(true) {
            unsafe { gl.enable_vertex_attrib_array(location) };
            attribute.enabled = Some(true);
        }
    }

    /// Disables the attribute before the next draw, unless something enables it first.
    pub fn disable_attribute(&mut self, location: u32) {
        let attribute = slot(&mut self.attributes, location as usize);
        if attribute.enabled != Some(false) {
            attribute.disable_pending = true;
        }
    }

    pub unsafe fn flush_attributes(&mut self, gl: &glow::Context) {
        for (location, attribute) in self.attributes.iter_mut().enumerate() {
            if attribute.disable_pending {
                unsafe { gl.disable_vertex_attrib_array(location as u32) };
                attribute.enabled = Some(false);
                attribute.disable_pending = false;
            }
        }
    }

    pub unsafe fn attribute_pointer(
        &mut self,
        gl: &glow::Context,
        location: u32,
        pointer: AttributePointer,
    ) {
        if slot(&mut self.attributes, location as usize).pointer == Some(pointer) {
            return;
        }
        unsafe { self.bind_buffer(gl, glow::ARRAY_BUFFER, Some(pointer.buffer)) };
        if pointer.integer {
            unsafe {
                gl.vertex_attrib_pointer_i32(
                    location,
                    pointer.size,
                    pointer.data_type,
                    pointer.stride,
                    pointer.offset,
                )
            };
        } else {
            unsafe {
                gl.vertex_attrib_pointer_f32(
                    location,
                    pointer.size,
                    pointer.data_type,
                    true,
                    pointer.stride,
                    pointer.offset,
                )
            };
        }
        self.attributes[location as usize].pointer = Some(pointer);
    }

    pub fn forget_attribute_pointer(&mut self, location: u32) {
        slot(&mut self.attributes, location as usize).pointer = None;
    }

    pub unsafe fn attribute_divisor(&mut self, gl: &glow::Context, location: u32, divisor: u32) {
        let attribute = slot(&mut self.attributes, location as usize);
        if attribute.divisor != Some(divisor) {
            unsafe { gl.vertex_attrib_divisor(location, divisor) };
            attribute.divisor = Some(divisor);
        }
    }

    pub unsafe fn active_texture(&mut self, gl: &glow::Context, unit: u32) {
        if self.active_texture != Some(unit) {
            unsafe { gl.active_texture(glow::TEXTURE0 + unit) };
            self.active_texture = Some(unit);
        }
    }

    /// Binds to the active unit.
    pub unsafe fn bind_texture(&mut self, gl: &glow::Context, target: u32, texture: glow::Texture) {
        let Some(unit) = self.active_texture else {
            unsafe { gl.bind_texture(target, Some(texture)) };
            self.texture_units.clear();
            return;
        };
        let bound = slot(&mut self.texture_units, unit as usize);
        if *bound != Some((target, texture)) {
            unsafe { gl.bind_texture(target, Some(texture)) };
            *bound = Some((target, texture));
        }
    }

    /// Sets the levels of the texture bound to `target` on the active unit.
    pub unsafe fn texture_levels(
        &mut self,
        gl: &glow::Context,
        target: u32,
        texture: glow::Texture,
        levels: (i32, i32),
    ) {
        if self.texture_levels.get(&texture) == Some(&levels) {
            return;
        }
        unsafe { gl.tex_parameter_i32(target, glow::TEXTURE_BASE_LEVEL, levels.0) };
        unsafe { gl.tex_parameter_i32(target, glow::TEXTURE_MAX_LEVEL, levels.1) };
        self.texture_levels.insert(texture, levels);
    }

    pub unsafe fn bind_sampler(
        &mut self,
        gl: &glow::Context,
        unit: u32,
        sampler: Option<glow::Sampler>,
    ) {
        let bound = slot(&mut self.samplers, unit as usize);
        if *bound != Some(sampler) {
            unsafe { gl.bind_sampler(unit, sampler) };
            *bound = Some(sampler);
        }
    }

    pub unsafe fn bind_buffer_range(
        &mut self,
        gl: &glow::Context,
        target: u32,
        index: u32,
        buffer: glow::Buffer,
        offset: i32,
        size: i32,
    ) {
        if target != glow::UNIFORM_BUFFER {
            unsafe { gl.bind_buffer_range(target, index, Some(buffer), offset, size) };
            return;
        }
        let bound = slot(&mut self.uniform_buffers, index as usize);
        if *bound != Some((buffer, offset, size)) {
            unsafe { gl.bind_buffer_range(target, index, Some(buffer), offset, size) };
            *bound = Some((buffer, offset, size));
        }
    }
}
