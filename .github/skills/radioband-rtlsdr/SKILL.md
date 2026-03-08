# Radioband RTL-SDR WebUSB Skill

## Description
Domain knowledge for developing, debugging, and extending the Radioband project — a browser-based RTL-SDR receiver built with Rust + WebAssembly + WebUSB.

## When to Use
- Debugging USB communication with RTL2832U + R820T
- Modifying the DSP pipeline (demodulation, decimation, filtering)
- Fixing audio quality issues
- Adding new demodulation modes
- Troubleshooting WebUSB / WebAudio / Web Worker interactions

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌───────────────┐
│  app-ui     │────▶│  sdr-worker  │────▶│  sdr-core     │
│  (main thr) │◀────│  (Web Worker)│◀────│  (DSP engine) │
│  + waterfall│     │              │     │  + FFT        │
│  + controls │     │              │     │  + demod      │
└──────┬──────┘     └──────────────┘     └───────────────┘
       │
       ▼
┌─────────────┐     ┌──────────────┐
│  usb-rtl    │     │ audio-bridge │
│  (WebUSB)   │     │ (AudioWorklet│
│  RTL2832U   │     │  + WebAudio) │
│  + R820T    │     └──────────────┘
└─────────────┘
```

## Critical Protocol Knowledge

### RTL2832U USB Protocol
- **All vendor OUT transfers**: `wIndex |= WRITE_FLAG (0x10)` — MUST be OR'd for every write
- **Block addresses**: USB=`0x0100`, SYS=`0x0200`, I2C=`0x0600`
- **Demod register access**: `wValue = (addr << 8) | 0x20`, `wIndex = page` (page ORs WRITE_FLAG automatically for writes)
- **Status readback**: After every demod write, read back from `page=0x0A, addr=0x01`
- **Minimum ctrl_in read**: 8 bytes (device may return less if fewer requested)
- **System registers**: `SYS_DEMOD_CTL (0x3000)` and `SYS_DEMOD_CTL1 (0x300B)` must be written during init
- **EPA_CTL reset**: Write `0x0210` then `0x0000` to `USB_EPA_CTL (0x2148)` to reset FIFO

### R820T Tuner Protocol
- **I2C address**: `0x34`
- **I2C write**: Send `[register, value]` to `BLOCK_I2C` with address as wValue
- **I2C read**: First write `[register]`, then read `(register + 1)` bytes. Target byte is at index `register`. Apply bit reversal.
- **Bit reversal**: Nibble-swap table — required on all I2C reads
- **IF frequency**: 3,570,000 Hz — must be set in demod registers `1:0x19-0x1B` with formula: `multiplier = -floor(freq * 2^22 / xtal)`
- **Spectrum inversion**: Register `1:0x15 = 0x01` (required for R820T low-IF architecture)

### R820T PLL Tuning
```
lo_freq = target_freq + IF_FREQ (3.57 MHz)
div_num = floor(log2(1770MHz / lo_freq))  [clamped 0..6]
mix_div = 2^(div_num + 1)
vco_freq = lo_freq * mix_div
nint = vco_freq / (2 * xtal)
vco_fra = vco_freq % (2 * xtal)

Reg 0x14: si = (nint-13) % 4, ni = (nint-13) / 4
           value = (si << 6) | (ni & 0x3F)
SDM:       sdm = min(65535, 32768 * vco_fra / xtal)
Reg 0x16:  sdm >> 8
Reg 0x15:  sdm & 0xFF
Reg 0x10:  (existing & 0x1F) | (div_num << 5)
```

### R820T Gain Control (Bit Polarity!)
| Register | Bit | Auto | Manual |
|----------|-----|------|--------|
| 0x05 | [4] | 0 (auto LNA) | 1 (manual LNA) |
| 0x05 | [3:0] | don't care | LNA gain index 0-15 |
| 0x07 | [4] | **1** (auto mixer) | **0** (manual mixer) |
| 0x07 | [3:0] | don't care | Mixer gain index 0-15 |
| 0x0C | — | `(r & 0x60) \| 0x0B` (VGA=26.5dB) | `(r & 0x60) \| 0x08` (VGA=16dB) |

**WARNING**: Mixer AGC polarity is INVERTED from LNA! bit[4]=1 means AUTO for mixer.

### RTL2832U Initialization Sequence (43 steps)
1. USB_SYSCTL = 0x09
2. EPA_MAXPKT = 0x0200
3. EPA_CTL = 0x0210 (stall + reset)
4. DEMOD_CTL1 = 0x22
5. DEMOD_CTL = 0xE8
6-7. Demod reset (1:0x01 = 0x14, then 0x10)
8. Spectrum inversion (1:0x15 = 0x01)
9-11. Carrier offset = 0 (1:0x16-0x18)
12-14. IF = 0 (1:0x19-0x1B) — overwritten later by set_if_frequency
15-34. LPF coefficients (1:0x1C-0x2F)
35. SDR mode (0:0x19 = 0x05)
36-37. FSM init (1:0x93 = 0xF0, 1:0x94 = 0x0F)
38. Disable DAGC (1:0x11 = 0x00)
39. AGC loop (1:0x04 = 0x00)
40. Error packets (0:0x61 = 0x60)
41. ADC datapath (0:0x06 = 0x80)
42. Zero-IF (1:0xB1 = 0x1B)
43. TP_CK0 (0:0x0D = 0x83)

## DSP Pipeline

### WFM (Wideband FM)
```
2.4 MSps IQ → FIR LPF (120kHz) + decimate /10 → 240 kHz
→ FM demod (conjugate product, atan2, gain=rate/2π*dev)
→ Audio FIR LPF (15kHz) + decimate /5 → 48 kHz
→ De-emphasis IIR (τ=50µs, fc=3183Hz) at 48kHz
→ Clamp [-1,1] → AudioWorklet
```

### AM
```
2.4 MSps IQ → FIR LPF (5kHz) + decimate /50 → 48 kHz
→ Envelope detection (sqrt(I²+Q²) - mean)
→ Clamp [-1,1] → AudioWorklet
```

### Critical DSP Rules
1. **De-emphasis MUST be applied AFTER final decimation** (at audio rate, not intermediate rate)
2. De-emphasis time constant: 50µs (EU) or 75µs (US)
3. FM demod gain = `sample_rate / (2π × max_deviation)`
4. USB block size must sustain throughput: at 2.4 MSps × 2 bytes = 4.8 MB/s
5. Read loop must NOT sleep — use `sleep_ms(0)` for yield only
6. Block size ≥ 128KB for continuous streaming

## Common Pitfalls
1. **RefCell borrow across await**: Use take-and-put-back pattern with `SdrCell = Rc<RefCell<Option<RtlSdr>>>`
2. **Race condition**: read_loop and frequency handler compete for SdrCell — both must handle `None` gracefully
3. **Sleep in read loop**: Even 16ms sleep causes 80% data loss at 2.4 MSps
4. **Missing WRITE_FLAG**: All ctrl_out must OR `0x10` into wIndex
5. **Missing IF frequency**: Without it, baseband is shifted 3.57 MHz — stations appear off-center
6. **Mixer gain bit polarity**: Opposite of LNA — caused gain slider to not work

## Reference Implementations
- `jtarrio/webrtlsdr` — TypeScript USB driver (ground truth for protocol)
- `jtarrio/radioreceiver` — Full app with spectrum display
- `jtarrio/signals` — DSP library (demodulation, filtering)
