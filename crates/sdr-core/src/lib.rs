//! Pure Rust DSP library for SDR processing.
//! No web dependencies — works on any target including wasm32.

use num_complex::Complex;
use rustfft::FftPlanner;
use std::f32::consts::PI;

// ── Types ──────────────────────────────────────────────────────────────────

/// Demodulation mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemodMode {
    /// Wideband FM (broadcast radio)
    WFM,
    /// Narrowband FM (walkie-talkie, amateur)
    NFM,
    /// Amplitude modulation
    AM,
}

/// Configuration for the DSP pipeline.
#[derive(Clone, Debug)]
pub struct DspConfig {
    pub sample_rate: u32,
    pub fft_size: usize,
    pub mode: DemodMode,
    pub audio_rate: u32,
}

/// Results from one DSP processing block.
pub struct DspResult {
    /// Spectrum magnitude in dB, FFT-shifted (DC in center).
    pub spectrum: Vec<f32>,
    /// Demodulated audio PCM samples (mono, f32 in -1..1 range).
    pub audio: Vec<f32>,
}

// ── Conversion ─────────────────────────────────────────────────────────────

/// Convert RTL-SDR unsigned-8 IQ samples to `Complex<f32>`.
/// Each pair of bytes is (I, Q), unsigned 0–255, center 127.5.
#[inline]
pub fn u8_iq_to_complex(raw: &[u8]) -> Vec<Complex<f32>> {
    raw.chunks_exact(2)
        .map(|pair| Complex {
            re: (pair[0] as f32 - 127.5) / 127.5,
            im: (pair[1] as f32 - 127.5) / 127.5,
        })
        .collect()
}

// ── Low-pass FIR ───────────────────────────────────────────────────────────

/// Simple windowed-sinc FIR low-pass filter (complex-valued).
pub struct FirLowPass {
    taps: Vec<f32>,
    delay: Vec<Complex<f32>>,
    pos: usize,
}

impl FirLowPass {
    /// Create filter with given cutoff (normalised to sample_rate) and number of taps.
    pub fn new(cutoff_hz: f32, sample_rate: f32, num_taps: usize) -> Self {
        let fc = cutoff_hz / sample_rate;
        let m = num_taps as isize;
        let half = m / 2;
        let mut taps = Vec::with_capacity(num_taps);
        let mut sum = 0.0f32;
        for i in 0..m {
            let n = i - half;
            let h = if n == 0 {
                2.0 * PI * fc
            } else {
                (2.0 * PI * fc * n as f32).sin() / (n as f32)
            };
            // Hann window
            let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / (m - 1) as f32).cos());
            let val = h * w;
            sum += val;
            taps.push(val);
        }
        // Normalise
        for t in &mut taps {
            *t /= sum;
        }
        FirLowPass {
            delay: vec![Complex::new(0.0, 0.0); num_taps],
            taps,
            pos: 0,
        }
    }

    /// Filter one sample, return filtered output.
    pub fn process_one(&mut self, sample: Complex<f32>) -> Complex<f32> {
        self.delay[self.pos] = sample;
        let mut out = Complex::new(0.0, 0.0);
        let n = self.taps.len();
        for i in 0..n {
            let idx = (self.pos + n - i) % n;
            out += self.delay[idx] * self.taps[i];
        }
        self.pos = (self.pos + 1) % n;
        out
    }

    /// Filter and decimate: process `decim` input samples, return one output.
    pub fn filter_decimate(&mut self, samples: &[Complex<f32>], decim: usize) -> Vec<Complex<f32>> {
        let mut out = Vec::with_capacity(samples.len() / decim + 1);
        for (i, &s) in samples.iter().enumerate() {
            let y = self.process_one(s);
            if i % decim == 0 {
                out.push(y);
            }
        }
        out
    }
}

// ── Real-valued FIR low-pass ───────────────────────────────────────────────

/// Simple windowed-sinc FIR low-pass filter for real signals.
pub struct FirLowPassReal {
    taps: Vec<f32>,
    delay: Vec<f32>,
    pos: usize,
}

