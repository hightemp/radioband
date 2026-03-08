# Radioband — Development Notes

## Architecture

Five Rust/WASM crates:
- **app-ui**: Main thread — DOM, events, WebUSB, AudioWorklet, worker bridge
- **usb-rtl**: RTL2832U + R820T driver via WebUSB vendor control transfers
- **sdr-worker**: Web Worker entry point — receives IQ, runs DSP, returns spectrum+audio
- **sdr-core**: Pure DSP — FFT, FIR filters, FM/AM demod, de-emphasis, waterfall buffer
- **audio-bridge**: AudioWorklet bridge — ring buffer, PCM feed

Build: Trunk for app-ui (ES modules), cargo+wasm-bindgen for sdr-worker (--target no-modules).

## Bug History & Lessons Learned

### 1. USB Bulk Transfer Hangs (initial)
**Symptom**: `read_block()` never returns.
**Root cause**: Entirely wrong USB protocol — missing WRITE_FLAG, wrong block constants, incomplete RTL2832U init.
**Fix**: Complete rewrite from jtarrio/webrtlsdr reference.

### 2. Spectrum Mirrored + Shifted
**Symptom**: Waterfall shows mirror image, stations displaced from center.
**Root causes**:
- Demod register `1:0x15` was `0x00`, must be `0x01` (spectrum inversion for R820T)
- IF frequency registers `1:0x19-0x1B` were zeroed — must set `multiplier = -floor(3570000 * 2^22 / 28800000)`
**Fix**: Set inversion flag and add `set_if_frequency()` call after tuner init.

### 3. RefCell Borrow Panics
**Symptom**: `RefCell<T> already borrowed` panic in read loop.
**Root cause**: Holding `RefCell` borrow of SDR across an `.await` point.
**Fix**: Separate `SdrCell = Rc<RefCell<Option<RtlSdr>>>` with take-and-put-back pattern.

### 4. Frequency Change Crashes Read Loop
**Symptom**: `sdr_cell is None!` error when changing frequency while streaming.
**Root cause**: Race between read_loop and frequency handler — both `.take()` from SdrCell.
**Fix**: read_loop skips on None (retry), frequency handler retries in loop up to 1s.

### 5. Gain Slider Has No Effect
**Symptom**: Moving gain slider changes nothing in waterfall.
**Root causes**:
- Gain handler only updated UI label, never called `sdr.set_gain()`
- Mixer AGC bit polarity inverted from LNA (reg 0x07 bit[4]=1 means AUTO, not manual)
- VGA register mask wrong
**Fix**: Wire gain slider to hardware, rewrite with correct bit polarity from reference.

### 6. Audio Garbled / Distorted (farting sound)
**Symptom**: Voice unintelligible, sounds like data is being chewed up.
**Root causes**:
- `sleep_ms(16)` in read loop: at 2.4 MSps only ~1 MB/s throughput vs 4.8 MB/s needed → 80% data loss
- Block size 16KB too small (reference uses ~100KB)
- De-emphasis applied BEFORE audio decimation at 240kHz rate with 48kHz time constant → wrong filter response
**Fix**: Block size → 128KB, sleep → 0ms (yield only), de-emphasis moved to AFTER decimation.

## Key Protocol Values

| Parameter | Value |
|---|---|
| Vendor ID | 0x0bda |
| Product IDs | 0x2832, 0x2838 |
| WRITE_FLAG | 0x10 |
| USB Block | 0x0100 |
| SYS Block | 0x0200 |
| I2C Block | 0x0600 |
| R820T I2C Address | 0x34 |
| IF Frequency | 3,570,000 Hz |
| Crystal | 28,800,000 Hz |
| Bulk Endpoint | 0x01 |
| Block Size | 131,072 bytes |

## DSP Parameters

| Mode | IQ Cutoff | IQ Decim | Intermediate Rate | FM Dev | Audio Decim | Output |
|---|---|---|---|---|---|---|
| WFM | 120 kHz | /10 | 240 kHz | 75 kHz | /5 | 48 kHz |
| NFM | 8 kHz | /50 | 48 kHz | 5 kHz | /1 | 48 kHz |
| AM | 5 kHz | /50 | 48 kHz | — | /1 | 48 kHz |

De-emphasis: τ=50µs (EU), applied at 48 kHz (AFTER decimation).

## Reference Repos

| Repo | Purpose | Key Files |
|---|---|---|
| jtarrio/webrtlsdr | USB protocol | rtl2832u.ts, r8xx.ts |
| jtarrio/radioreceiver | Full UI app | radio.ts, audioplayer.ts |
| jtarrio/signals | DSP engine | demodulators.ts, filters.ts, demod-wbfm.ts |

## Build & Deploy

```bash
# Full build
bash build.sh

# Deploy (GitHub Pages from docs/)
git add -A && git commit -m "msg" && git push

# Kernel modules must be blacklisted for WebUSB:
# /etc/modprobe.d/blacklist-rtlsdr.conf
# blacklist dvb_usb_rtl28xxu
# blacklist rtl2832
# blacklist rtl2832_sdr
# blacklist r820t
```

## FM vs AM

- 87.5-108 MHz = FM broadcast → use **WFM** mode
- AM broadcast = 530-1700 kHz (not reachable by RTL-SDR, min ~24 MHz)
- AM mode is for airband (118-137 MHz) or similar narrow AM transmissions
