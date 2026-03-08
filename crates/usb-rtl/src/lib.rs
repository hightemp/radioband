//! WebUSB driver for RTL2832U-based SDR dongles.
//!
//! Protocol matched to jtarrio/webrtlsdr (TypeScript reference implementation).
//! Handles device discovery, initialization, register access,
//! R820T tuner configuration, and bulk IQ sample reading.

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    console, UsbControlTransferParameters, UsbDevice, UsbDeviceFilter,
    UsbDeviceRequestOptions, UsbInTransferResult, UsbRecipient, UsbRequestType,
};

// ── Constants ──────────────────────────────────────────────────────────────

const RTL2832_VENDOR_ID: u16 = 0x0bda;
const RTL2832_PRODUCT_IDS: &[u16] = &[0x2832, 0x2838];

/// Write-flag OR'd into wIndex for all control-transfer-out operations.
const WRITE_FLAG: u16 = 0x10;

// Block numbers (used as wIndex base)
const BLOCK_USB: u16 = 0x0100;
const BLOCK_SYS: u16 = 0x0200;
const BLOCK_I2C: u16 = 0x0600;

// USB registers (addressed via wValue)
const USB_SYSCTL: u16 = 0x2000;
const USB_EPA_CTL: u16 = 0x2148;
const USB_EPA_MAXPKT: u16 = 0x2158;

// System registers
const SYS_DEMOD_CTL: u16 = 0x3000;
const SYS_DEMOD_CTL1: u16 = 0x300B;

// I2C address
const R820T_I2C_ADDR: u8 = 0x34;

/// R820T IF frequency offset (Hz).
const R820T_IF_FREQ: u32 = 3_570_000;

// R820T init register values (regs 0x05 through 0x1F) — matches webrtlsdr r8xx.ts
const R820T_INIT_REGS: [u8; 27] = [
    0x83, 0x32, 0x75, 0xC0, 0x40, 0xD6, 0x6C, 0xF5, 0x63, 0x75, 0x68, 0x6C,
    0x83, 0x80, 0x00, 0x0F, 0x00, 0xC0, 0x30, 0x48, 0xCC, 0x60, 0x00, 0x54,
    0xAE, 0x4A, 0xC0,
];

// Crystal frequency
const XTAL_FREQ: u32 = 28_800_000;

// Bulk endpoint for IQ data (EP1 IN)
const BULK_ENDPOINT: u8 = 0x01;

// Default read block size (bytes) — must sustain sample rate throughput
// At 2.4 MSps × 2 bytes = 4.8 MB/s; with ~20 reads/sec → ~240KB/read
const DEFAULT_BLOCK_SIZE: u32 = 131072;

// LPF coefficients for RTL2832U init (regs 0x1C..0x2F on demod page 1)
const LPF_COEFS: [u8; 20] = [
    0xCA, 0xDC, 0xD7, 0xD8, 0xE0, 0xF2, 0x0E, 0x35, 0x06, 0x50,
    0x9C, 0x0D, 0x71, 0x11, 0x14, 0x71, 0x74, 0x19, 0x41, 0xA5,
];

// ── Public API ─────────────────────────────────────────────────────────────

pub struct RtlSdr {
    device: UsbDevice,
    tuner_freq: u32,
    sample_rate: u32,
    gain: i32,
}

#[derive(Clone, Debug)]
pub struct RtlSdrConfig {
    pub center_freq: u32,
    pub sample_rate: u32,
    pub gain: i32,
}

impl Default for RtlSdrConfig {
    fn default() -> Self {
        RtlSdrConfig {
            center_freq: 100_000_000,
            sample_rate: 2_400_000,
            gain: 0,
        }
    }
}

