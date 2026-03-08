# Architecture: Modular Monolith (Workspace Crates)

## Overview
Radioband uses a **Modular Monolith** architecture implemented via Rust's workspace crate system. Each crate is an isolated module with a clear public API, compiled to a single WebAssembly binary (or two: main thread + worker). This gives strong compile-time boundary enforcement while keeping deployment simple — just static files on GitHub Pages.

The architecture is driven by the browser's threading model: the main thread handles UI + USB I/O, a Web Worker runs the DSP pipeline, and an AudioWorklet outputs audio. Each thread boundary maps to a separate WASM binary with a thin JS bridge.

## Decision Rationale
- **Project type:** Single-page browser application, no server backend
- **Tech stack:** Rust → wasm32-unknown-unknown, 5 workspace crates
- **Key factor:** Rust workspace provides zero-cost module boundaries with compile-time dependency checks. The project is small enough (2,600 LoC) to remain a single workspace, but the crate split cleanly maps to the browser's thread architecture (main thread vs worker vs worklet).

## Folder Structure
```
radioband/
├── Cargo.toml                 # Workspace root — defines all members
├── crates/
│   ├── sdr-core/              # CORE: Pure DSP (no web dependencies)
│   │   └── src/lib.rs         # DemodMode, DspPipeline, FIR, FFT, MockIqSource
│   ├── usb-rtl/               # DRIVER: WebUSB hardware abstraction
│   │   └── src/lib.rs         # RtlSdr, USB protocol, R820T tuner
│   ├── audio-bridge/          # DRIVER: Web Audio output abstraction
│   │   └── src/lib.rs         # AudioBridge, AudioWorklet, GainNode
│   ├── sdr-worker/            # BOUNDARY: Worker-thread WASM entry point
│   │   └── src/lib.rs         # wasm_bindgen FFI → sdr-core pipeline
│   └── app-ui/                # BOUNDARY: Main-thread WASM entry point
│       └── src/
│           ├── lib.rs          # AppState, event handlers, read_loop
│           └── waterfall.rs    # Canvas renderer, spectrum, passband
├── static/                    # JS bridge files (non-Rust)
│   ├── worker.js              # Worker bootstrap + message routing
│   ├── audio-worklet-processor.js  # Ring buffer AudioWorklet
│   └── style.css              # Dark theme UI
├── index.html                 # Trunk entry point
├── build.sh                   # Full build script
└── docs/                      # Build output (GitHub Pages)
```

## Dependency Rules

Crate dependency graph (enforced by `Cargo.toml`):

```
    app-ui
   /  |  \
  /   |   \
usb-rtl  audio-bridge  sdr-core
                          ↑
                     sdr-worker
```

### Allowed
- ✅ `app-ui` → `sdr-core`, `usb-rtl`, `audio-bridge` (main thread orchestrator)
- ✅ `sdr-worker` → `sdr-core` (worker only needs DSP)
- ✅ `sdr-core` → external crates only (`rustfft`, `num-complex`) — no web-sys

### Forbidden
- ❌ `sdr-core` → any `web-sys` / `wasm-bindgen` / `js-sys` (must remain pure Rust, testable natively)
- ❌ `sdr-worker` → `usb-rtl` or `audio-bridge` (worker has no USB/audio access)
- ❌ `usb-rtl` → `audio-bridge` or vice versa (independent drivers)
- ❌ `usb-rtl` → `sdr-core` (USB driver has no DSP concern)
- ❌ Any crate → `app-ui` (app-ui is the composition root, nothing depends on it)

## Layer/Module Communication

### Main Thread ↔ Worker (postMessage)
- **Protocol:** JSON-like `MessageEvent` via `postMessage` / `onmessage`
- **Messages IN (to worker):** `configure`, `iq_data`, `mock`, `set_mode`, `clear_waterfall`
- **Messages OUT (from worker):** JS object with `spectrum` (Float32Array), `audio` (Float32Array), `waterfall` (Uint8ClampedArray), `wfWidth`, `wfHeight`
- **Bridge:** `static/worker.js` routes messages to `#[wasm_bindgen]` functions in `sdr-worker`

### Main Thread → AudioWorklet (MessagePort)
- **Protocol:** `Float32Array` PCM chunks via `MessagePort.postMessage()`
- **Direction:** One-way (main → worklet)
- **Bridge:** `audio-bridge` crate wraps the `MessagePort` API

### Within Main Thread (Rust)
- **State sharing:** `Rc<RefCell<T>>` — split into `AppState` (UI state) and `SdrCell` (hardware handle) to avoid borrow conflicts across `.await`
- **Async:** `wasm_bindgen_futures::spawn_local` for async operations from sync event handlers
- **Event handlers:** `Closure::wrap` + `.forget()` to prevent GC of JS closures

## Key Principles

1. **sdr-core stays pure.** No web platform dependencies. It must compile and test on native targets (`cargo test` without wasm). All FFT, FIR, demodulation, and waterfall logic lives here.

2. **Thread boundaries = crate boundaries.** `app-ui` = main thread, `sdr-worker` = worker thread. Each compiles to its own `.wasm`. Communication is via `postMessage` only.

3. **Drivers are independent.** `usb-rtl` and `audio-bridge` know nothing about each other. `app-ui` is the composition root that wires them together.

4. **No shared mutable state across threads.** All inter-thread data passes by value through `postMessage`. Within a thread, use `Rc<RefCell<T>>` with the take-and-put-back pattern for async safety.

5. **Build is two-stage.** Worker WASM is built first (`cargo build + wasm-bindgen --target no-modules`), then main app via Trunk (ES modules). This is because Trunk doesn't support multiple WASM outputs with different bindgen targets.

## Code Examples

### Adding a new DSP function (sdr-core)
```rust
// crates/sdr-core/src/lib.rs
// Pure Rust — no web-sys imports allowed here

/// Compute RMS power of a signal
pub fn rms_power(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_power() {
        let signal = vec![1.0, -1.0, 1.0, -1.0];
        assert!((rms_power(&signal) - 1.0).abs() < 1e-6);
    }
}
```

### Exposing DSP to Worker (sdr-worker)
```rust
// crates/sdr-worker/src/lib.rs
use wasm_bindgen::prelude::*;
use sdr_core::rms_power;

#[wasm_bindgen]
pub fn get_signal_rms(data: &[f32]) -> f32 {
    rms_power(data)
}
```

### Correct RefCell pattern for async (app-ui)
```rust
// crates/app-ui/src/lib.rs
// Take SDR out of cell, do async work, put it back
let sdr = sdr_cell.borrow_mut().take();
if let Some(mut sdr) = sdr {
    // Async USB operation — no borrow held across .await
    let result = sdr.read_samples().await;
    sdr_cell.borrow_mut().replace(sdr);
    // Process result...
}
```

## Anti-Patterns

- ❌ **Adding `web-sys` to `sdr-core`** — breaks native testability, couples DSP to browser
- ❌ **Holding `RefCell` borrow across `.await`** — causes "already borrowed" panics at runtime
- ❌ **Direct DOM manipulation in `sdr-worker`** — workers have no DOM access, will panic
- ❌ **Sleeping in the read loop** — even 16ms sleep at 2.4 MSps causes 80% data loss
- ❌ **Shared memory between threads** — WASM doesn't support SharedArrayBuffer without COOP/COEP headers; use `postMessage` copying
- ❌ **Importing `app-ui` from other crates** — it's the composition root, dependency arrows only point into it
