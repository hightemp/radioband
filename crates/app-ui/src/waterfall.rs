//! Waterfall and spectrum canvas renderer.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

/// Renders the spectrum overlay and waterfall display on a canvas.
pub struct WaterfallRenderer {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    width: u32,
    height: u32,
    spectrum_height: u32,
    /// Off-screen canvas used to hold the raw waterfall ImageData before scaling.
    off_canvas: HtmlCanvasElement,
    off_ctx: CanvasRenderingContext2d,
    off_w: u32,
    off_h: u32,
    /// Center frequency in Hz (for labels).
    center_freq: f64,
    /// Sample rate in Hz (bandwidth = sample_rate).
    sample_rate: f64,
    /// Passband width in Hz for the current demodulation mode.
    /// WFM=240_000, NFM=16_000, AM=10_000.
    passband_hz: f64,
    /// Short mode label for the passband overlay (e.g. "WFM", "NFM", "AM").
    mode_label: String,
}

impl WaterfallRenderer {
    pub fn new(canvas: HtmlCanvasElement) -> Result<Self, JsValue> {
        let ctx: CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .ok_or("Failed to get 2d context")?
            .dyn_into()?;
        let width = canvas.width();
        let height = canvas.height();
        let spectrum_height = height / 4; // Top quarter for spectrum

        let doc = web_sys::window().unwrap().document().unwrap();
        let off_canvas: HtmlCanvasElement =
            doc.create_element("canvas")?.dyn_into()?;
        off_canvas.set_width(1);
        off_canvas.set_height(1);
        let off_ctx: CanvasRenderingContext2d = off_canvas
            .get_context("2d")?
            .ok_or("off-screen 2d ctx")?
            .dyn_into()?;

        Ok(WaterfallRenderer {
            canvas,
            ctx,
            width,
            height,
            spectrum_height,
            off_canvas,
            off_ctx,
            off_w: 0,
            off_h: 0,
            center_freq: 100_000_000.0,
            sample_rate: 2_400_000.0,
            passband_hz: 240_000.0,
            mode_label: "WFM".to_string(),
        })
    }

    /// Ensure the off-screen canvas matches the waterfall dimensions.
    fn ensure_offscreen(&mut self, w: u32, h: u32) {
        if self.off_w != w || self.off_h != h {
            self.off_canvas.set_width(w);
            self.off_canvas.set_height(h);
            self.off_w = w;
            self.off_h = h;
        }
    }

    /// Render spectrum line (top overlay) and waterfall image (bottom).
    pub fn render(
        &mut self,
        spectrum: &[f32],
        waterfall_rgba: &[u8],
        wf_width: u32,
        wf_height: u32,
    ) -> Result<(), JsValue> {
        let w = self.width as f64;
        let h = self.height as f64;
        let sh = self.spectrum_height as f64;

        // Clear canvas
        self.ctx.set_fill_style_str("#000000");
        self.ctx.fill_rect(0.0, 0.0, w, h);

        // ── Draw waterfall (bottom part) via offscreen canvas + drawImage scaling
        if !waterfall_rgba.is_empty() && wf_width > 0 && wf_height > 0 {
            let expected_len = (wf_width * wf_height * 4) as usize;
            if waterfall_rgba.len() == expected_len {
                self.ensure_offscreen(wf_width, wf_height);

                let clamped = wasm_bindgen::Clamped(waterfall_rgba);
                if let Ok(img_data) =
                    ImageData::new_with_u8_clamped_array(clamped, wf_width)
                {
                    self.off_ctx.put_image_data(&img_data, 0, 0)?;

                    // Scale from (wf_width × wf_height) to fill the bottom of the main canvas
                    self.ctx
                        .draw_image_with_html_canvas_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                            &self.off_canvas,
                            0.0,
                            0.0,
                            wf_width as f64,
                            wf_height as f64,
                            0.0,
                            sh,
                            w,
                            h - sh,
                        )?;
                }
            }
        }

