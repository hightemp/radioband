//! WebUSB driver for RTL2832U-based SDR dongles.
//!
//! Handles device discovery, initialization, register access,
//! R820T/R828D tuner configuration, and bulk IQ sample reading.

use js_sys::{Object, Uint8Array};
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

// Register blocks
const BLOCK_DEMOD: u16 = 0; // Demodulator
const BLOCK_USB: u16 = 1; // USB interface

// USB block registers
const USB_SYSCTL: u16 = 0x2000;
const USB_EPA_CFG: u16 = 0x2148;
const USB_EPA_CTL: u16 = 0x2148;
const USB_EPA_MAXPKT: u16 = 0x2158;
const USB_EPA_FIFO_CFG: u16 = 0x2014;

// I2C block index for tuner access through RTL2832U passthrough
const I2C_INDEX: u16 = 0x0600; // block 6 << 8

// Demod I2C base (OR'd into page index)
const DEMOD_I2C_BASE: u16 = 0x000A;

// I2C addresses
const R820T_I2C_ADDR: u8 = 0x34;

// R820T init register values (regs 0x05 through 0x1F)
const R820T_INIT_REGS: [u8; 27] = [
    0x83, 0x32, 0x75, 0xC0, 0x40, 0xD6, 0x6C, 0xF5, 0x63, 0x75, 0x68, 0x6C,
    0x83, 0x80, 0x00, 0x0F, 0x00, 0x20, 0xFF, 0xFC, 0x02, 0x2A, 0x48, 0x34,
    0x37, 0xE0, 0x75,
];

// R820T tuner reference clock (from RTL2832U)
const R820T_REF_FREQ: u32 = 28_800_000;

// Bulk endpoint for IQ data (EP1 IN)
const BULK_ENDPOINT: u8 = 0x01;

// Default read block size
const DEFAULT_BLOCK_SIZE: u32 = 16384;

// ── Public API ─────────────────────────────────────────────────────────────

/// RTL-SDR device state.
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
    pub gain: i32, // 0 = auto, otherwise tenths of dB
}

impl Default for RtlSdrConfig {
    fn default() -> Self {
        RtlSdrConfig {
            center_freq: 100_000_000, // 100 MHz (FM broadcast)
            sample_rate: 2_400_000,   // 2.4 MSps
            gain: 0,                  // auto
        }
    }
}

/// Request an RTL-SDR device from the user via WebUSB.
/// Must be called from a user gesture handler.
pub async fn request_device() -> Result<UsbDevice, JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let navigator = window.navigator();
    let usb = navigator.usb();

    let filters: Vec<UsbDeviceFilter> = RTL2832_PRODUCT_IDS
        .iter()
        .map(|&pid| {
            let filter = UsbDeviceFilter::new();
            filter.set_vendor_id(RTL2832_VENDOR_ID);
            filter.set_product_id(pid);
            filter
        })
        .collect();

    let options = UsbDeviceRequestOptions::new(&filters);
    let promise = usb.request_device(&options);
    let device: UsbDevice = JsFuture::from(promise).await?.unchecked_into();
    Ok(device)
}

impl RtlSdr {
    /// Open and initialise an RTL-SDR device.
    pub async fn new(device: UsbDevice, config: &RtlSdrConfig) -> Result<Self, JsValue> {
        // Open device
        JsFuture::from(device.open()).await?;
        log("RTL-SDR: device opened");

        // Select configuration 1
        JsFuture::from(device.select_configuration(1)).await?;
        log("RTL-SDR: configuration selected");

        // Claim interface 0
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

        sdr.init_r820t().await?;
        log("RTL-SDR: R820T tuner initialized");

        sdr.set_sample_rate(config.sample_rate).await?;
        sdr.set_center_freq(config.center_freq).await?;

        if config.gain != 0 {
            sdr.set_gain(config.gain).await?;
        }

        sdr.reset_buffer().await?;
        log("RTL-SDR: ready");

        Ok(sdr)
    }

