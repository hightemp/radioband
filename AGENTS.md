# AGENTS.md

> Project map for AI agents. Keep this file up-to-date as the project evolves.

## Project Overview
Browser-based RTL-SDR radio receiver (FM/AM) built with Rust + WebAssembly. Runs entirely in the browser via WebUSB — no server, no native drivers. Deployed to GitHub Pages.

## Tech Stack
- **Language:** Rust (edition 2021)
- **Target:** wasm32-unknown-unknown (WebAssembly)
- **Build:** Trunk (main app) + wasm-bindgen-cli (worker WASM)
- **Web APIs:** WebUSB, Web Audio (AudioWorklet), Web Workers, Canvas 2D
- **Key crates:** wasm-bindgen, web-sys, js-sys, rustfft, num-complex
- **Frontend:** Vanilla HTML/CSS/JS (no framework)
- **Deployment:** GitHub Pages (docs/)

## Project Structure
```
radioband/
├── Cargo.toml              # Workspace root (5 members)
├── Trunk.toml              # Trunk build config (output → docs/)
├── build.sh                # Full build script (worker + main app)
├── index.html              # Trunk entry point (loads app-ui WASM)
├── crates/
│   ├── sdr-core/           # Pure DSP library (FFT, FIR, demod, waterfall)
│   │   └── src/lib.rs      # 594 LoC — DemodMode, DspPipeline, MockIqSource
│   ├── usb-rtl/            # WebUSB driver for RTL2832U + R820T tuner
│   │   └── src/lib.rs      # 581 LoC — RtlSdr, USB protocol, PLL tuning
│   ├── audio-bridge/       # Web Audio output via AudioWorklet
│   │   └── src/lib.rs      # 108 LoC — AudioBridge, GainNode volume
│   ├── sdr-worker/         # Web Worker entry point (WASM DSP bridge)
│   │   └── src/lib.rs      # 128 LoC — process_iq(), configure(), set_mode()
│   └── app-ui/             # Main thread UI, controls, canvas rendering
│       └── src/
│           ├── lib.rs       # 832 LoC — AppState, SdrCell, event handlers, read_loop
│           └── waterfall.rs # 286 LoC — WaterfallRenderer, spectrum + passband overlay
├── static/
│   ├── style.css            # Dark theme CSS (138 LoC)
│   ├── worker.js            # DSP Web Worker bootstrap (67 LoC)
│   ├── audio-worklet-processor.js  # AudioWorklet ring buffer (55 LoC)
│   └── worker-pkg/         # Generated WASM bindings for worker (gitignored)
├── docs/                    # Built output for GitHub Pages deployment
├── screenshots/             # App screenshots
├── NOTES.md                 # Development notes, bug history, protocol values
└── README.md                # Project README with feature table & architecture
```

## Key Entry Points
| File | Purpose |
|------|---------|
| `crates/app-ui/src/lib.rs` | WASM entry point (`#[wasm_bindgen(start)]` main), UI initialization |
| `crates/sdr-worker/src/lib.rs` | Worker WASM entry: `init_worker()`, `process_iq()`, `configure()` |
| `static/worker.js` | JS bootstrap that loads worker WASM and routes messages |
| `static/audio-worklet-processor.js` | AudioWorklet with ring buffer for PCM playback |
| `index.html` | Trunk HTML entry point |
| `build.sh` | Full build: worker WASM → bindings → Trunk build |
| `Trunk.toml` | Build config: pre-build hook for worker, output to docs/ |

## Data Flow
```
RTL-SDR dongle (USB)
    │ WebUSB bulk transfer (128KB blocks)
    ▼
Main Thread (app-ui WASM)
    │ postMessage (IQ bytes)
    ▼
Web Worker (sdr-worker WASM)
    │ DSP: FIR LPF → decimate → demod → de-emphasis
    │ FFT → spectrum → waterfall buffer
    ▼
Main Thread
    ├─→ Canvas 2D: spectrum line + waterfall + passband overlay
    └─→ AudioWorklet: PCM via MessagePort → ring buffer → speakers
```

## Documentation
| Document | Path | Description |
|----------|------|-------------|
| README | README.md | Project overview, features, build instructions |
| Dev Notes | NOTES.md | Bug history, protocol values, DSP parameters, references |

## AI Context Files
| File | Purpose |
|------|---------|
| AGENTS.md | This file — project structure map |
| .ai-factory/DESCRIPTION.md | Project specification and tech stack |
| .ai-factory/ARCHITECTURE.md | Architecture decisions and guidelines |
| .ai-factory/PLAN.md | Current feature plan (passband + volume) |
| .github/skills/radioband-rtlsdr/SKILL.md | Domain skill: RTL-SDR protocol, DSP rules, pitfalls |
