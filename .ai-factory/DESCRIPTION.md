# Project: Radioband

## Overview
Browser-based SPA for receiving FM/AM radio via RTL-SDR (RTL2832U + R820T) through the WebUSB API. Runs entirely in the browser — no server, no native driver, no extensions. Built with Rust compiled to WebAssembly, deployed to GitHub Pages.

## Core Features
- WebUSB connection to RTL-SDR dongles (vendor 0x0bda, products 0x2832/0x2838)
- WFM / NFM / AM demodulation with configurable gain
- Real-time spectrum display + scrolling waterfall (Canvas 2D)
- Audio playback via AudioWorklet (48 kHz output)
- Passband visualization with mode-dependent overlay
- Volume control via Web Audio GainNode
- Mock IQ mode for testing without hardware (synthetic FM carrier + 800 Hz tone)
- GitHub Pages deployment (static build output in `docs/`)

## Tech Stack
- **Language:** Rust (stable, edition 2021)
- **Compile target:** `wasm32-unknown-unknown` (WebAssembly)
- **Build tools:** Trunk (main app, ES modules), wasm-bindgen-cli (worker, `--target no-modules`), wasm-opt (size optimization)
- **Web APIs:** WebUSB, Web Audio (AudioWorklet), Web Workers, Canvas 2D, MessagePort
- **Key Rust crates:** wasm-bindgen, web-sys, js-sys, rustfft, num-complex, serde
- **Frontend:** Vanilla HTML/CSS/JS (no framework), dark theme, responsive layout
- **Deployment:** GitHub Pages from `docs/` directory
- **Browser requirement:** Chromium ≥ 89

## Architecture Notes
- **Workspace:** 5 Rust crates with clear separation of concerns
- **Threading model:** Main thread (UI + USB) ↔ Web Worker (DSP) ↔ AudioWorklet (audio output)
- **Data flow:** USB bulk transfer → main thread → Worker (WASM DSP) → main thread → Canvas + AudioWorklet
- **DSP pipeline:** IQ → FIR LPF + decimation → demodulation → de-emphasis → audio output
- **Two-stage build:** Worker WASM built separately (`cargo + wasm-bindgen`), then main app via Trunk
- **State management:** `Rc<RefCell<T>>` with split-cell pattern to avoid borrow conflicts across `.await`

## Architecture
See `.ai-factory/ARCHITECTURE.md` for detailed architecture guidelines.
Pattern: Modular Monolith (Rust Workspace Crates)

## Non-Functional Requirements
- **Performance:** Sustain 4.8 MB/s USB throughput (2.4 MSps × 2 bytes); zero-sleep read loop
- **Latency:** AudioWorklet ring buffer (0.5s / 24,000 samples) for low-latency audio
- **Binary size:** Release profile with `opt-level = "z"`, LTO, single codegen unit, stripped debuginfo
- **Error handling:** Structured error display in UI error bar; console_error_panic_hook for Rust panics
- **Security:** WebUSB requires HTTPS (or localhost); kernel modules must be blacklisted for device access
- **Logging:** `web_sys::console` logging in non-hot-path code