impl FirLowPassReal {
    pub fn new(cutoff_hz: f32, sample_rate: f32, num_taps: usize) -> Self {
        let fc = cutoff_hz / sample_rate;
        let m = num_taps as isize;
        let half = m / 2;
        let mut taps = Vec::with_capacity(num_taps);
        let mut sum = 0.0f32;
        for i in 0..m {
            let n = i - half;
            let h = if n == 0 {
                2.0 * PI * fc
            } else {
                (2.0 * PI * fc * n as f32).sin() / (n as f32)
            };
            let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / (m - 1) as f32).cos());
            let val = h * w;
            sum += val;
            taps.push(val);
        }
        for t in &mut taps {
            *t /= sum;
        }
        FirLowPassReal {
            delay: vec![0.0; num_taps],
            taps,
            pos: 0,
        }
    }

    pub fn filter_decimate(&mut self, samples: &[f32], decim: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples.len() / decim + 1);
        for (i, &s) in samples.iter().enumerate() {
            self.delay[self.pos] = s;
            if i % decim == 0 {
                let mut y = 0.0f32;
                let n = self.taps.len();
                for j in 0..n {
                    let idx = (self.pos + n - j) % n;
                    y += self.delay[idx] * self.taps[j];
                }
                out.push(y);
            }
            self.pos = (self.pos + 1) % self.taps.len();
        }
        out
    }
}

// ── FM Demodulation ────────────────────────────────────────────────────────

/// FM demodulator using the conjugate-product method.
pub struct FmDemod {
    prev: Complex<f32>,
    gain: f32,
}

impl FmDemod {
    /// `max_deviation`: max frequency deviation in Hz
    /// `sample_rate`: sample rate after decimation
    pub fn new(max_deviation: f32, sample_rate: f32) -> Self {
        FmDemod {
            prev: Complex::new(0.0, 0.0),
            gain: sample_rate / (2.0 * PI * max_deviation),
        }
    }

    pub fn demodulate(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples.len());
        for &s in samples {
            let product = s * self.prev.conj();
            let phase = product.im.atan2(product.re);
            out.push(phase * self.gain);
            self.prev = s;
        }
        out
    }
}

// ── AM Demodulation ────────────────────────────────────────────────────────

/// AM envelope demodulator.
pub fn am_demod(samples: &[Complex<f32>]) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples.len());
    // DC-blocking: compute mean magnitude then subtract
    let mean: f32 = samples.iter().map(|s| s.norm()).sum::<f32>() / samples.len().max(1) as f32;
    for s in samples {
        out.push(s.norm() - mean);
    }
    out
}

// ── De-emphasis filter ─────────────────────────────────────────────────────

/// Single-pole IIR de-emphasis filter.
/// Standard: 75 µs (US) or 50 µs (EU).
pub struct DeEmphasis {
    alpha: f32,
    prev: f32,
}

impl DeEmphasis {
    pub fn new(tau_us: f32, sample_rate: f32) -> Self {
        let dt = 1.0 / sample_rate;
        let tau = tau_us * 1e-6;
        let alpha = dt / (tau + dt);
        DeEmphasis { alpha, prev: 0.0 }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            self.prev = self.alpha * *s + (1.0 - self.alpha) * self.prev;
            *s = self.prev;
        }
    }
}

// ── Decimation ─────────────────────────────────────────────────────────────

/// Simple decimation (take every N-th sample).
pub fn decimate(samples: &[f32], factor: usize) -> Vec<f32> {
    samples.iter().step_by(factor).copied().collect()
}

// ── FFT / Spectrum ─────────────────────────────────────────────────────────

/// Compute power spectrum in dB from complex IQ samples.
/// Output is FFT-shifted: DC (center frequency) in the middle.
pub fn compute_spectrum(samples: &[Complex<f32>], fft_size: usize) -> Vec<f32> {
    let len = samples.len().min(fft_size);
    let mut buffer = vec![Complex::new(0.0, 0.0); fft_size];
    buffer[..len].copy_from_slice(&samples[..len]);

    // Hann window
    for (i, s) in buffer.iter_mut().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / fft_size as f32).cos());
        *s *= w;
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    fft.process(&mut buffer);

    // Magnitude → dB, FFT-shift
    let half = fft_size / 2;
    let mut result = vec![0.0f32; fft_size];
    for i in 0..fft_size {
        let idx = (i + half) % fft_size;
        let mag = buffer[idx].norm() / fft_size as f32;
        result[i] = 20.0 * mag.max(1e-10).log10();
    }
    result
}

// ── Color mapping ──────────────────────────────────────────────────────────

/// Map a dB value to an RGBA color for waterfall display.
/// Uses a simple blue→cyan→green→yellow→red gradient.
pub fn db_to_rgba(db: f32, min_db: f32, max_db: f32) -> [u8; 4] {
    let t = ((db - min_db) / (max_db - min_db)).clamp(0.0, 1.0);

    let (r, g, b) = if t < 0.25 {
        let u = t / 0.25;
        (0.0, 0.0, u)
    } else if t < 0.5 {
        let u = (t - 0.25) / 0.25;
        (0.0, u, 1.0)
    } else if t < 0.75 {
        let u = (t - 0.5) / 0.25;
        (u, 1.0, 1.0 - u)
    } else {
        let u = (t - 0.75) / 0.25;
        (1.0, 1.0 - u, 0.0)
    };

    [
        (r * 255.0) as u8,
        (g * 255.0) as u8,
        (b * 255.0) as u8,
        255,
    ]
}

