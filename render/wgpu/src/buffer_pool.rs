use crate::descriptors::Descriptors;
use crate::globals::Globals;
use fnv::FnvHashMap;
use std::fmt::{Debug, Formatter};
use std::ops::Deref;
use std::sync::{Arc, Mutex, Weak};

type PoolInner<T> = Mutex<Vec<T>>;
type Constructor<Type, Description> = Box<dyn Fn(&Descriptors, &Description) -> Type>;

#[derive(Debug, Default)]
pub struct TexturePool {
    pools: FnvHashMap<TextureKey, BufferPool<(wgpu::Texture, wgpu::TextureView), AlwaysCompatible>>,
    globals_cache: FnvHashMap<GlobalsKey, Arc<Globals>>,
    frame: u64,
    last_used: FnvHashMap<TextureKey, u64>,
    globals_last_used: FnvHashMap<GlobalsKey, u64>,
}

impl TexturePool {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn get_texture(
        &mut self,
        descriptors: &Descriptors,
        size: wgpu::Extent3d,
        usage: wgpu::TextureUsages,
        format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> PoolEntry<(wgpu::Texture, wgpu::TextureView), AlwaysCompatible> {
        let key = TextureKey {
            size,
            usage,
            format,
            sample_count,
        };
        self.last_used.insert(key, self.frame);
        let pool = self.pools.entry(key).or_insert_with(|| {
            let label = if cfg!(feature = "render_debug_labels") {
                use std::sync::atomic::{AtomicU32, Ordering};
                static ID_COUNT: AtomicU32 = AtomicU32::new(0);
                let id = ID_COUNT.fetch_add(1, Ordering::Relaxed);
                create_debug_label!("Pooled texture {}", id)
            } else {
                None
            };
            BufferPool::new(Box::new(move |descriptors, _description| {
                crate::stats::count(&crate::stats::POOL_TEXTURES_CREATED);
                let texture = descriptors.device.create_texture(&wgpu::TextureDescriptor {
                    label: label.as_deref(),
                    size,
                    mip_level_count: 1,
                    sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    view_formats: &[format],
                    usage,
                });
                let view = texture.create_view(&Default::default());
                (texture, view)
            }))
        });
        pool.take(descriptors, AlwaysCompatible)
    }

    pub fn get_globals(
        &mut self,
        descriptors: &Descriptors,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Arc<Globals> {
        let key = GlobalsKey {
            viewport_width,
            viewport_height,
        };
        self.globals_last_used.insert(key, self.frame);
        self.globals_cache
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Globals::new(
                    &descriptors.device,
                    &descriptors.bind_layouts.globals,
                    viewport_width,
                    viewport_height,
                ))
            })
            .clone()
    }

    pub fn idle_bytes(&self) -> u64 {
        self.pools
            .iter()
            .map(|(key, pool)| key.bytes() * pool.len() as u64)
            .sum()
    }

    pub fn end_frame(&mut self, max_idle_frames: u64, max_idle_bytes: u64) {
        let frame = self.frame;
        let fresh = |last: &u64| frame - *last <= max_idle_frames;
        let mut idle: Vec<_> = self
            .last_used
            .iter()
            .filter(|(_, last)| **last != frame)
            .map(|(key, last)| (*last, *key))
            .collect();
        idle.sort_unstable_by_key(|(last, _)| std::cmp::Reverse(*last));
        let mut kept_bytes = 0;
        for (last, key) in idle {
            kept_bytes += self
                .pools
                .get(&key)
                .map_or(0, |pool| key.bytes() * pool.len() as u64);
            if !fresh(&last) || kept_bytes > max_idle_bytes {
                self.last_used.remove(&key);
            }
        }
        self.pools.retain(|key, pool| {
            let used = self.last_used.contains_key(key);
            if !used {
                pool.destroy_available();
            }
            used
        });
        self.globals_last_used.retain(|_, last| fresh(last));
        self.globals_cache
            .retain(|key, _| self.globals_last_used.contains_key(key));
        self.frame += 1;
    }

    pub fn destroy_idle_textures(&self) {
        for pool in self.pools.values() {
            pool.destroy_available();
        }
    }
}

