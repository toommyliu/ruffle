//! CPU-side elapsed call times, including blocking, summed between samples.
//! Excludes time between calls and GPU execution.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::time::Duration;
use web_time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatsSample {
    /// Monotonic elapsed time since collection was enabled, taken at sampling.
    pub timestamp: Duration,
    pub tick_time: Duration,
    /// `None` if `Player::render` was not called since the previous sample.
    pub render_time: Option<Duration>,
    /// Frames since enable, including direct `Player::run_frame` calls.
    pub game_frames: u64,
    /// Render calls since enable, not screen presentations.
    pub renders: u64,
    /// Movie target FPS, not measured FPS.
    pub target_frame_rate: f64,
    pub memory: Option<MemoryStats>,
}

/// Host-supplied byte counts. Scopes may overlap; `None` means unavailable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    /// Resident bytes of the entire native host process.
    pub process_resident_bytes: Option<u64>,
    /// Live bytes tracked by the host's Rust allocator.
    pub rust_heap_bytes: Option<u64>,
}

pub(crate) struct Stats {
    started: Instant,
    baseline_game_frames: u64,
    pub tick_time: Duration,
    pub render_time: Option<Duration>,
    pub renders: u64,
    capacity: NonZeroUsize,
    pub samples: VecDeque<StatsSample>,
}

impl Stats {
    pub fn new(capacity: NonZeroUsize, game_frames: u64) -> Self {
        Self {
            started: Instant::now(),
            baseline_game_frames: game_frames,
            tick_time: Duration::ZERO,
            render_time: None,
            renders: 0,
            capacity,
            samples: VecDeque::with_capacity(capacity.get()),
        }
    }

    pub fn sample(
        &mut self,
        game_frames: u64,
        target_frame_rate: f64,
        memory: Option<MemoryStats>,
    ) -> StatsSample {
        let sample = StatsSample {
            timestamp: self.started.elapsed(),
            tick_time: std::mem::take(&mut self.tick_time),
            render_time: self.render_time.take(),
            game_frames: game_frames - self.baseline_game_frames,
            renders: self.renders,
            target_frame_rate,
            memory,
        };
        if self.samples.len() == self.capacity.get() {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        sample
    }
}
