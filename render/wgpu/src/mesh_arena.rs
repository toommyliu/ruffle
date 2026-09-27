use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, Weak};

#[derive(Debug)]
pub struct ArenaAllocation {
    buffer: wgpu::Buffer,
    range: Range<wgpu::BufferAddress>,
    chunk: usize,
    arena: Weak<Mutex<ArenaState>>,
}

impl ArenaAllocation {
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn offset(&self) -> wgpu::BufferAddress {
        self.range.start
    }
}

impl Drop for ArenaAllocation {
    fn drop(&mut self) {
        if let Some(arena) = self.arena.upgrade() {
            let mut state = arena.lock().expect("Arena lock shouldn't be poisoned");
            let frame = state.frame;
            state
                .pending_frees
                .push((frame, self.chunk, self.range.clone()));
        }
    }
}

#[derive(Debug)]
pub struct BufferArena {
    state: Arc<Mutex<ArenaState>>,
    label: &'static str,
    usage: wgpu::BufferUsages,
    alignment: wgpu::BufferAddress,
    chunk_size: wgpu::BufferAddress,
}

#[derive(Debug, Default)]
struct ArenaState {
    chunks: Vec<Option<Chunk>>,
    /// Ranges freed in a frame, reusable once that frame's GPU work can't
    /// still be waiting to read them: recorded draws of a dropped mesh are
    /// submitted after the drop, and a queue write for a new mesh in the
    /// same range would land first.
    pending_frees: Vec<(u64, usize, Range<wgpu::BufferAddress>)>,
    frame: u64,
}

#[derive(Debug)]
struct Chunk {
    buffer: wgpu::Buffer,
    free: BTreeMap<wgpu::BufferAddress, wgpu::BufferAddress>,
    allocations: usize,
}

const FREE_DELAY_FRAMES: u64 = 2;

impl BufferArena {
    pub fn new(
        label: &'static str,
        usage: wgpu::BufferUsages,
        alignment: wgpu::BufferAddress,
        chunk_size: wgpu::BufferAddress,
    ) -> Self {
        Self {
            state: Default::default(),
            label,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            alignment: alignment.max(wgpu::COPY_BUFFER_ALIGNMENT),
            chunk_size,
        }
    }

    pub fn allocate(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        contents: &[u8],
    ) -> ArenaAllocation {
        let size = (contents.len() as wgpu::BufferAddress)
            .max(1)
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        let mut state = self.state.lock().expect("Arena lock shouldn't be poisoned");
        let (chunk, range) = self.find_space(&mut state, size).unwrap_or_else(|| {
            let chunk_size = self.chunk_size.max(size);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: chunk_size,
                usage: self.usage,
                mapped_at_creation: false,
            });
            let mut free = BTreeMap::new();
            if chunk_size > size {
                free.insert(size, chunk_size - size);
            }
            let chunk = Chunk {
                buffer,
                free,
                allocations: 0,
            };
            let index = match state.chunks.iter().position(Option::is_none) {
                Some(index) => {
                    state.chunks[index] = Some(chunk);
                    index
                }
                None => {
                    state.chunks.push(Some(chunk));
                    state.chunks.len() - 1
                }
            };
            (index, 0..size)
        });
        let entry = state.chunks[chunk]
            .as_mut()
            .expect("Allocated from a live chunk");
        entry.allocations += 1;
        let buffer = entry.buffer.clone();
        drop(state);

        let aligned = contents.len() as wgpu::BufferAddress & !(wgpu::COPY_BUFFER_ALIGNMENT - 1);
        if aligned > 0 {
            queue.write_buffer(&buffer, range.start, &contents[..aligned as usize]);
        }
        if aligned < contents.len() as wgpu::BufferAddress {
            // Queue writes must be a multiple of 4 bytes long.
            let mut tail = [0u8; wgpu::COPY_BUFFER_ALIGNMENT as usize];
            let rest = &contents[aligned as usize..];
            tail[..rest.len()].copy_from_slice(rest);
            queue.write_buffer(&buffer, range.start + aligned, &tail);
        }
        ArenaAllocation {
            buffer,
            range,
            chunk,
            arena: Arc::downgrade(&self.state),
        }
    }

    fn find_space(
        &self,
        state: &mut ArenaState,
        size: wgpu::BufferAddress,
    ) -> Option<(usize, Range<wgpu::BufferAddress>)> {
        for (index, chunk) in state.chunks.iter_mut().enumerate() {
            let Some(chunk) = chunk else {
                continue;
            };
            let found = chunk.free.iter().find_map(|(&start, &len)| {
                let aligned = start.next_multiple_of(self.alignment);
                (aligned + size <= start + len).then_some((start, len, aligned))
            });
            if let Some((start, len, aligned)) = found {
                chunk.free.remove(&start);
                if aligned > start {
                    chunk.free.insert(start, aligned - start);
                }
                let end = aligned + size;
                if end < start + len {
                    chunk.free.insert(end, start + len - end);
                }
                return Some((index, aligned..end));
            }
        }
        None
    }

    pub fn end_frame(&self) {
        let mut state = self.state.lock().expect("Arena lock shouldn't be poisoned");
        state.frame += 1;
        let frame = state.frame;
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut state.pending_frees)
            .into_iter()
            .partition(|(freed, _, _)| frame - freed >= FREE_DELAY_FRAMES);
        state.pending_frees = waiting;
        for (_, index, range) in ready {
            let Some(chunk) = state.chunks[index].as_mut() else {
                continue;
            };
            chunk.allocations -= 1;
            if chunk.allocations == 0 {
                state.chunks[index] = None;
                continue;
            }
            chunk.release(range);
        }
    }
}

impl Chunk {
    fn release(&mut self, range: Range<wgpu::BufferAddress>) {
        let mut start = range.start;
        let mut end = range.end;
        if let Some((&before, &len)) = self.free.range(..start).next_back()
            && before + len == start
        {
            self.free.remove(&before);
            start = before;
        }
        if let Some(len) = self.free.remove(&end) {
            end += len;
        }
        self.free.insert(start, end - start);
    }
}