    /// Read a block of IQ samples from the device.
    pub async fn read_samples(&self, length: u32) -> Result<Vec<u8>, JsValue> {
        let promise = self.device.transfer_in(BULK_ENDPOINT, length);
        let result: UsbInTransferResult = JsFuture::from(promise).await?.unchecked_into();

        let status = result.status();
        if status != web_sys::UsbTransferStatus::Ok {
            return Err(JsValue::from_str(&format!(
                "USB transfer status: {:?}",
                status
            )));
        }

        if let Some(data_view) = result.data() {
            let buffer = data_view.buffer();
            let arr = Uint8Array::new(&buffer);
            let mut vec = vec![0u8; arr.length() as usize];
            arr.copy_to(&mut vec);
            Ok(vec)
        } else {
            Err(JsValue::from_str("No data in transfer result"))
        }
    }

    /// Read samples with default block size.
    pub async fn read_block(&self) -> Result<Vec<u8>, JsValue> {
        self.read_samples(DEFAULT_BLOCK_SIZE).await
    }

    /// Set center frequency.
    pub async fn set_center_freq(&mut self, freq: u32) -> Result<(), JsValue> {
        self.set_r820t_freq(freq).await?;
        self.tuner_freq = freq;
        log(&format!("RTL-SDR: frequency set to {} Hz", freq));
        Ok(())
    }

    /// Set sample rate.
    pub async fn set_sample_rate(&mut self, rate: u32) -> Result<(), JsValue> {
        // RTL2832U sample rate is derived from 28.8 MHz reference
        let real_rsamp_ratio = (R820T_REF_FREQ as f64 * (1u64 << 22) as f64 / rate as f64
            + 0.5) as u32;
        let real_rate =
            (R820T_REF_FREQ as f64 * (1u64 << 22) as f64 / real_rsamp_ratio as f64) as u32;

        // Write sample rate ratio (3 bytes, big-endian)
        self.demod_write_reg(1, 0x9F, (real_rsamp_ratio >> 16) as u16, 2)
            .await?;
        self.demod_write_reg(1, 0xA1, real_rsamp_ratio as u16, 2)
            .await?;

        // Set IF frequency to 0 (zero-IF mode)
        self.set_if_freq(0).await?;

        // Reset demodulator
        self.demod_write_reg(1, 0x01, 0x14, 1).await?;
        self.demod_write_reg(1, 0x01, 0x10, 1).await?;

        self.sample_rate = real_rate;
        log(&format!("RTL-SDR: sample rate set to {} Sps", real_rate));
        Ok(())
    }

    /// Set tuner gain in tenths of dB.
    pub async fn set_gain(&mut self, gain_tenth_db: i32) -> Result<(), JsValue> {
        // Ensure I2C repeater is on
        self.enable_i2c_repeater().await?;

        // Map gain to R820T LNA + mixer gain register values
        let lna_gain = ((gain_tenth_db.clamp(0, 500) / 35) as u8).min(15);
        let mixer_gain = 0x10_u8; // auto
        let vga_gain = ((gain_tenth_db.clamp(0, 500) / 20) as u8).min(15);

        // LNA gain (reg 0x05, bits [3:0])
        let mut val = self.read_i2c_reg(R820T_I2C_ADDR, 0x05).await?;
        val = (val & 0xF0) | lna_gain;
        self.write_i2c_reg(R820T_I2C_ADDR, 0x05, val).await?;

        // Mixer gain (reg 0x07, bit 4)
        val = self.read_i2c_reg(R820T_I2C_ADDR, 0x07).await?;
        val = (val & 0xEF) | mixer_gain;
        self.write_i2c_reg(R820T_I2C_ADDR, 0x07, val).await?;

        // VGA gain (reg 0x0C, bits [3:0])
        val = self.read_i2c_reg(R820T_I2C_ADDR, 0x0C).await?;
        val = (val & 0xF0) | vga_gain;
        self.write_i2c_reg(R820T_I2C_ADDR, 0x0C, val).await?;

        self.gain = gain_tenth_db;
        log(&format!("RTL-SDR: gain set to {} tenths dB", gain_tenth_db));
        Ok(())
    }