// ── Waterfall ring buffer ──────────────────────────────────────────────────

/// Ring buffer of spectrum rows for waterfall display.
pub struct WaterfallBuffer {
    /// RGBA pixel data, row-major, newest row at write_pos.
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub write_pos: usize,
    pub min_db: f32,
    pub max_db: f32,
}

impl WaterfallBuffer {
    pub fn new(width: usize, height: usize) -> Self {
        WaterfallBuffer {
            data: vec![0u8; width * height * 4],
            width,
            height,
            write_pos: 0,
            min_db: -60.0,
            max_db: 0.0,
        }
    }

    /// Push a new spectrum line.
    pub fn push_line(&mut self, spectrum: &[f32]) {
        let row_start = self.write_pos * self.width * 4;
        for (i, &db) in spectrum.iter().take(self.width).enumerate() {
            let rgba = db_to_rgba(db, self.min_db, self.max_db);
            let off = row_start + i * 4;
            self.data[off] = rgba[0];
            self.data[off + 1] = rgba[1];
            self.data[off + 2] = rgba[2];
            self.data[off + 3] = rgba[3];
        }
        self.write_pos = (self.write_pos + 1) % self.height;
    }

    /// Get rows in order (oldest first) as contiguous RGBA bytes.
    pub fn get_ordered_rows(&self) -> Vec<u8> {
        let row_bytes = self.width * 4;
        let mut out = Vec::with_capacity(self.data.len());
        for i in 0..self.height {
            let row = (self.write_pos + i) % self.height;
            let start = row * row_bytes;
            out.extend_from_slice(&self.data[start..start + row_bytes]);
        }
        out
    }
}

// ── DSP Pipeline ───────────────────────────────────────────────────────────

/// Complete DSP pipeline: IQ → spectrum + audio.
pub struct DspPipeline {
    pub config: DspConfig,
    iq_filter: FirLowPass,
    audio_filter: FirLowPassReal,
    fm_demod: FmDemod,
    de_emphasis: DeEmphasis,
    iq_decim: usize,
    audio_decim: usize,
    pub waterfall: WaterfallBuffer,
}

impl DspPipeline {
    pub fn new(config: DspConfig) -> Self {
        let (iq_cutoff, iq_decim, fm_dev, audio_decim) = match config.mode {
            DemodMode::WFM => {
                // 2.4 MSps → /10 → 240 kHz → FM demod → /5 → 48 kHz
                let iq_d = (config.sample_rate / 240_000).max(1) as usize;
                let audio_d = (240_000 / config.audio_rate).max(1) as usize;
                (120_000.0f32, iq_d, 75_000.0f32, audio_d)
            }
            DemodMode::NFM => {
                let iq_d = (config.sample_rate / 48_000).max(1) as usize;
                (8_000.0f32, iq_d, 5_000.0f32, 1)
            }
            DemodMode::AM => {
                let iq_d = (config.sample_rate / 48_000).max(1) as usize;
                (5_000.0f32, iq_d, 5_000.0f32, 1)
            }
        };

        let intermediate_rate = config.sample_rate as f32 / iq_decim as f32;

        DspPipeline {
            iq_filter: FirLowPass::new(iq_cutoff, config.sample_rate as f32, 31),
            audio_filter: FirLowPassReal::new(15_000.0, intermediate_rate, 31),
            fm_demod: FmDemod::new(fm_dev, intermediate_rate),
            // De-emphasis operates at final audio rate (AFTER decimation), per reference
            de_emphasis: DeEmphasis::new(50.0, config.audio_rate as f32),
            iq_decim,
            audio_decim,
            waterfall: WaterfallBuffer::new(config.fft_size, 200),
            config,
        }
    }

    /// Process a block of raw u8 IQ data.
    pub fn process(&mut self, raw_iq: &[u8]) -> DspResult {
        let iq = u8_iq_to_complex(raw_iq);

        // Spectrum (from raw IQ, full bandwidth)
        let spectrum = if iq.len() >= self.config.fft_size {
            compute_spectrum(&iq, self.config.fft_size)
        } else {
            vec![self.waterfall.min_db; self.config.fft_size]
        };
        self.waterfall.push_line(&spectrum);

        // Decimate IQ
        let decimated = self.iq_filter.filter_decimate(&iq, self.iq_decim);

        // Demodulate
        let mut audio = match self.config.mode {
            DemodMode::WFM | DemodMode::NFM => self.fm_demod.demodulate(&decimated),
            DemodMode::AM => am_demod(&decimated),
        };

        // Audio decimation if needed (BEFORE de-emphasis, per reference)
        let mut audio = if self.audio_decim > 1 {
            self.audio_filter.filter_decimate(&audio, self.audio_decim)
        } else {
            audio
        };

        // De-emphasis (only for WFM, applied AFTER decimation to audio rate)
        if self.config.mode == DemodMode::WFM {
            self.de_emphasis.process(&mut audio);
        }

        // Clamp audio to -1..1
        let audio: Vec<f32> = audio.iter().map(|&s| s.clamp(-1.0, 1.0)).collect();

        DspResult { spectrum, audio }
    }
}