        // ── Draw spectrum overlay (top part) ───────────────────────────
        if !spectrum.is_empty() {
            let bin_count = spectrum.len();
            let x_scale = w / bin_count as f64;
            let min_db = -60.0_f64;
            let max_db = 0.0_f64;
            let db_range = max_db - min_db;

            // Background
            self.ctx.set_fill_style_str("rgba(0,0,0,0.7)");
            self.ctx.fill_rect(0.0, 0.0, w, sh);

            // Spectrum line
            self.ctx.begin_path();
            self.ctx.set_stroke_style_str("#00ff88");
            self.ctx.set_line_width(1.5);

            for (i, &db) in spectrum.iter().enumerate() {
                let x = i as f64 * x_scale;
                let norm = ((db as f64 - min_db) / db_range).clamp(0.0, 1.0);
                let y = sh * (1.0 - norm);

                if i == 0 {
                    self.ctx.move_to(x, y);
                } else {
                    self.ctx.line_to(x, y);
                }
            }
            self.ctx.stroke();

            // Grid lines (dB)
            self.ctx.set_stroke_style_str("rgba(255,255,255,0.15)");
            self.ctx.set_line_width(0.5);
            for db_line in [-50.0, -40.0, -30.0, -20.0, -10.0] {
                let norm = ((db_line - min_db) / db_range).clamp(0.0, 1.0);
                let y = sh * (1.0 - norm);
                self.ctx.begin_path();
                self.ctx.move_to(0.0, y);
                self.ctx.line_to(w, y);
                self.ctx.stroke();

                // Label
                self.ctx.set_fill_style_str("rgba(255,255,255,0.5)");
                self.ctx
                    .set_font("10px monospace");
                self.ctx
                    .fill_text(&format!("{:.0} dB", db_line), 4.0, y - 2.0)?;
            }

            // ── Frequency labels along bottom of spectrum area ─────────
            self.draw_frequency_labels(w, sh)?;

            // ── Passband overlay (center + boundary lines + shading) ───
            self.draw_passband_overlay(w, sh)?;
        }

