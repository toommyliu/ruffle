use ruffle_core::Player;
use ruffle_render_wgpu::stats;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use winit::dpi::LogicalPosition;
use winit::window::{Window, WindowLevel};

struct Recorder {
    last: Instant,
    last_present: Option<Instant>,
    game_frames: u64,
    renders: u64,
    intervals: Vec<f64>,
    counters: stats::Counters,
}

thread_local! {
    static RECORDER: RefCell<Option<Recorder>> = RefCell::new(
        (std::env::var("RUFFLE_DESKTOP_STATS").as_deref() == Ok("1")).then(|| Recorder {
            last: Instant::now(),
            last_present: None,
            game_frames: 0,
            renders: 0,
            intervals: Vec::new(),
            counters: stats::snapshot(),
        })
    );
}

pub fn configure_window(window: &Window) {
    if let Ok(name) = std::env::var("RUFFLE_DESKTOP_MONITOR") {
        let monitors: Vec<_> = window.available_monitors().collect();
        let Some(monitor) = monitors
            .iter()
            .find(|monitor| monitor.name().as_deref() == Some(&name))
        else {
            eprintln!("[desktop-stats] Monitor {name:?} missing: {monitors:?}");
            std::process::exit(2);
        };
        let position = monitor.position().to_logical::<f64>(monitor.scale_factor());
        window.set_outer_position(LogicalPosition::new(position.x + 40.0, position.y + 60.0));
        eprintln!("[desktop-monitor] {monitor:?}");
    }
    if std::env::var("RUFFLE_DESKTOP_TOPMOST").as_deref() == Ok("1") {
        window.set_window_level(WindowLevel::AlwaysOnTop);
    }
}

pub fn render() {
    RECORDER.with_borrow_mut(|recorder| {
        if let Some(recorder) = recorder {
            recorder.renders += 1;
        }
    });
}

pub fn present() {
    RECORDER.with_borrow_mut(|recorder| {
        if let Some(recorder) = recorder {
            let now = Instant::now();
            if let Some(last) = recorder.last_present.replace(now) {
                recorder
                    .intervals
                    .push(now.duration_since(last).as_secs_f64() * 1000.0);
            }
        }
    });
}

pub fn sample(player: Option<&Player>, window: &Window, hidden: bool) {
    RECORDER.with_borrow_mut(|recorder| {
        let Some(recorder) = recorder else { return };
        let now = Instant::now();
        let elapsed = now.duration_since(recorder.last);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let frames = player.map_or(0, Player::frames_run);
        let counters = stats::snapshot();
        let size = window.inner_size();
        let texture_bytes = stats::LIVE_BITMAP_BYTES.load(Ordering::Relaxed)
            + stats::LIVE_EMPTY_TEXTURE_BYTES.load(Ordering::Relaxed)
            + stats::POOL_BYTES.load(Ordering::Relaxed)
            + stats::OFFSCREEN_POOL_BYTES.load(Ordering::Relaxed);
        eprintln!(
            "[desktop-stats] {{\"seconds\":{:.6},\"game_frames\":{},\"renders\":{},\"passes\":{},\"width\":{},\"height\":{},\"scale\":{},\"hidden\":{},\"texture_bytes\":{},\"intervals\":{:?}}}",
            elapsed.as_secs_f64(),
            frames.saturating_sub(recorder.game_frames),
            recorder.renders,
            counters.render_passes - recorder.counters.render_passes,
            size.width,
            size.height,
            window.scale_factor(),
            hidden,
            texture_bytes,
            recorder.intervals,
        );
        recorder.last = now;
        recorder.game_frames = frames;
        recorder.renders = 0;
        recorder.intervals.clear();
        recorder.counters = counters;
    });
}
