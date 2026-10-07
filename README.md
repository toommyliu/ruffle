# ruffle

Experimental patches for [Ruffle](https://github.com/ruffle-rs/ruffle) that make AdventureQuest Worlds run noticeably smoother.

> [!NOTE]
> These patches are for Ruffle's web player, not the desktop app. They've only been tested on an Apple Silicon Mac, so they may run differently on other hardware.

Based on upstream `e2643c2d1`; `git log e2643c2d1..` lists the patches.

## Building

You need Rust (stable) with the WebAssembly target, `wasm-bindgen-cli` at the exact version Ruffle uses, Java (for the ActionScript compiler), and Node.js 24 with npm. Binaryen's `wasm-opt` is optional and makes the wasm smaller.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127
```

The web build, for a page or an Electron app:

```bash
cd web
npm install
npm run build --workspace=ruffle-core
npm run build --workspace=ruffle-selfhosted
```

It lands in `web/packages/selfhosted/dist`. The desktop player is `cargo build --release -p ruffle_desktop`.
