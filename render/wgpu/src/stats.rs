//! GPU work counters and memory gauges for benchmarks. Without the `stats`
//! feature they're no-ops, so callers need no `cfg`. Counters only go up:
//! diff two [`snapshot`]s.

#[cfg(feature = "stats")]
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

#[cfg(feature = "stats")]
pub type Counter = AtomicU64;
#[cfg(feature = "stats")]
pub type Gauge = AtomicI64;

#[cfg(not(feature = "stats"))]
pub struct Counter;
#[cfg(not(feature = "stats"))]
impl Counter {
    const fn new(_value: u64) -> Self {
        Self
    }
}

#[cfg(not(feature = "stats"))]
pub struct Gauge;
#[cfg(not(feature = "stats"))]
impl Gauge {
    const fn new(_value: i64) -> Self {
        Self
    }
}

/// Draws in submission order instead of batching, to check that batching
/// renders the same image.
#[cfg(feature = "stats")]
pub static SEQUENTIAL_DRAWS: AtomicBool = AtomicBool::new(false);

/// Keeps multisampled pixels from one render pass to the next rather than
/// resolving and reseeding them, which depends on where passes split, so that
/// `SEQUENTIAL_DRAWS` renders the same image as batching.
#[cfg(feature = "stats")]
pub static KEEP_SAMPLES: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "stats")]
#[inline]
pub fn sequential_draws() -> bool {
    SEQUENTIAL_DRAWS.load(Ordering::Relaxed)
}

#[cfg(feature = "stats")]
#[inline]
pub fn keep_samples() -> bool {
    KEEP_SAMPLES.load(Ordering::Relaxed)
}

#[cfg(not(feature = "stats"))]
#[inline]
pub fn keep_samples() -> bool {
    false
}

#[cfg(not(feature = "stats"))]
#[inline]
pub fn sequential_draws() -> bool {
    false
}

pub static RENDER_PASSES: Counter = Counter::new(0);
pub static SUBMITS: Counter = Counter::new(0);
pub static CACHE_DRAWS: Counter = Counter::new(0);
pub static OFFSCREEN_DRAWS: Counter = Counter::new(0);
pub static TEXTURE_UPLOADS: Counter = Counter::new(0);
/// Blend layers, indexed by `swf::BlendMode` discriminant; index 15 counts
/// Pixel Bender shader blends.
pub static BLEND_LAYERS: [Counter; 16] = [const { Counter::new(0) }; 16];
/// Blend layers drawn straight into their parent instead of a texture of
/// their own (included in `BLEND_LAYERS`).
pub static BLEND_LAYERS_DIRECT: Counter = Counter::new(0);
pub static EMPTY_TEXTURES_CREATED: Counter = Counter::new(0);
pub static POOL_TEXTURES_CREATED: Counter = Counter::new(0);
pub static MESHES_CREATED: Counter = Counter::new(0);

#[derive(Clone, Copy, Debug)]
pub enum PassKind {
    Draw,
    Blend,
    Clear,
    Copy,
    Filter,
    Shader,
    Stage3d,
}

#[cfg(feature = "stats")]
pub const PASS_KIND_NAMES: [&str; 7] = [
    "draw", "blend", "clear", "copy", "filter", "shader", "stage3d",
];

pub static RENDER_PASSES_BY_KIND: [Counter; 7] = [const { Counter::new(0) }; 7];

#[cfg(feature = "stats")]
pub const BLEND_LAYER_NAMES: [&str; 16] = [
    "normal",
    "?1",
    "layer",
    "multiply",
    "screen",
    "lighten",
    "darken",
    "difference",
    "add",
    "subtract",
    "invert",
    "alpha",
    "erase",
    "overlay",
    "hardlight",
    "shader",
];

pub static LIVE_BITMAP_TEXTURES: Gauge = Gauge::new(0);
pub static LIVE_BITMAP_BYTES: Gauge = Gauge::new(0);
pub static LIVE_EMPTY_TEXTURE_BYTES: Gauge = Gauge::new(0);
pub static POOL_BYTES: Gauge = Gauge::new(0);
pub static OFFSCREEN_POOL_BYTES: Gauge = Gauge::new(0);
pub static MESH_BUFFER_BYTES: Gauge = Gauge::new(0);
pub static LIVE_GRADIENT_TEXTURES: Gauge = Gauge::new(0);

#[inline]
pub fn count(counter: &Counter) {
    #[cfg(feature = "stats")]
    counter.fetch_add(1, Ordering::Relaxed);
    #[cfg(not(feature = "stats"))]
    let _ = counter;
}

