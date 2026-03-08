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

        Ok(WaterfallRenderer {
            canvas,
            ctx,
            width,
            height,
            spectrum_height,
        })
    }

    /// Render spectrum line (top overlay) and waterfall image (bottom).
    pub fn render(
        &self,
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

        // ── Draw waterfall (bottom part) ───────────────────────────────
        if !waterfall_rgba.is_empty() && wf_width > 0 && wf_height > 0 {
            let clamped = wasm_bindgen::Clamped(waterfall_rgba);
            if let Ok(img_data) = ImageData::new_with_u8_clamped_array(clamped, wf_width) {
                // Scale waterfall to fill bottom of canvas
                // We need to put it on an offscreen bitmap or use drawImage
                // For simplicity, use putImageData at native resolution
                // and let CSS scaling handle the rest.
                let wf_top = sh;
                let wf_canvas_height = h - sh;

                // Draw row by row scaled
                let y_scale = wf_canvas_height / wf_height as f64;
                let x_scale = w / wf_width as f64;

                // For performance, just putImageData without scaling
                // Then use CSS to stretch the canvas
                self.ctx.put_image_data(&img_data, 0, sh as i32)?;
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
        }

        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.canvas.set_width(width);
        self.canvas.set_height(height);
        self.width = width;
        self.height = height;
        self.spectrum_height = height / 4;
    }
}
