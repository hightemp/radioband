# Feature: Passband Visualization + Volume Control

**Mode:** Fast  
**Created:** 2026-03-08  
**Testing:** Yes  
**Logging:** Verbose  
**Docs:** No  

## Description

Add two features to the Radioband UI:

1. **Passband visualization** — vertical lines and semi-transparent shading on the spectrum display showing the demodulation filter boundaries (center line + left/right edges) that update based on the current mode:
   - WFM: ±120 kHz (240 kHz passband)
   - NFM: ±8 kHz (16 kHz passband)
   - AM: ±5 kHz (10 kHz passband)

2. **Volume slider** — a Web Audio GainNode inserted into the audio chain with a UI range slider.

## Tasks

### Phase 1: Passband Visualization on Spectrum

#### Task 1: Add mode/bandwidth fields to WaterfallRenderer
**File:** `crates/app-ui/src/waterfall.rs`  
**Deliverable:** Add a `passband_hz: f64` field to `WaterfallRenderer` (default `240_000.0` for WFM). Add a `set_mode_bandwidth(&mut self, passband_hz: f64)` method that updates this field.  
**Logging:** `console::log_1("WaterfallRenderer: passband set to {} Hz")`

#### Task 2: Draw passband overlay on spectrum area
**File:** `crates/app-ui/src/waterfall.rs`  
**Deliverable:** Add a `draw_passband_overlay(&self, w: f64, sh: f64)` method called from `render()` after the spectrum line and frequency labels are drawn. It should:
1. Calculate left/right frequency boundaries: `center_freq ± passband_hz / 2`.
2. Convert to pixel X positions: `x = ((freq - f_left) / sample_rate) * w`.
3. Draw a **semi-transparent filled rectangle** (e.g. `rgba(88, 166, 255, 0.12)`) spanning the passband area from `y=0` to `y=sh`.
4. Draw **dashed vertical lines** at left and right boundaries (e.g. `rgba(88, 166, 255, 0.6)`, lineWidth 1.0, dash pattern `[4, 3]`).
5. Draw a **solid vertical line** at the center frequency (e.g. `rgba(255, 100, 100, 0.8)`, lineWidth 1.5) — replaces/enhances the existing center triangle marker.
6. Draw a small label above the passband showing the mode bandwidth (e.g. "WFM ±120k" or "AM ±5k").  
**Logging:** None (render-hot path, no logs).

#### Task 3: Wire mode changes to update renderer passband
**File:** `crates/app-ui/src/lib.rs`  
**Deliverable:** In the mode selector (`select-mode`) event handler:
1. After setting `state.mode`, compute `passband_hz` based on the mode string:
   - `"wfm"` → `240_000.0`
   - `"nfm"` → `16_000.0`
   - `"am"` → `10_000.0`
2. Call `renderer.set_mode_bandwidth(passband_hz)` on the renderer.
3. Also call `set_mode_bandwidth` during `connect_device()` and `start_mock_mode()` initialization so the passband is correct from the start.  
**Logging:** `console::log_1("Radioband: passband updated for mode {}: {} Hz")`

### Phase 2: Volume Control

#### Task 4: Add GainNode to AudioBridge
**File:** `crates/audio-bridge/src/lib.rs`  
**Deliverable:**
1. Add a `gain_node: Option<GainNode>` field to `AudioBridge`.
2. In `init()`, create a `GainNode` from `AudioContext`, set initial gain to `1.0`.
3. Re-wire audio chain: `AudioWorkletNode → GainNode → ctx.destination()` (instead of worklet direct to destination).
4. Add a `pub fn set_volume(&self, value: f32)` method that sets `gain_node.gain().set_value(value)`. Clamp to `0.0..=2.0`.
5. Add a `pub fn volume(&self) -> f32` getter.  
**Logging:** `console::log_1("AudioBridge: volume set to {}")`

#### Task 5: Add volume slider to UI HTML
**File:** `crates/app-ui/src/lib.rs` (inside `build_ui`)  
**Deliverable:** Add a new `control-group` div after the Mode selector containing:
```html
<label>Volume
    <input id="input-volume" type="range" min="0" max="200" value="100" step="1" />
    <span id="volume-value">100%</span>
</label>
```
**File:** `static/style.css`  
**Deliverable:** Add `#volume-value` styling identical to `#gain-value`.

#### Task 6: Wire volume slider event handler
**File:** `crates/app-ui/src/lib.rs`  
**Deliverable:** Add an `input` event listener for `#input-volume` that:
1. Reads the slider value (0–200) and converts to float (0.0–2.0).
2. Updates `#volume-value` label (e.g. "100%", "150%", "0%").
3. Calls `audio.set_volume(value)` on the AudioBridge.
4. If audio is not yet initialized, store the volume in `AppState` and apply it when AudioBridge is created.  
**Deliverable (AppState):** Add a `volume: f32` field (default `1.0`) to `AppState`.  
**Logging:** `console::log_1("Radioband: volume set to {}%")`

### Phase 3: Tests

#### Task 7: Add tests for new functionality
**File:** `crates/sdr-core/src/lib.rs` — already has test infrastructure  
**Deliverable:** Add test verifying `mode_to_passband()` helper returns correct values:
- `"wfm"` → `240_000.0`
- `"nfm"` → `16_000.0`
- `"am"` → `10_000.0`

**File:** `crates/audio-bridge/src/lib.rs`  
**Deliverable:** Since AudioBridge requires browser APIs (AudioContext), add doc-tests or conditional `cfg(test)` comments explaining how to test manually.

### Phase 4: Build & Verify

#### Task 8: Build and verify
**Deliverable:** Run `bash build.sh`, verify no compile errors. Check that:
- Spectrum display shows semi-transparent passband overlay with boundary lines.
- Switching mode (WFM/NFM/AM) changes the passband width visually.
- Volume slider changes audio output level smoothly.
- Volume 0% = silence, 100% = normal, 200% = amplified.

## Commit Plan

**Commit 1 (after Tasks 1–3):** `feat: passband visualization overlay on spectrum display`  
**Commit 2 (after Tasks 4–6):** `feat: volume control slider with Web Audio GainNode`  
**Commit 3 (after Tasks 7–8):** `test: add passband and volume tests + build verify`