#[inline]
pub fn count_pass(kind: PassKind) {
    count(&RENDER_PASSES);
    count(&RENDER_PASSES_BY_KIND[kind as usize]);
}

#[inline]
pub fn add(gauge: &Gauge, amount: i64) {
    #[cfg(feature = "stats")]
    gauge.fetch_add(amount, Ordering::Relaxed);
    #[cfg(not(feature = "stats"))]
    let _ = (gauge, amount);
}

#[inline]
pub fn set_with(gauge: &Gauge, value: impl FnOnce() -> i64) {
    #[cfg(feature = "stats")]
    gauge.store(value(), Ordering::Relaxed);
    #[cfg(not(feature = "stats"))]
    let _ = (gauge, value);
}

#[derive(Debug)]
pub struct Live {
    #[cfg(feature = "stats")]
    count: &'static Gauge,
    #[cfg(feature = "stats")]
    bytes: Option<(&'static Gauge, i64)>,
}

impl Live {
    pub fn new(gauge: &'static Gauge) -> Self {
        add(gauge, 1);
        Self {
            #[cfg(feature = "stats")]
            count: gauge,
            #[cfg(feature = "stats")]
            bytes: None,
        }
    }

    pub fn with_bytes(gauge: &'static Gauge, bytes_gauge: &'static Gauge, bytes: u64) -> Self {
        let bytes = bytes as i64;
        add(gauge, 1);
        add(bytes_gauge, bytes);
        Self {
            #[cfg(feature = "stats")]
            count: gauge,
            #[cfg(feature = "stats")]
            bytes: Some((bytes_gauge, bytes)),
        }
    }
}

#[cfg(feature = "stats")]
impl Drop for Live {
    fn drop(&mut self) {
        add(self.count, -1);
        if let Some((gauge, bytes)) = self.bytes {
            add(gauge, -bytes);
        }
    }
}

#[cfg(feature = "stats")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    // Keep in sync with `snapshot` and `Sub`.
    pub render_passes: u64,
    pub render_passes_by_kind: [u64; 7],
    pub submits: u64,
    pub cache_draws: u64,
    pub offscreen_draws: u64,
    pub texture_uploads: u64,
    pub blend_layers: [u64; 16],
    pub blend_layers_direct: u64,
    pub empty_textures_created: u64,
    pub pool_textures_created: u64,
    pub meshes_created: u64,
}

#[cfg(feature = "stats")]
pub fn snapshot() -> Counters {
    Counters {
        render_passes: RENDER_PASSES.load(Ordering::Relaxed),
        render_passes_by_kind: std::array::from_fn(|i| {
            RENDER_PASSES_BY_KIND[i].load(Ordering::Relaxed)
        }),
        submits: SUBMITS.load(Ordering::Relaxed),
        cache_draws: CACHE_DRAWS.load(Ordering::Relaxed),
        offscreen_draws: OFFSCREEN_DRAWS.load(Ordering::Relaxed),
        texture_uploads: TEXTURE_UPLOADS.load(Ordering::Relaxed),
        blend_layers: std::array::from_fn(|i| BLEND_LAYERS[i].load(Ordering::Relaxed)),
        blend_layers_direct: BLEND_LAYERS_DIRECT.load(Ordering::Relaxed),
        empty_textures_created: EMPTY_TEXTURES_CREATED.load(Ordering::Relaxed),
        pool_textures_created: POOL_TEXTURES_CREATED.load(Ordering::Relaxed),
        meshes_created: MESHES_CREATED.load(Ordering::Relaxed),
    }
}

#[cfg(feature = "stats")]
impl std::ops::Sub for Counters {
    type Output = Counters;

    fn sub(self, rhs: Counters) -> Counters {
        Counters {
            render_passes: self.render_passes - rhs.render_passes,
            render_passes_by_kind: std::array::from_fn(|i| {
                self.render_passes_by_kind[i] - rhs.render_passes_by_kind[i]
            }),
            submits: self.submits - rhs.submits,
            cache_draws: self.cache_draws - rhs.cache_draws,
            offscreen_draws: self.offscreen_draws - rhs.offscreen_draws,
            texture_uploads: self.texture_uploads - rhs.texture_uploads,
            blend_layers: std::array::from_fn(|i| self.blend_layers[i] - rhs.blend_layers[i]),
            blend_layers_direct: self.blend_layers_direct - rhs.blend_layers_direct,
            empty_textures_created: self.empty_textures_created - rhs.empty_textures_created,
            pool_textures_created: self.pool_textures_created - rhs.pool_textures_created,
            meshes_created: self.meshes_created - rhs.meshes_created,
        }
    }
}