/// Request an RTL-SDR device from the user via WebUSB.
pub async fn request_device() -> Result<UsbDevice, JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let usb = window.navigator().usb();

    let filters: Vec<UsbDeviceFilter> = RTL2832_PRODUCT_IDS
        .iter()
        .map(|&pid| {
            let f = UsbDeviceFilter::new();
            f.set_vendor_id(RTL2832_VENDOR_ID);
            f.set_product_id(pid);
            f
        })
        .collect();

    let opts = UsbDeviceRequestOptions::new(&filters);
    let device: UsbDevice = JsFuture::from(usb.request_device(&opts))
        .await?
        .unchecked_into();
    Ok(device)
}

impl RtlSdr {
    pub async fn new(device: UsbDevice, config: &RtlSdrConfig) -> Result<Self, JsValue> {
        JsFuture::from(device.open()).await?;
        log("RTL-SDR: device opened");

        JsFuture::from(device.select_configuration(1)).await?;
        log("RTL-SDR: configuration selected");

        JsFuture::from(device.claim_interface(0)).await?;
        log("RTL-SDR: interface claimed");

        let mut sdr = RtlSdr {
            device,
            tuner_freq: config.center_freq,
            sample_rate: config.sample_rate,
            gain: config.gain,
        };

        sdr.init_rtl2832u().await?;
        log("RTL-SDR: RTL2832U initialized");

        sdr.open_i2c().await?;
        sdr.init_r820t().await?;
        sdr.close_i2c().await?;
        log("RTL-SDR: R820T tuner initialized");

        // Set IF frequency in demod to match R820T IF offset.
        // Without this, the baseband is shifted by 3.57 MHz and
        // stations won't appear centered.
        sdr.set_if_frequency(R820T_IF_FREQ).await?;
        log("RTL-SDR: IF frequency set");

        sdr.set_sample_rate(config.sample_rate).await?;

        sdr.open_i2c().await?;
        sdr.set_r820t_freq(config.center_freq).await?;
        sdr.close_i2c().await?;
        log(&format!(
            "RTL-SDR: frequency set to {} Hz",
            config.center_freq
        ));

        // Always set gain (even gain=0 sets proper AGC mode)
        sdr.open_i2c().await?;
        sdr.set_r820t_gain(config.gain).await?;
        sdr.close_i2c().await?;

        sdr.reset_buffer().await?;
        log("RTL-SDR: ready");
        Ok(sdr)
    }

    // ── Public: sample reading ─────────────────────────────────────────

    pub async fn read_samples(&self, length: u32) -> Result<Vec<u8>, JsValue> {
        let promise = self.device.transfer_in(BULK_ENDPOINT, length);
        let result: UsbInTransferResult = JsFuture::from(promise).await?.unchecked_into();

        let status = result.status();
        if status == web_sys::UsbTransferStatus::Stall {
            let _ = JsFuture::from(
                self.device
                    .clear_halt(web_sys::UsbDirection::In, BULK_ENDPOINT),
            )
            .await;
            return Ok(vec![0u8; length as usize]);
        }
        if status != web_sys::UsbTransferStatus::Ok {
            return Err(JsValue::from_str(&format!(
                "USB transfer status: {:?}",
                status
            )));
        }

        if let Some(dv) = result.data() {
            let arr = Uint8Array::new(&dv.buffer());
            let mut vec = vec![0u8; arr.length() as usize];
            arr.copy_to(&mut vec);
            Ok(vec)
        } else {
            Err(JsValue::from_str("No data in transfer result"))
        }
    }

    pub async fn read_block(&self) -> Result<Vec<u8>, JsValue> {
        self.read_samples(DEFAULT_BLOCK_SIZE).await
    }

    // ── Public: tuning ─────────────────────────────────────────────────

    pub async fn set_center_freq(&mut self, freq: u32) -> Result<(), JsValue> {
        self.open_i2c().await?;
        self.set_r820t_freq(freq).await?;
        self.close_i2c().await?;
        self.tuner_freq = freq;
        log(&format!("RTL-SDR: frequency set to {} Hz", freq));
        Ok(())
    }

