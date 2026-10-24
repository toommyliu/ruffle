use crate::descriptors::Descriptors;
use crate::surface::ChunkLists;
use crate::{PosUvVertex, Transforms};
use std::cell::RefCell;
use std::mem;

const MAX_TRANSFORMS_BYTES: u64 = 64 << 10;

pub struct DynamicTransforms {
    pub buffer: wgpu::Buffer,
    pub vertex_buffer: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
    /// One for each surface being drawn: a blend layer's surface is drawn
    /// while its parent's lists are in use.
    lists: RefCell<Vec<ChunkLists>>,
}

pub fn transforms_per_draw(limits: &wgpu::Limits) -> u64 {
    limits
        .max_uniform_buffer_binding_size
        .min(MAX_TRANSFORMS_BYTES)
        / size_of::<Transforms>() as u64
}

impl DynamicTransforms {
    pub fn new(descriptors: &Descriptors) -> Self {
        let buffer = descriptors.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: transforms_per_draw(&descriptors.limits) * size_of::<Transforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vertex_buffer = descriptors.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (mem::size_of::<PosUvVertex>() as u64
                * 4
                * transforms_per_draw(&descriptors.limits))
            .min(descriptors.limits.max_buffer_size),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &descriptors.bind_layouts.transforms,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
        Self {
            buffer,
            bind_group,
            vertex_buffer,
            lists: RefCell::default(),
        }
    }

    pub fn take_lists(&self) -> ChunkLists {
        self.lists.borrow_mut().pop().unwrap_or_default()
    }

    pub fn return_lists(&self, mut lists: ChunkLists) {
        lists.clear();
        self.lists.borrow_mut().push(lists);
    }

    pub fn drop_lists(&self) {
        self.lists.take();
    }
}