// ── Mock IQ Source ─────────────────────────────────────────────────────────

/// Generates fake IQ data for testing without real hardware.
/// Produces a WFM-like signal: carrier + FM-modulated tone.
pub struct MockIqSource {
    phase: f32,
    audio_phase: f32,
    sample_rate: f32,
    carrier_offset: f32,
    mod_freq: f32,
    fm_deviation: f32,
    noise_seed: u32,
}

impl MockIqSource {
    pub fn new(sample_rate: f32) -> Self {
        MockIqSource {
            phase: 0.0,
            audio_phase: 0.0,
            sample_rate,
            carrier_offset: 50_000.0,  // 50 kHz offset from center
            mod_freq: 1000.0,           // 1 kHz audio tone
            fm_deviation: 75_000.0,     // ±75 kHz WFM
            noise_seed: 42,
        }
    }

    /// Generate `num_samples` IQ pairs as u8 (RTL-SDR format).
    pub fn generate(&mut self, num_bytes: usize) -> Vec<u8> {
        let num_samples = num_bytes / 2;
        let mut out = Vec::with_capacity(num_bytes);
        let dt = 1.0 / self.sample_rate;

        for _ in 0..num_samples {
            // Audio signal: simple sine tone
            let audio = (2.0 * PI * self.mod_freq * self.audio_phase).sin();
            self.audio_phase += dt;
            if self.audio_phase > 1.0 {
                self.audio_phase -= 1.0;
            }

            // FM modulation: instantaneous frequency = carrier + deviation * audio
            let inst_freq = self.carrier_offset + self.fm_deviation * audio;
            self.phase += 2.0 * PI * inst_freq * dt;
            if self.phase > PI {
                self.phase -= 2.0 * PI;
            }

            // IQ signal
            let i_f = self.phase.cos();
            let q_f = self.phase.sin();

            // Add noise
            let noise_i = self.cheap_noise() * 0.05;
            let noise_q = self.cheap_noise() * 0.05;

            // Convert to u8 (0–255 centered at 127.5)
            let i_u8 = ((i_f + noise_i) * 80.0 + 127.5).clamp(0.0, 255.0) as u8;
            let q_u8 = ((q_f + noise_q) * 80.0 + 127.5).clamp(0.0, 255.0) as u8;

            out.push(i_u8);
            out.push(q_u8);
        }
        out
    }

    fn cheap_noise(&mut self) -> f32 {
        // Simple xorshift PRNG
        self.noise_seed ^= self.noise_seed << 13;
        self.noise_seed ^= self.noise_seed >> 17;
        self.noise_seed ^= self.noise_seed << 5;
        (self.noise_seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u8_iq_to_complex() {
        let raw = [127, 128, 0, 255];
        let c = u8_iq_to_complex(&raw);
        assert_eq!(c.len(), 2);
        assert!((c[0].re - (-0.5 / 127.5)).abs() < 0.01);
    }

    #[test]
    fn test_mock_iq_source() {
        let mut mock = MockIqSource::new(2_400_000.0);
        let data = mock.generate(16384);
        assert_eq!(data.len(), 16384);
        // All values should be in 0..255
        assert!(data.iter().all(|&v| v <= 255));
    }

    #[test]
    fn test_spectrum() {
        let mut mock = MockIqSource::new(2_400_000.0);
        let raw = mock.generate(8192);
        let iq = u8_iq_to_complex(&raw);
        let spectrum = compute_spectrum(&iq, 2048);
        assert_eq!(spectrum.len(), 2048);
    }

    #[test]
    fn test_pipeline() {
        let config = DspConfig {
            sample_rate: 2_400_000,
            fft_size: 2048,
            mode: DemodMode::WFM,
            audio_rate: 48_000,
        };
        let mut pipeline = DspPipeline::new(config);
        let mut mock = MockIqSource::new(2_400_000.0);
        let raw = mock.generate(16384);
        let result = pipeline.process(&raw);
        assert_eq!(result.spectrum.len(), 2048);
        assert!(!result.audio.is_empty());
    }
}