    pub async fn set_sample_rate(&mut self, rate: u32) -> Result<(), JsValue> {
        let ratio = ((XTAL_FREQ as f64 * (1u64 << 22) as f64 / rate as f64) + 0.5) as u32;
        let ratio = ratio & 0x0FFF_FFFC;
        let real_rate = (XTAL_FREQ as f64 * (1u64 << 22) as f64 / ratio as f64) as u32;

        self.demod_write_reg(1, 0x9F, (ratio >> 16) as u16, 2)
            .await?;
        self.demod_write_reg(1, 0xA1, ratio as u16, 2).await?;

        // Reset demodulator
        self.demod_write_reg(1, 0x01, 0x14, 1).await?;
        self.demod_write_reg(1, 0x01, 0x10, 1).await?;

        self.sample_rate = real_rate;
        log(&format!("RTL-SDR: sample rate set to {} Sps", real_rate));
        Ok(())
    }

    pub async fn set_gain(&mut self, gain_tenth_db: i32) -> Result<(), JsValue> {
        self.open_i2c().await?;
        self.set_r820t_gain(gain_tenth_db).await?;
        self.close_i2c().await?;
        self.gain = gain_tenth_db;
        Ok(())
    }

    pub async fn reset_buffer(&self) -> Result<(), JsValue> {
        self.set_usb_reg(USB_EPA_CTL, 0x0210, 2).await?;
        self.set_usb_reg(USB_EPA_CTL, 0x0000, 2).await?;
        Ok(())
    }

    pub async fn close(&self) -> Result<(), JsValue> {
        let _ = JsFuture::from(self.device.release_interface(0)).await;
        let _ = JsFuture::from(self.device.close()).await;
        log("RTL-SDR: device closed");
        Ok(())
    }

