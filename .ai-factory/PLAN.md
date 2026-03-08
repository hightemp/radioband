# Fix: Frequency Change Not Applied to Hardware

**Mode:** Fast  
**Created:** 2026-03-08  
**Testing:** Yes  
**Logging:** Verbose  
**Docs:** No  

## Problem

When setting frequency via the UI dial, the display label updates but the RTL-SDR hardware remains tuned to the previous frequency. The waterfall/spectrum continue showing the old frequency's signal. Occasionally (~10% of the time) it works.

## Root Cause

The frequency change handler in `app-ui` races with `read_loop` for `SdrCell` ownership using a take-and-put-back pattern. `read_loop` holds the SDR ~90% of the time (during USB bulk transfers), leaving a ~1-4ms window for the frequency handler. Even when the handler acquires the SDR, errors from `set_center_freq` are silently discarded via `let _`.

Additionally, the gain slider suffers the same race condition pattern.

## Solution

Replace the competitive take-retry pattern with a **cooperative pending-change mechanism**: the `read_loop` itself applies pending frequency/gain changes from shared state, since it already owns the SDR. This completely eliminates the race.

## Tasks

### Phase 1: Core Fix — Cooperative Frequency/Gain Changes

#### Task 1: Add pending-change fields to AppState
**File:** `crates/app-ui/src/lib.rs`  
**Deliverable:** Add `pending_freq: Option<u32>` and `pending_gain: Option<i32>` fields to `AppState`.  
**Logging:** Log when a pending change is queued: `"Radioband: queued frequency change to {} Hz"`, same for gain.

#### Task 2: Rewrite frequency button handler to set pending flag
**File:** `crates/app-ui/src/lib.rs`  
**Deliverable:** Replace the retry-loop frequency handler with one that:
1. Parses frequency, updates `state.frequency` and waterfall labels (same as before).
2. Sets `state.pending_freq = Some(freq)`.
3. Does NOT touch `sdr_cell` at all — no take/retry loop.
4. Logs the queued change.

**Logging:** `console::log_1("Radioband: frequency change queued: {} MHz")`

#### Task 3: Rewrite gain slider handler to set pending flag
**File:** `crates/app-ui/src/lib.rs`  
**Deliverable:** Replace the retry-loop gain handler with one that:
1. Updates `state.gain` and UI label (same as before).
2. Sets `state.pending_gain = Some(val)`.
3. Does NOT touch `sdr_cell` — no take/retry loop.

**Logging:** `console::log_1("Radioband: gain change queued")`

#### Task 4: Apply pending changes inside read_loop
**File:** `crates/app-ui/src/lib.rs`  
**Deliverable:** In the `read_loop`, after each successful `read_block`, check `state.pending_freq` and `state.pending_gain`. If either is `Some`, apply the change using the SDR already in hand:
1. Check `pending_freq`: if Some(freq), call `sdr.set_center_freq(freq)`, then `sdr.reset_buffer()`, log result. Clear the pending flag. Send `clear_waterfall` message to worker.
2. Check `pending_gain`: if Some(gain), call `sdr.set_gain(gain)`, log result. Clear the pending flag.
3. On error: log the error, retry on the next loop iteration (leave pending flag set).

**Logging:**
- `"read_loop: applying frequency change to {} Hz"` (DEBUG)
- `"read_loop: frequency change applied successfully"` or `"read_loop: frequency change FAILED: {:?}"` (ERROR)
- Same pattern for gain.

### Phase 2: Worker — Clear Waterfall on Frequency Change

#### Task 5: Add clear_waterfall export to sdr-worker
**File:** `crates/sdr-worker/src/lib.rs`  
**Deliverable:** Add a `#[wasm_bindgen] pub fn clear_waterfall()` function that calls `pipeline.waterfall.clear()`.

#### Task 6: Handle clear_waterfall message in worker.js
**File:** `static/worker.js`  
**Deliverable:** Add a `'clear_waterfall'` case to the message switch that calls `wasm.clear_waterfall()`.

### Phase 3: Tests

#### Task 7: Add unit test for pending-change mechanism
**File:** `crates/sdr-core/src/lib.rs`  
**Deliverable:** Add test for `WaterfallBuffer::clear()` ensuring the buffer is zeroed and `write_pos` reset.

### Phase 4: Build & Verify

#### Task 8: Build and verify
**Deliverable:** Run `bash build.sh`, verify no compile errors. Check that:
- Console shows `"read_loop: applying frequency change..."` messages when frequency is changed.
- Waterfall clears and shows new frequency's spectrum after change.

## Commit Plan

**Commit 1 (after Tasks 1-4):** `fix: cooperative frequency/gain changes via read_loop`
**Commit 2 (after Tasks 5-6):** `feat: clear waterfall buffer on frequency change`
**Commit 3 (after Tasks 7-8):** `test: add waterfall clear test + build verify`