    /// Reset the sample buffer / FIFO.
    pub async fn reset_buffer(&self) -> Result<(), JsValue> {
        self.write_reg(BLOCK_USB, USB_EPA_FIFO_CFG, &[0x10, 0x02])
            .await?;
        self.write_reg(BLOCK_USB, USB_EPA_FIFO_CFG, &[0x00, 0x00])
            .await?;
        Ok(())
    }

    /// Close the device.
    pub async fn close(&self) -> Result<(), JsValue> {
        JsFuture::from(self.device.release_interface(0)).await?;
        JsFuture::from(self.device.close()).await?;
        log("RTL-SDR: device closed");
        Ok(())
    }

    pub fn device(&self) -> &UsbDevice {
        &self.device
    }

    pub fn tuner_freq(&self) -> u32 {
        self.tuner_freq
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    // ── Private: Low-level vendor control transfers ───────────────────────

    /// Vendor control OUT (host → device).
    async fn ctrl_out(&self, value: u16, index: u16, data: &[u8]) -> Result<(), JsValue> {
        let setup = UsbControlTransferParameters::new(
            index,
            UsbRecipient::Device,
            0, // request = 0 for all RTL2832U vendor transfers
            UsbRequestType::Vendor,
            value,
        );
        let array = Uint8Array::from(data);
        let promise = self
            .device
            .control_transfer_out_with_u8_array(&setup, &array)?;
        JsFuture::from(promise).await?;
        Ok(())
    }

    /// Vendor control IN (device → host).
    async fn ctrl_in(&self, value: u16, index: u16, len: u16) -> Result<Vec<u8>, JsValue> {
        let setup = UsbControlTransferParameters::new(
            index,
            UsbRecipient::Device,
            0,
            UsbRequestType::Vendor,
            value,
        );
        let promise = self.device.control_transfer_in(&setup, len);
        let result: UsbInTransferResult = JsFuture::from(promise).await?.unchecked_into();
        if let Some(dv) = result.data() {
            let buf = dv.buffer();
            let arr = Uint8Array::new(&buf);
            let mut vec = vec![0u8; arr.length() as usize];
            arr.copy_to(&mut vec);
            Ok(vec)
        } else {
            Ok(vec![])
        }
    }

    // ── Private: USB block register access ─────────────────────────────────

    async fn write_reg(&self, block: u16, addr: u16, data: &[u8]) -> Result<(), JsValue> {
        self.ctrl_out(addr, block << 8, data).await
    }

    #[allow(dead_code)]
    async fn read_reg(&self, block: u16, addr: u16, len: u16) -> Result<Vec<u8>, JsValue> {
        self.ctrl_in(addr, block << 8, len).await
    }

    // ── Private: Demodulator register access ───────────────────────────────
    //
    // The demod index has a special base 0x0A OR'd in: (page << 8) | 0x0A.

    async fn demod_write_reg(
        &self,
        page: u16,
        addr: u16,
        val: u16,
        len: u8,
    ) -> Result<(), JsValue> {
        let real_addr = (addr << 8) | 0x20;
        let index = (page << 8) | DEMOD_I2C_BASE;
        let data = if len == 1 {
            vec![val as u8]
        } else {
            vec![(val >> 8) as u8, val as u8]
        };
        self.ctrl_out(real_addr, index, &data).await?;
        // Read status register to complete the write
        self.demod_read_reg(0x0A, 0x01).await?;
        Ok(())
    }

    async fn demod_read_reg(&self, page: u16, addr: u16) -> Result<u8, JsValue> {
        let real_addr = (addr << 8) | 0x20;
        let index = (page << 8) | DEMOD_I2C_BASE;
        let data = self.ctrl_in(real_addr, index, 1).await?;
        Ok(data.first().copied().unwrap_or(0))
    }

    // ── Private: I2C register access (for R820T tuner) ─────────────────────
    //
    // Uses the RTL2832U I2C passthrough: block index = 0x0600 (I2C block 6).
    // Data format for write: [register_address, value].
    // R820T always reads from register 0 and auto-increments; returned bytes
    // are bit-reversed.

    /// Enable the I2C repeater so the host can talk to the tuner.
    async fn enable_i2c_repeater(&self) -> Result<(), JsValue> {
        self.demod_write_reg(1, 0x01, 0x18, 1).await
    }

    async fn write_i2c_reg(
        &self,
        i2c_addr: u8,
        reg_addr: u8,
        value: u8,
    ) -> Result<(), JsValue> {
        self.ctrl_out(i2c_addr as u16, I2C_INDEX, &[reg_addr, value])
            .await
    }

    async fn read_i2c_reg(&self, i2c_addr: u8, reg_addr: u8) -> Result<u8, JsValue> {
        // R820T reads always start at register 0, so read (reg+1) bytes.
        let data = self
            .ctrl_in(i2c_addr as u16, I2C_INDEX, (reg_addr as u16) + 1)
            .await?;
        Ok(data
            .get(reg_addr as usize)
            .copied()
            .map(reverse_bits)
            .unwrap_or(0))
    }

    async fn write_i2c_regs(
        &self,
        i2c_addr: u8,
        start_reg: u8,
        values: &[u8],
    ) -> Result<(), JsValue> {
        for (i, &val) in values.iter().enumerate() {
            self.write_i2c_reg(i2c_addr, start_reg + i as u8, val)
                .await?;
        }
        Ok(())
    }

    // ── Private: RTL2832U initialization ───────────────────────────────────

    async fn init_rtl2832u(&self) -> Result<(), JsValue> {
        // USB block: enable and configure
        self.write_reg(BLOCK_USB, USB_SYSCTL, &[0x09]).await?;
        self.write_reg(BLOCK_USB, 0x2000, &[0x40]).await?;
        self.write_reg(BLOCK_USB, 0x2008, &[0x02]).await?;

        // EPA config
        self.write_reg(BLOCK_USB, USB_EPA_CTL, &[0x10, 0x02]).await?;
        self.write_reg(BLOCK_USB, USB_EPA_MAXPKT, &[0x00, 0x02]).await?;

        // Demod init
        self.demod_write_reg(1, 0x01, 0x14, 1).await?; // reset
        self.demod_write_reg(1, 0x01, 0x10, 1).await?; // out of reset

        // Disable spectrum inversion in zero-IF
        self.demod_write_reg(0, 0x0D, 0x83, 1).await?;

        // Set ADC parameters
        self.demod_write_reg(1, 0x1B, 0x00, 1).await?;

        // Enable I2C repeater for tuner access
        self.demod_write_reg(1, 0x01, 0x18, 1).await?;

        // Set FIR coefficients (default)
        self.demod_write_reg(1, 0x06, 0x00, 1).await?;

        // AGC settings
        self.demod_write_reg(0, 0x19, 0x25, 1).await?;

        // Enable zero-IF mode
        self.demod_write_reg(1, 0xB1, 0x1A, 1).await?;

        // Set IF frequency to 0
        self.set_if_freq(0).await?;

        // Enable I2C repeater
        self.demod_write_reg(1, 0x01, 0x18, 1).await?;

        Ok(())
    }

    async fn set_if_freq(&self, freq: u32) -> Result<(), JsValue> {
        let if_freq = if freq == 0 {
            0u32
        } else {
            // Calculate IF register value
            ((freq as f64 * (1u64 << 22) as f64 / R820T_REF_FREQ as f64) as u32) & 0x3FFFFF
        };
        self.demod_write_reg(1, 0x19, (if_freq >> 16) as u16, 1)
            .await?;
        self.demod_write_reg(1, 0x1A, (if_freq >> 8) as u16, 1)
            .await?;
        self.demod_write_reg(1, 0x1B, if_freq as u16, 1).await?;
        Ok(())
    }

    // ── Private: R820T tuner initialization ────────────────────────────────

    async fn init_r820t(&self) -> Result<(), JsValue> {
        // Enable I2C repeater so we can talk to the tuner
        self.enable_i2c_repeater().await?;

        // Write init register values
        self.write_i2c_regs(R820T_I2C_ADDR, 0x05, &R820T_INIT_REGS)
            .await?;

        // Set initial frequency (will be overridden)
        self.set_r820t_freq(100_000_000).await?;

        Ok(())
    }

    async fn set_r820t_freq(&self, freq: u32) -> Result<(), JsValue> {
        // Ensure I2C repeater is on (set_sample_rate may have turned it off)
        self.enable_i2c_repeater().await?;

        // R820T PLL calculation
        // Select LO divider based on frequency
        let (lo_div, div_num) = if freq < 50_000_000 {
            (128, 0u8)
        } else if freq < 100_000_000 {
            (64, 1)
        } else if freq < 200_000_000 {
            (32, 2)
        } else if freq < 400_000_000 {
            (16, 3)
        } else if freq < 800_000_000 {
            (8, 4)
        } else {
            (4, 5)
        };

        let vco_freq = freq as u64 * lo_div as u64;

        // PLL integer divider
        let nint = (vco_freq / (2 * R820T_REF_FREQ as u64)) as u32;
        let nfrac =
            ((vco_freq % (2 * R820T_REF_FREQ as u64)) * 65536 / (2 * R820T_REF_FREQ as u64))
                as u16;

        // Write divider select (reg 0x10)
        let mut reg10 = self.read_i2c_reg(R820T_I2C_ADDR, 0x10).await?;
        reg10 = (reg10 & 0x1F) | (div_num << 5);
        self.write_i2c_reg(R820T_I2C_ADDR, 0x10, reg10).await?;

        // Write PLL integer N (regs 0x14, 0x15)
        let ni = (nint - 13) as u8;
        self.write_i2c_reg(R820T_I2C_ADDR, 0x14, ni >> 1).await?;

        let mut reg15 = self.read_i2c_reg(R820T_I2C_ADDR, 0x15).await?;
        reg15 = (reg15 & 0xFE) | (ni & 0x01);
        self.write_i2c_reg(R820T_I2C_ADDR, 0x15, reg15).await?;

        // Write PLL fractional N (regs 0x16, 0x17)
        self.write_i2c_reg(R820T_I2C_ADDR, 0x16, (nfrac >> 8) as u8)
            .await?;
        self.write_i2c_reg(R820T_I2C_ADDR, 0x17, nfrac as u8)
            .await?;

        // Wait for PLL lock (check reg 0x02 bit 6)
        for _ in 0..10 {
            let reg02 = self.read_i2c_reg(R820T_I2C_ADDR, 0x02).await?;
            if reg02 & 0x40 != 0 {
                log("RTL-SDR: PLL locked");
                return Ok(());
            }
        }
        log("RTL-SDR: PLL lock timeout (may still work)");
        Ok(())
    }
}

/// Reverse bits in a byte.  R820T returns bit-reversed register data.
fn reverse_bits(b: u8) -> u8 {
    let mut v = b;
    v = ((v >> 1) & 0x55) | ((v & 0x55) << 1);
    v = ((v >> 2) & 0x33) | ((v & 0x33) << 2);
    v = (v >> 4) | (v << 4);
    v
}

fn log(msg: &str) {
    console::log_1(&JsValue::from_str(msg));
}