    pub fn tuner_freq(&self) -> u32 {
        self.tuner_freq
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    // ════════════════════════════════════════════════════════════════════
    //  Low-level vendor control transfers
    // ════════════════════════════════════════════════════════════════════

    /// Vendor OUT (host → device).  wIndex gets WRITE_FLAG OR'd in.
    async fn ctrl_out(&self, value: u16, index: u16, data: &[u8]) -> Result<(), JsValue> {
        let params = UsbControlTransferParameters::new(
            index | WRITE_FLAG,
            UsbRecipient::Device,
            0,
            UsbRequestType::Vendor,
            value,
        );
        let arr = Uint8Array::from(data);
        let p = self
            .device
            .control_transfer_out_with_u8_array(&params, &arr)?;
        JsFuture::from(p).await?;
        Ok(())
    }

    /// Vendor IN (device → host).  Minimum 8 bytes per reference.
    async fn ctrl_in(&self, value: u16, index: u16, len: u16) -> Result<Vec<u8>, JsValue> {
        let actual_len = len.max(8);
        let params = UsbControlTransferParameters::new(
            index, // no WRITE_FLAG for reads
            UsbRecipient::Device,
            0,
            UsbRequestType::Vendor,
            value,
        );
        let p = self.device.control_transfer_in(&params, actual_len);
        let result: UsbInTransferResult = JsFuture::from(p).await?.unchecked_into();
        if let Some(dv) = result.data() {
            let arr = Uint8Array::new(&dv.buffer());
            let total = arr.length() as usize;
            let mut vec = vec![0u8; total];
            arr.copy_to(&mut vec);
            vec.truncate(len as usize);
            Ok(vec)
        } else {
            Ok(vec![])
        }
    }

    // ── USB block registers ────────────────────────────────────────────

    async fn set_usb_reg(&self, addr: u16, val: u32, len: u8) -> Result<(), JsValue> {
        self.ctrl_out(addr, BLOCK_USB, &num_to_le(val, len)).await
    }

    // ── System registers ───────────────────────────────────────────────

    async fn set_sys_reg(&self, addr: u16, val: u8) -> Result<(), JsValue> {
        self.ctrl_out(addr, BLOCK_SYS, &[val]).await
    }

    // ── Demod registers ────────────────────────────────────────────────

    async fn demod_write_reg(
        &self,
        page: u16,
        addr: u16,
        val: u16,
        len: u8,
    ) -> Result<(), JsValue> {
        let wvalue = (addr << 8) | 0x20;
        let data = num_to_be(val as u32, len);
        self.ctrl_out(wvalue, page, &data).await?;
        // Status readback
        self.demod_read_reg(0x0A, 0x01).await?;
        Ok(())
    }

    async fn demod_read_reg(&self, page: u16, addr: u16) -> Result<u8, JsValue> {
        let wvalue = (addr << 8) | 0x20;
        let data = self.ctrl_in(wvalue, page, 1).await?;
        Ok(data.first().copied().unwrap_or(0))
    }

    // ── I2C repeater ───────────────────────────────────────────────────

    async fn open_i2c(&self) -> Result<(), JsValue> {
        self.demod_write_reg(1, 0x01, 0x18, 1).await
    }

    async fn close_i2c(&self) -> Result<(), JsValue> {
        self.demod_write_reg(1, 0x01, 0x10, 1).await
    }

    // ── I2C access (R820T tuner) ───────────────────────────────────────

    async fn i2c_write(&self, i2c_addr: u8, data: &[u8]) -> Result<(), JsValue> {
        self.ctrl_out(i2c_addr as u16, BLOCK_I2C, data).await
    }

    async fn i2c_read(&self, i2c_addr: u8, len: u16) -> Result<Vec<u8>, JsValue> {
        self.ctrl_in(i2c_addr as u16, BLOCK_I2C, len).await
    }

    async fn write_r820t_reg(&self, reg: u8, val: u8) -> Result<(), JsValue> {
        self.i2c_write(R820T_I2C_ADDR, &[reg, val]).await
    }

    async fn read_r820t_reg(&self, reg: u8) -> Result<u8, JsValue> {
        self.i2c_write(R820T_I2C_ADDR, &[reg]).await?;
        let data = self.i2c_read(R820T_I2C_ADDR, (reg as u16) + 1).await?;
        Ok(data
            .get(reg as usize)
            .copied()
            .map(bit_rev)
            .unwrap_or(0))
    }

    // ════════════════════════════════════════════════════════════════════
    //  RTL2832U initialization  (matches webrtlsdr rtl2832u.ts _init)
    // ════════════════════════════════════════════════════════════════════

    async fn init_rtl2832u(&self) -> Result<(), JsValue> {
        // 1. USB_SYSCTL
        self.set_usb_reg(USB_SYSCTL, 0x09, 1).await?;
        // 2. EPA max packet = 512
        self.set_usb_reg(USB_EPA_MAXPKT, 0x0200, 2).await?;
        // 3. EPA_CTL stall + FIFO reset
        self.set_usb_reg(USB_EPA_CTL, 0x0210, 2).await?;
        // 4. DEMOD_CTL1
        self.set_sys_reg(SYS_DEMOD_CTL1, 0x22).await?;
        // 5. DEMOD_CTL: ADC enable, PLL enable
        self.set_sys_reg(SYS_DEMOD_CTL, 0xE8).await?;
        // 6–7. Reset demod
        self.demod_write_reg(1, 0x01, 0x14, 1).await?;
        self.demod_write_reg(1, 0x01, 0x10, 1).await?;
        // 8. Spectrum inversion — R820T requires inverted spectrum
        // Reference: r8xx.ts open() sets this to 0x01
        self.demod_write_reg(1, 0x15, 0x01, 1).await?;
        // 9–11. Carrier offset = 0
        self.demod_write_reg(1, 0x16, 0x00, 1).await?;
        self.demod_write_reg(1, 0x17, 0x00, 1).await?;
        self.demod_write_reg(1, 0x18, 0x00, 1).await?;
        // 12–14. IF = 0
        self.demod_write_reg(1, 0x19, 0x00, 1).await?;
        self.demod_write_reg(1, 0x1A, 0x00, 1).await?;
        self.demod_write_reg(1, 0x1B, 0x00, 1).await?;
        // 15–34. LPF coefficients
        for (i, &c) in LPF_COEFS.iter().enumerate() {
            self.demod_write_reg(1, 0x1C + i as u16, c as u16, 1)
                .await?;
        }
        // 35. SDR mode
        self.demod_write_reg(0, 0x19, 0x05, 1).await?;
        // 36–37. FSM init
        self.demod_write_reg(1, 0x93, 0xF0, 1).await?;
        self.demod_write_reg(1, 0x94, 0x0F, 1).await?;
        // 38. Disable DAGC
        self.demod_write_reg(1, 0x11, 0x00, 1).await?;
        // 39. AGC loop
        self.demod_write_reg(1, 0x04, 0x00, 1).await?;
        // 40. Error packets
        self.demod_write_reg(0, 0x61, 0x60, 1).await?;
        // 41. ADC datapath
        self.demod_write_reg(0, 0x06, 0x80, 1).await?;
        // 42. Zero-IF
        self.demod_write_reg(1, 0xB1, 0x1B, 1).await?;
        // 43. TP_CK0
        self.demod_write_reg(0, 0x0D, 0x83, 1).await?;

        Ok(())
    }

    // ════════════════════════════════════════════════════════════════════
    //  R820T tuner
    // ════════════════════════════════════════════════════════════════════

    async fn init_r820t(&self) -> Result<(), JsValue> {
        for (i, &val) in R820T_INIT_REGS.iter().enumerate() {
            self.write_r820t_reg(0x05 + i as u8, val).await?;
        }
        Ok(())
    }

    /// PLL tune — matches webrtlsdr r8xx.ts _setPll()
    async fn set_r820t_freq(&self, freq: u32) -> Result<(), JsValue> {
        let lo_freq = freq as u64 + R820T_IF_FREQ as u64;

        let div_num = if lo_freq == 0 {
            6u8
        } else {
            let v = (1_770_000_000u64 / lo_freq) as u32;
            let bits = if v == 0 { 0 } else { 31 - v.leading_zeros() };
            (bits as u8).min(6)
        };
        let mix_div: u64 = 1 << (div_num as u64 + 1);
        let vco_freq = lo_freq * mix_div;
        let pll_ref = XTAL_FREQ as u64;
        let nint = (vco_freq / (2 * pll_ref)) as u32;
        let vco_fra = vco_freq % (2 * pll_ref);

        if nint < 13 {
            return Err(JsValue::from_str("PLL: nint too small"));
        }
        let ni = ((nint - 13) / 4) as u8;
        let si = ((nint - 13) % 4) as u8;

        self.write_r820t_reg(0x14, (si << 6) | (ni & 0x3F))
            .await?;

        // SDM
        let sdm = if vco_fra == 0 {
            let r12 = self.read_r820t_reg(0x12).await?;
            self.write_r820t_reg(0x12, r12 | 0x08).await?;
            0u16
        } else {
            let r12 = self.read_r820t_reg(0x12).await?;
            self.write_r820t_reg(0x12, r12 & !0x08).await?;
            ((32768u64 * vco_fra) / pll_ref).min(65535) as u16
        };

        self.write_r820t_reg(0x16, (sdm >> 8) as u8).await?;
        self.write_r820t_reg(0x15, (sdm & 0xFF) as u8).await?;

        let r10 = self.read_r820t_reg(0x10).await?;
        self.write_r820t_reg(0x10, (r10 & 0x1F) | (div_num << 5))
            .await?;

        // PLL lock check
        for _ in 0..20 {
            let r02 = self.read_r820t_reg(0x02).await?;
            if r02 & 0x40 != 0 {
                log("RTL-SDR: PLL locked");
                return Ok(());
            }
        }
        log("RTL-SDR: PLL lock timeout (may still work)");
        Ok(())
    }

    /// Set IF frequency in the RTL2832U demodulator.
    /// Formula from reference: multiplier = -floor(freq * 2^22 / xtal)
    async fn set_if_frequency(&self, freq: u32) -> Result<(), JsValue> {
        let multiplier = -((freq as i64 * (1i64 << 22)) / XTAL_FREQ as i64) as i32;
        self.demod_write_reg(1, 0x19, ((multiplier >> 16) & 0x3F) as u16, 1)
            .await?;
        self.demod_write_reg(1, 0x1A, ((multiplier >> 8) & 0xFF) as u16, 1)
            .await?;
        self.demod_write_reg(1, 0x1B, (multiplier & 0xFF) as u16, 1)
            .await?;
        Ok(())
    }

    /// Set R820T gain.  gain_tenth_db=0 → auto (AGC), >0 → manual.
    ///
    /// Bit polarity from reference (r8xx.ts):
    ///   reg 0x05 bit[4]: 0=auto LNA,  1=manual LNA
    ///   reg 0x07 bit[4]: 1=auto mixer, 0=manual mixer  (INVERTED!)
    ///   reg 0x0C mask 0b10011111: bit[7]=0, bit[4]=0 (VGA code-controlled)
    async fn set_r820t_gain(&self, gain_tenth_db: i32) -> Result<(), JsValue> {
        if gain_tenth_db == 0 {
            // Auto gain — matches reference setAutoGain()
            // LNA auto: reg 0x05 bit[4] = 0
            let r05 = self.read_r820t_reg(0x05).await?;
            self.write_r820t_reg(0x05, r05 & !0x10).await?;
            // Mixer auto: reg 0x07 bit[4] = 1
            let r07 = self.read_r820t_reg(0x07).await?;
            self.write_r820t_reg(0x07, r07 | 0x10).await?;
            // VGA code-controlled, gain index 0x0B (26.5 dB)
            let r0c = self.read_r820t_reg(0x0C).await?;
            self.write_r820t_reg(0x0C, (r0c & 0x60) | 0x0B).await?;
            log("RTL-SDR: gain set to auto (AGC)");
        } else {
            // Manual gain — matches reference setManualGain()
            let gain_db = gain_tenth_db as f32 / 10.0;
            let fullsteps = (gain_db / 3.5).floor() as u8;
            let halfsteps = if gain_db - 3.5 * fullsteps as f32 >= 2.3 {
                1u8
            } else {
                0u8
            };
            let lna_idx = (fullsteps + halfsteps).min(15);
            let mix_idx = fullsteps.min(15);

            // LNA manual: reg 0x05 bit[4] = 1, bits[3:0] = lna_idx
            let r05 = self.read_r820t_reg(0x05).await?;
            self.write_r820t_reg(0x05, (r05 & 0xE0) | 0x10 | lna_idx)
                .await?;
            // Mixer manual: reg 0x07 bit[4] = 0, bits[3:0] = mix_idx
            let r07 = self.read_r820t_reg(0x07).await?;
            self.write_r820t_reg(0x07, (r07 & 0xE0) | mix_idx).await?;
            // VGA code-controlled, gain index 0x08 (16 dB)
            let r0c = self.read_r820t_reg(0x0C).await?;
            self.write_r820t_reg(0x0C, (r0c & 0x60) | 0x08).await?;

            log(&format!(
                "RTL-SDR: gain manual LNA={} MIX={} VGA=8",
                lna_idx, mix_idx
            ));
        }
        Ok(())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn bit_rev(b: u8) -> u8 {
    const REV: [u8; 16] = [
        0x0, 0x8, 0x4, 0xC, 0x2, 0xA, 0x6, 0xE,
        0x1, 0x9, 0x5, 0xD, 0x3, 0xB, 0x7, 0xF,
    ];
    (REV[(b & 0x0F) as usize] << 4) | REV[(b >> 4) as usize]
}

fn num_to_le(val: u32, len: u8) -> Vec<u8> {
    (0..len).map(|i| (val >> (8 * i)) as u8).collect()
}

fn num_to_be(val: u32, len: u8) -> Vec<u8> {
    (0..len).rev().map(|i| (val >> (8 * i)) as u8).collect()
}

fn log(msg: &str) {
    console::log_1(&JsValue::from_str(msg));
}
