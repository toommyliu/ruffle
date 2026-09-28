//! With the `stats` feature, calls the page's
//! `globalThis.__ruffleOnFrame(tickMs, renderMs, counters, renderer,
//! framesRun, wasmMemoryBytes, gpuMemory, heapBytes, gcMs)`, if any, after
//! every tick. Setting `globalThis.__ruffleCollectGarbage = true` runs a full
//! garbage collection after the next tick and resets it; `gcMs` is its
//! duration, or -1.
//! `renderMs` is -1 if nothing rendered. `counters` (from
//! `ruffle_render_wgpu::stats`) and `gpuMemory` are undefined without a wgpu
//! renderer. Without the feature, [`Timings`] does nothing.

use ruffle_core::Player;
#[cfg(feature = "stats")]
use wasm_bindgen::{JsCast, JsValue};

#[cfg(feature = "stats")]
thread_local! {
    static PERFORMANCE: Option<web_sys::Performance> =
        web_sys::window().and_then(|window| window.performance());
}

#[cfg(feature = "stats")]
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

    struct Counting;

    // SAFETY: forwards to `System`, only counting.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            }
            ptr
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) };
            LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new = unsafe { System.realloc(ptr, layout, new_size) };
            if !new.is_null() {
                LIVE_BYTES.fetch_add(new_size, Ordering::Relaxed);
                LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
            }
            new
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;
}

#[cfg(feature = "stats")]
fn now() -> f64 {
    PERFORMANCE.with(|performance| performance.as_ref().map_or(0.0, |p| p.now()))
}

pub struct Timings {
    #[cfg(feature = "stats")]
    mark: f64,
    #[cfg(feature = "stats")]
    tick_ms: f64,
    #[cfg(feature = "stats")]
    render_ms: Option<f64>,
}

// Without the feature the methods are empty.
#[cfg_attr(not(feature = "stats"), allow(clippy::needless_pass_by_ref_mut))]
impl Timings {
    #[inline]
    pub fn start() -> Self {
        Self {
            #[cfg(feature = "stats")]
            mark: now(),
            #[cfg(feature = "stats")]
            tick_ms: 0.0,
            #[cfg(feature = "stats")]
            render_ms: None,
        }
    }

    #[inline]
    pub fn ticked(&mut self) {
        #[cfg(feature = "stats")]
        {
            let now = now();
            self.tick_ms = now - self.mark;
            self.mark = now;
        }
    }

    #[inline]
    pub fn rendered(&mut self) {
        #[cfg(feature = "stats")]
        {
            self.render_ms = Some(now() - self.mark);
        }
    }

    #[inline]
    pub fn report(&self, player: &mut Player) {
        #[cfg(feature = "stats")]
        {
            let gc_ms = collect_garbage_if_asked(player);
            report(
                self.tick_ms,
                self.render_ms,
                player.renderer().name(),
                player.frames_run(),
                gc_ms,
            );
        }
        #[cfg(not(feature = "stats"))]
        let _ = player;
    }
}

#[cfg(feature = "stats")]
fn collect_garbage_if_asked(player: &mut Player) -> Option<f64> {
    let key = JsValue::from_str("__ruffleCollectGarbage");
    let global = js_sys::global();
    if js_sys::Reflect::get(&global, &key).ok()?.as_bool() != Some(true) {
        return None;
    }
    let _ = js_sys::Reflect::set(&global, &key, &JsValue::FALSE);
    let started = now();
    player.collect_garbage();
    Some(now() - started)
}

#[cfg(feature = "stats")]
fn report(
    tick_ms: f64,
    render_ms: Option<f64>,
    renderer: &str,
    frames_run: u64,
    gc_ms: Option<f64>,
) {
    let Ok(hook) = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__ruffleOnFrame"))
    else {
        return;
    };
    let Some(hook) = hook.dyn_ref::<js_sys::Function>() else {
        return;
    };
    let args = js_sys::Array::of5(
        &JsValue::from_f64(tick_ms),
        &JsValue::from_f64(render_ms.unwrap_or(-1.0)),
        &counters(),
        &JsValue::from_str(renderer),
        &JsValue::from_f64(frames_run as f64),
    );
    args.push(&wasm_memory_bytes());
    args.push(&gpu_memory());
    args.push(&JsValue::from_f64(
        heap::LIVE_BYTES.load(std::sync::atomic::Ordering::Relaxed) as f64,
    ));
    args.push(&JsValue::from_f64(gc_ms.unwrap_or(-1.0)));
    let _ = hook.apply(&JsValue::NULL, &args);
}

#[cfg(all(feature = "stats", any(feature = "webgpu", feature = "wgpu-webgl")))]
fn gpu_memory() -> JsValue {
    use ruffle_render_wgpu::stats;
    use std::sync::atomic::Ordering;
    let object = js_sys::Object::new();
    for (name, gauge) in [
        ("pools", &stats::POOL_BYTES),
        ("offscreen_pools", &stats::OFFSCREEN_POOL_BYTES),
        ("caches_and_bitmapdata", &stats::LIVE_EMPTY_TEXTURE_BYTES),
        ("bitmaps", &stats::LIVE_BITMAP_BYTES),
        ("meshes", &stats::MESH_BUFFER_BYTES),
    ] {
        let _ = js_sys::Reflect::set(
            &object,
            &JsValue::from_str(name),
            &JsValue::from_f64(gauge.load(Ordering::Relaxed) as f64),
        );
    }
    object.into()
}

#[cfg(all(
    feature = "stats",
    not(any(feature = "webgpu", feature = "wgpu-webgl"))
))]
fn gpu_memory() -> JsValue {
    JsValue::UNDEFINED
}

#[cfg(feature = "stats")]
fn wasm_memory_bytes() -> JsValue {
    js_sys::Reflect::get(&wasm_bindgen::memory(), &JsValue::from_str("buffer"))
        .and_then(|buffer| js_sys::Reflect::get(&buffer, &JsValue::from_str("byteLength")))
        .unwrap_or(JsValue::UNDEFINED)
}

#[cfg(all(feature = "stats", any(feature = "webgpu", feature = "wgpu-webgl")))]
fn counters() -> JsValue {
    let c = ruffle_render_wgpu::stats::snapshot();
    let blend_layers: u64 = c.blend_layers.iter().sum();
    let values = [
        c.render_passes,
        c.submits,
        c.cache_draws,
        c.offscreen_draws,
        c.texture_uploads,
        blend_layers,
        c.blend_layers_direct,
    ];
    let array = js_sys::Float64Array::new_with_length(values.len() as u32);
    for (i, value) in values.into_iter().enumerate() {
        array.set_index(i as u32, value as f64);
    }
    array.into()
}

#[cfg(all(
    feature = "stats",
    not(any(feature = "webgpu", feature = "wgpu-webgl"))
))]
fn counters() -> JsValue {
    JsValue::UNDEFINED
}
