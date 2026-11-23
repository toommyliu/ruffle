# ruffle

Experimental patches for [Ruffle](https://github.com/ruffle-rs/ruffle) that make AdventureQuest Worlds run noticeably smoother on Apple Silicon Macs.

It is intentionally time-frozen at upstream [`e2643c2d1`](https://github.com/ruffle-rs/ruffle/commit/e2643c2d1) from 2026-09-26. There is no good reason to pull in every upstream release.

## Building

You need Rust (stable) and Java, for the ActionScript compiler.

```bash
cargo build --release -p ruffle_desktop
```