        Ok(())
    }

    /// Draw frequency grid lines and labels on the spectrum area.
    fn draw_frequency_labels(&self, w: f64, sh: f64) -> Result<(), JsValue> {
        let half_bw = self.sample_rate / 2.0;
        let f_left = self.center_freq - half_bw;
        let f_right = self.center_freq + half_bw;

        // Choose a nice step based on bandwidth
        let step = nice_freq_step(self.sample_rate);

        // First grid line at or above f_left, aligned to step
        let first = (f_left / step).ceil() * step;

        self.ctx.set_stroke_style_str("rgba(255,255,255,0.12)");
        self.ctx.set_line_width(0.5);
        self.ctx.set_fill_style_str("rgba(255,255,255,0.7)");
        self.ctx.set_font("10px monospace");
        self.ctx.set_text_align("center");

        let mut f = first;
        while f <= f_right {
            let x = ((f - f_left) / self.sample_rate) * w;
            // Vertical grid line
            self.ctx.begin_path();
            self.ctx.move_to(x, 0.0);
            self.ctx.line_to(x, sh);
            self.ctx.stroke();

            // Frequency label
            let label = format_freq(f);
            self.ctx.fill_text(&label, x, sh - 3.0)?;

            f += step;
        }

        // Center marker (small triangle at bottom — complements the passband center line)
        let cx = w / 2.0;
        self.ctx.set_fill_style_str("rgba(255,100,100,0.8)");
        self.ctx.begin_path();
        self.ctx.move_to(cx - 4.0, sh);
        self.ctx.line_to(cx + 4.0, sh);
        self.ctx.line_to(cx, sh - 6.0);
        self.ctx.close_path();
        self.ctx.fill();

        // Reset text align
        self.ctx.set_text_align("start");

        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.canvas.set_width(width);
        self.canvas.set_height(height);
        self.width = width;
        self.height = height;
        self.spectrum_height = height / 4;
    }

    /// Draw the passband overlay: semi-transparent fill + dashed boundary
    /// lines + solid center line + mode label.
    fn draw_passband_overlay(&self, w: f64, sh: f64) -> Result<(), JsValue> {
        let half_bw = self.sample_rate / 2.0;
        let f_left = self.center_freq - half_bw;
        let half_pb = self.passband_hz / 2.0;

        // Pixel positions for passband edges and center
        let x_center = ((self.center_freq - f_left) / self.sample_rate) * w;
        let x_lo = ((self.center_freq - half_pb - f_left) / self.sample_rate) * w;
        let x_hi = ((self.center_freq + half_pb - f_left) / self.sample_rate) * w;

        // ── Semi-transparent filled rectangle ──────────────────────────
        self.ctx.set_fill_style_str("rgba(88,166,255,0.12)");
        self.ctx.fill_rect(x_lo, 0.0, x_hi - x_lo, sh);

        // ── Dashed boundary lines (left + right edges) ────────────────
        let dash_arr = js_sys::Array::new();
        dash_arr.push(&JsValue::from(4.0));
        dash_arr.push(&JsValue::from(3.0));
        self.ctx.set_line_dash(&dash_arr)?;
        self.ctx.set_stroke_style_str("rgba(88,166,255,0.6)");
        self.ctx.set_line_width(1.0);

        // Left boundary
        self.ctx.begin_path();
        self.ctx.move_to(x_lo, 0.0);
        self.ctx.line_to(x_lo, sh);
        self.ctx.stroke();

        // Right boundary
        self.ctx.begin_path();
        self.ctx.move_to(x_hi, 0.0);
        self.ctx.line_to(x_hi, sh);
        self.ctx.stroke();

        // Reset dash pattern
        let empty = js_sys::Array::new();
        self.ctx.set_line_dash(&empty)?;

        // ── Solid center line ──────────────────────────────────────────
        self.ctx.set_stroke_style_str("rgba(255,100,100,0.8)");
        self.ctx.set_line_width(1.5);
        self.ctx.begin_path();
        self.ctx.move_to(x_center, 0.0);
        self.ctx.line_to(x_center, sh);
        self.ctx.stroke();

        // ── Mode + bandwidth label ─────────────────────────────────────
        let half_khz = half_pb / 1000.0;
        let label = if half_khz >= 1.0 {
            format!("{} \u{00B1}{:.0}k", self.mode_label, half_khz)
        } else {
            format!("{} \u{00B1}{:.1}k", self.mode_label, half_khz)
        };
        self.ctx.set_fill_style_str("rgba(88,166,255,0.85)");
        self.ctx.set_font("11px monospace");
        self.ctx.set_text_align("center");
        self.ctx.fill_text(&label, x_center, 12.0)?;
        self.ctx.set_text_align("start");

        Ok(())
    }

    /// Update the frequency information for labels.
    pub fn set_frequency_info(&mut self, center_freq: f64, sample_rate: f64) {
        self.center_freq = center_freq;
        self.sample_rate = sample_rate;
    }

    /// Set the passband width for the current demodulation mode.
    pub fn set_mode_bandwidth(&mut self, passband_hz: f64, mode_label: &str) {
        self.passband_hz = passband_hz;
        self.mode_label = mode_label.to_string();
    }
}

/// Choose a nice frequency step for grid lines based on bandwidth.
fn nice_freq_step(bandwidth: f64) -> f64 {
    // We want roughly 5-10 grid lines across the display
    let rough_step = bandwidth / 8.0;
    let magnitude = 10.0_f64.powf(rough_step.log10().floor());
    let normalized = rough_step / magnitude;
    let nice = if normalized < 1.5 {
        1.0
    } else if normalized < 3.5 {
        2.0
    } else if normalized < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// Format frequency as a human-readable string.
fn format_freq(hz: f64) -> String {
    let mhz = hz / 1_000_000.0;
    if mhz.fract().abs() < 0.0005 {
        format!("{:.0} MHz", mhz)
    } else if (mhz * 10.0).fract().abs() < 0.005 {
        format!("{:.1}", mhz)
    } else {
        format!("{:.2}", mhz)
    }
}
