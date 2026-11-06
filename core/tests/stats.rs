#![cfg(all(feature = "stats", not(target_family = "wasm")))]

use ruffle_core::stats::{MemoryStats, StatsSample};
use ruffle_core::{FloatDuration, PlayerBuilder};
use std::num::NonZeroUsize;
use std::time::Duration;

#[test]
fn samples_native_loop_and_transfers_owned_data() {
    let player = PlayerBuilder::new()
        .with_autoplay(true)
        .with_frame_rate(Some(24.0))
        .build();
    let mut player = player.lock().unwrap();

    player.tick(FloatDuration::from_millis(42.0));
    player.render();
    assert!(!player.stats_enabled());
    assert_eq!(player.sample_stats(None), None);
    assert_eq!(player.drain_stats().count(), 0);

    player.enable_stats(NonZeroUsize::new(4).unwrap());
    player.set_frame_interpolation(true);
    player.tick(FloatDuration::from_millis(42.0));
    player.render();
    let first = player.sample_stats(None).unwrap();
    assert_eq!(first.game_frames, 1);
    assert_eq!(first.renders, 1);
    assert_eq!(first.target_frame_rate, 24.0);
    assert!(first.render_time.is_some());
    assert_eq!(first.memory, None);

    player.tick(FloatDuration::from_millis(1.0));
    assert!(player.needs_render());
    player.render();
    player.render();
    let memory = MemoryStats {
        process_resident_bytes: Some(123_456),
        ..Default::default()
    };
    let second = player.sample_stats(Some(memory)).unwrap();
    assert_eq!(second.game_frames, 1);
    assert_eq!(second.renders, 3);
    assert!(second.timestamp >= first.timestamp);
    assert!(second.render_time.is_some());
    assert_eq!(second.memory.unwrap().process_resident_bytes, Some(123_456));
    assert_eq!(second.memory.unwrap().rust_heap_bytes, None);

    player.tick(FloatDuration::from_millis(1.0));
    let skipped = player.sample_stats(None).unwrap();
    assert_eq!(skipped.game_frames, 1);
    assert_eq!(skipped.renders, 3);
    assert_eq!(skipped.render_time, None);

    let idle = player.sample_stats(None).unwrap();
    assert_eq!(idle.tick_time, Duration::ZERO);
    assert_eq!(idle.render_time, None);
    let (sender, receiver) = std::sync::mpsc::channel::<StatsSample>();
    for sample in player.drain_stats() {
        sender.send(sample).unwrap();
    }
    assert_eq!(player.drain_stats().count(), 0);
    drop(player);
    drop(sender);
    let samples = std::thread::spawn(move || receiver.into_iter().collect::<Vec<_>>())
        .join()
        .unwrap();
    assert_eq!(samples, [first, second, skipped, idle]);
}

#[test]
fn bounds_history_and_resets_collection_sessions() {
    let player = PlayerBuilder::new()
        .with_autoplay(true)
        .with_frame_rate(Some(24.0))
        .build();
    let mut player = player.lock().unwrap();
    player.run_frame();
    player.enable_stats(NonZeroUsize::new(2).unwrap());

    for _ in 0..3 {
        player.run_frame();
        player.render();
        player.sample_stats(None);
    }
    let samples: Vec<_> = player.drain_stats().collect();
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0].game_frames, 2);
    assert_eq!(samples[0].renders, 2);
    assert_eq!(samples[1].game_frames, 3);
    assert_eq!(samples[1].renders, 3);

    player.tick(FloatDuration::from_millis(42.0));
    player.render();
    player.sample_stats(None);
    player.tick(FloatDuration::from_millis(42.0));
    player.render();
    player.enable_stats(NonZeroUsize::new(1).unwrap());
    assert_eq!(player.drain_stats().count(), 0);
    let reset = player.sample_stats(None).unwrap();
    assert_eq!(reset.game_frames, 0);
    assert_eq!(reset.renders, 0);
    assert_eq!(reset.tick_time, Duration::ZERO);
    assert_eq!(reset.render_time, None);

    player.set_is_playing(false);
    player.tick(FloatDuration::from_millis(42.0));
    let paused = player.sample_stats(None).unwrap();
    assert_eq!(paused.game_frames, 0);
    assert_eq!(paused.renders, 0);
    assert_eq!(paused.render_time, None);

    player.disable_stats();
    assert!(!player.stats_enabled());
    assert_eq!(player.drain_stats().count(), 0);
    player.tick(FloatDuration::from_millis(42.0));
    player.render();
    assert_eq!(player.sample_stats(None), None);
    player.enable_stats(NonZeroUsize::new(1).unwrap());
    let reset = player.sample_stats(None).unwrap();
    assert_eq!(reset.game_frames, 0);
    assert_eq!(reset.renders, 0);
}
