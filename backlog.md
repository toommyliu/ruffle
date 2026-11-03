# Ruffle iOS / macOS (`FlashPlayerKit`) Backlog

## 1. Room & Character Asset Loading Optimization (Multi-Player Rooms & Class Switching)
- **Status**: Planned
- **Symptom**:
  - Entering a room with other players loads avatars, equipment, pets, and animations slowly.
  - Switching classes in Inventory ("fast-updating classes") takes too long to reload class animations/icons.

### Root-Cause Bottlenecks Identified
1. **`MovieLoader::movie_loader_data` Caps Initial Preloading to `10 KB / 1 ms`, Deferring `Event.COMPLETE` to the 12–24 FPS `run_frame()` Loop**
   - **Location**: `core/src/loader.rs:1771-1909`, `core/src/display_object/movie_clip.rs:630-633`
   - `SwfMovie::from_data` decompresses ZLIB/LZMA SWF data on the same `UpdateContext`, consuming `1–4 ms` (`uc.update_start.elapsed() >= 1 ms`).
   - Immediately after, `MovieLoader::preload_tick` runs with `ExecutionLimit::with_max_ops_and_time(10000, Duration::from_millis(1))`.
   - Because each tag byte counts as 1 op and `1 ms` is already exceeded, every SWF over `10 KB` aborts preloading immediately (`LoaderStatus::Parsing`) and waits for `Player::run_frame()` (which only runs at the SWF's 24 FPS rate, or 12–15 FPS in busy rooms).
   - Because AQW loads 6–8 SWFs per player sequentially via `world.loadQueue` / `AvatarMC` (waiting for `Event.COMPLETE` on each SWF before starting the next `Loader.load()`), deferring `Event.COMPLETE` across multiple 12–24 FPS frames adds **4–8 seconds of artificial frame-wait** across 30–40 character assets.

2. **`wait_for_streamed_response` Spawns an Extra `CADisplayLink` Frame Round-Trip Just to Read `EOF`**
   - **Location**: `core/src/loader.rs:532-570`, `frontend-utils/src/backends/navigator/fetch.rs:94-140`
   - `Response::next_chunk()` reads the full body until `Ok(None)` on the first call and returns `Ok(Some(buf))`, requiring `wait_for_streamed_response` to call `response.next_chunk().await` a second time just to observe `Ok(None)`.
   - That second call still goes through `spawn_tokio` and waits for the next `CADisplayLink` frame's `drain_tasks()` (`16–80 ms` per file).

3. **No In-Memory HTTP Asset Cache for Repeated Classes, Shared Player Gear, and Rooms**
   - **Location**: `frontend-utils/src/backends/navigator.rs:92-163`, `frontend-utils/src/backends/navigator/fetch.rs:46-85`
   - `reqwest::Client` does not cache HTTP responses. Switching classes in Inventory, loading shared player equipment/pets/auras in a room, or re-entering visited rooms re-downloads the same `.swf` files over HTTPS every time.

4. **Single-Pass `drain_tasks()` in `RufflePlayerHandle::tick`**
   - **Location**: `apple-sdk/src/lib.rs:454-473`
   - `drain_tasks()` only runs a single pass before `player.tick()`. Fast/cached futures that immediately enqueue a follow-up local task wait until the next display frame instead of completing within the same tick.

### Implementation Plan
- [ ] **1. Immediate SWF Preloading on Download Completion (`core/src/loader.rs`, `core/src/player.rs`)**
  - Reset `uc.update_start = Instant::now()` right after `SwfMovie::from_data` decompression in `MovieLoader::movie_loader_data`.
  - Increase initial `preload_tick` budget in `MovieLoader::movie_loader_data` from `(10_000, 1 ms)` to `(2_000_000, 10 ms)` so character gear, class, weapon, helm, cape, and pet SWFs (`50 KB–1.5 MB`) finish preloading and dispatch `Event.COMPLETE` in the same tick they finish downloading.
  - Increase per-frame `LoadBehavior::Streaming` preload budget in `Player::run_frame` from `(10_000, 1 ms)` to `(500_000, 8 ms)` so multi-megabyte room map SWFs finish remaining tags in 1 frame.
- [ ] **2. Zero-Frame EOF + Bounded In-Memory LRU Asset Cache (`frontend-utils/src/backends/navigator/fetch.rs`, `navigator.rs`)**
  - Track EOF state in `Response` (`drop` the inner `ReqwestResponse` when `chunk()` returns `Ok(None)`) so the subsequent `next_chunk()` returns `Ok(None)` synchronously without `spawn_tokio`.
  - Add a thread-safe bounded (96 MiB max total, 4 MiB max per entry) in-memory LRU cache for HTTP `200 OK` `GET` responses of static assets (`.swf`, `.png`, `.jpg`, `.jpeg`, `.mp3`).
  - Serve cache hits from memory without spawning Tokio tasks or waiting for future `CADisplayLink` frames.
  - Wire cache clearing into `ruffle_player_free_memory` ("Free Memory" button in Settings).
- [ ] **3. Multi-Pass Task Drain (`apple-sdk/src/lib.rs`)**
  - Update `RufflePlayerHandle::drain_tasks` to drain chained synchronous/cached futures across multiple passes (up to 8 passes) and run `drain_tasks()` both before and after `player.tick(dt_ms)`.
- [ ] **4. Rebuild & Verify**
  - Rebuild `RuffleApple.xcframework` (`--release`) for iOS device, iOS Simulator, and macOS via `apple-sdk/build-xcframework.sh`.
  - Verify room loading speed with multiple players and fast class switching in Inventory on device.