impl<Description: BufferDescription> BufferPool<(wgpu::Texture, wgpu::TextureView), Description> {
    fn destroy_available(&self) {
        let available = std::mem::take(
            &mut *self
                .available
                .lock()
                .expect("Should not be able to lock recursively"),
        );
        for ((texture, _), _) in available {
            texture.destroy();
        }
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
struct TextureKey {
    size: wgpu::Extent3d,
    usage: wgpu::TextureUsages,
    format: wgpu::TextureFormat,
    sample_count: u32,
}

impl TextureKey {
    fn bytes(&self) -> u64 {
        let texel = self.format.block_copy_size(None).unwrap_or(4);
        u64::from(self.size.width)
            * u64::from(self.size.height)
            * u64::from(texel)
            * u64::from(self.sample_count)
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
struct GlobalsKey {
    viewport_width: u32,
    viewport_height: u32,
}

pub trait BufferDescription: Clone + Debug {
    type Cost: Ord;

    /// If the potential buffer represented by this description (`self`)
    /// fits another existing buffer and its description (`other`),
    /// return the cost to use that buffer instead of making a new one.
    ///
    /// Cost is an arbitrary unit, but lower is better.
    /// None means that the other buffer cannot be used in place of this one.
    fn cost_to_use(&self, other: &Self) -> Option<Self::Cost>;
}

#[derive(Clone, Debug)]
pub struct AlwaysCompatible;

impl BufferDescription for AlwaysCompatible {
    type Cost = ();

    fn cost_to_use(&self, _other: &Self) -> Option<()> {
        Some(())
    }
}

pub struct BufferPool<Type, Description: BufferDescription> {
    available: Arc<PoolInner<(Type, Description)>>,
    constructor: Constructor<Type, Description>,
}

impl<Type, Description: BufferDescription> Debug for BufferPool<Type, Description> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool").finish()
    }
}

impl<Type, Description: BufferDescription> BufferPool<Type, Description> {
    pub fn new(constructor: Constructor<Type, Description>) -> Self {
        Self {
            available: Arc::new(Mutex::new(vec![])),
            constructor,
        }
    }

    pub fn len(&self) -> usize {
        self.available
            .lock()
            .expect("Should not be able to lock recursively")
            .len()
    }

    pub fn take(
        &self,
        descriptors: &Descriptors,
        description: Description,
    ) -> PoolEntry<Type, Description> {
        let mut guard = self
            .available
            .lock()
            .expect("Should not be able to lock recursively");
        let mut best: Option<(Description::Cost, usize)> = None;
        for i in 0..guard.len() {
            if let Some(cost) = description.cost_to_use(&guard[i].1) {
                if let Some(best) = &mut best {
                    if best.0 > cost {
                        *best = (cost, i);
                    }
                } else if best.is_none() {
                    best = Some((cost, i));
                }
            }
        }

        let (item, used_description) = if let Some((_, best)) = best {
            guard.swap_remove(best)
        } else {
            let item = (self.constructor)(descriptors, &description);
            (item, description)
        };
        PoolEntry {
            item: Some(item),
            description: used_description,
            pool: Arc::downgrade(&self.available),
        }
    }
}

pub struct PoolEntry<Type, Description: BufferDescription> {
    item: Option<Type>,
    description: Description,
    pool: Weak<PoolInner<(Type, Description)>>,
}

impl<Type, Description: BufferDescription> Debug for PoolEntry<Type, Description>
where
    Type: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PoolEntry").field(&self.item).finish()
    }
}

impl<Type, Description: BufferDescription> Drop for PoolEntry<Type, Description> {
    fn drop(&mut self) {
        if let Some(item) = self.item.take()
            && let Some(pool) = self.pool.upgrade()
        {
            pool.lock()
                .expect("Should not be able to lock recursively")
                .push((item, self.description.clone()))
        }
    }
}

impl<Type, Description: BufferDescription> Deref for PoolEntry<Type, Description> {
    type Target = Type;

    fn deref(&self) -> &Self::Target {
        self.item.as_ref().expect("Item should exist until dropped")
    }
}

/// Uniforms and vertices needed only by the frame being recorded, written
/// through a staging belt at increasing offsets of one buffer. wgpu's WebGPU
/// backend doesn't destroy a dropped buffer, so a buffer made for each of them
/// keeps its shared memory in the browser's GPU process until the JavaScript
/// collector finds it.
#[derive(Debug)]
pub struct ScratchBuffer {
    buffer: wgpu::Buffer,
    used: u64,
    retired: Vec<wgpu::Buffer>,
}

impl ScratchBuffer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: Self::create(device, 256 * 1024),
            used: 0,
            retired: Vec::new(),
        }
    }

    fn create(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: create_debug_label!("Scratch buffer").as_deref(),
            size,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn write(
        &mut self,
        device: &wgpu::Device,
        staging_belt: &mut wgpu::util::StagingBelt,
        encoder: &mut wgpu::CommandEncoder,
        data: &[u8],
        alignment: u64,
    ) -> (wgpu::Buffer, wgpu::BufferAddress) {
        let size = wgpu::BufferSize::new(data.len() as u64).expect("Scratch data isn't empty");
        let mut offset = self.used.next_multiple_of(alignment);
        if offset + size.get() > self.buffer.size() {
            let grown = Self::create(
                device,
                (self.buffer.size() * 2).max(size.get().next_power_of_two()),
            );
            self.retired
                .push(std::mem::replace(&mut self.buffer, grown));
            offset = 0;
        }
        staging_belt
            .write_buffer(encoder, &self.buffer, offset, size)
            .copy_from_slice(data);
        self.used = offset + size.get();
        (self.buffer.clone(), offset)
    }

    /// Call once everything recorded since the last call has been submitted.
    pub fn reset(&mut self) {
        self.used = 0;
        for buffer in self.retired.drain(..) {
            buffer.destroy();
        }
    }
}
