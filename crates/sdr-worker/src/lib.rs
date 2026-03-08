//! Web Worker entry-point for DSP processing.
//!
//! Receives raw IQ data from the main thread via postMessage,
//! runs the sdr-core DSP pipeline, and posts back spectrum + audio results.

use std::cell::RefCell;

use js_sys::{Float32Array, Object, Reflect, Uint8Array};
use sdr_core::{DemodMode, DspConfig, DspPipeline, MockIqSource};
use wasm_bindgen::prelude::*;
use web_sys::console;

thread_local! {
    static PIPELINE: RefCell<Option<DspPipeline>> = RefCell::new(None);
    static MOCK_SOURCE: RefCell<Option<MockIqSource>> = RefCell::new(None);
}

/// Called once from worker.js after WASM initializes.
#[wasm_bindgen]
pub fn init_worker() {
    console_error_panic_hook::set_once();
    console::log_1(&"sdr-worker: initialized".into());
}

/// Configure the DSP pipeline.
#[wasm_bindgen]
pub fn configure(sample_rate: u32, fft_size: u32, mode: &str) {
    let demod_mode = match mode {
        "nfm" => DemodMode::NFM,
        "am" => DemodMode::AM,
        _ => DemodMode::WFM,
    };

    let config = DspConfig {
        sample_rate,
        fft_size: fft_size as usize,
        mode: demod_mode,
        audio_rate: 48_000,
    };

    PIPELINE.with(|p| {
        *p.borrow_mut() = Some(DspPipeline::new(config));
    });

    MOCK_SOURCE.with(|m| {
        *m.borrow_mut() = Some(MockIqSource::new(sample_rate as f32));
    });

    console::log_1(
        &format!(
            "sdr-worker: configured — rate={}, fft={}, mode={}",
            sample_rate, fft_size, mode
        )
        .into(),
    );
}

/// Process a block of raw u8 IQ data.
/// Returns a JS object { spectrum: Float32Array, audio: Float32Array,
///                        waterfall: Uint8Array, wfWidth: number, wfHeight: number }
#[wasm_bindgen]
pub fn process_iq(data: &[u8]) -> JsValue {
    PIPELINE.with(|p| {
        let mut borrow = p.borrow_mut();
        if let Some(ref mut pipeline) = *borrow {
            let result = pipeline.process(data);

            let obj = Object::new();
            let spectrum = Float32Array::from(result.spectrum.as_slice());
            let audio = Float32Array::from(result.audio.as_slice());

            // Get waterfall image data
            let wf_data = pipeline.waterfall.get_ordered_rows();
            let wf_arr = Uint8Array::from(wf_data.as_slice());

            let _ = Reflect::set(&obj, &"spectrum".into(), &spectrum);
            let _ = Reflect::set(&obj, &"audio".into(), &audio);
            let _ = Reflect::set(&obj, &"waterfall".into(), &wf_arr);
            let _ = Reflect::set(
                &obj,
                &"wfWidth".into(),
                &JsValue::from(pipeline.waterfall.width as u32),
            );
            let _ = Reflect::set(
                &obj,
                &"wfHeight".into(),
                &JsValue::from(pipeline.waterfall.height as u32),
            );

            obj.into()
        } else {
            JsValue::NULL
        }
    })
}

/// Generate mock IQ data (for testing without hardware).
#[wasm_bindgen]
pub fn generate_mock_iq(num_bytes: u32) -> Uint8Array {
    MOCK_SOURCE.with(|m| {
        let mut borrow = m.borrow_mut();
        if let Some(ref mut source) = *borrow {
            let data = source.generate(num_bytes as usize);
            Uint8Array::from(data.as_slice())
        } else {
            Uint8Array::new_with_length(0)
        }
    })
}

/// Reconfigure demod mode without full re-init.
#[wasm_bindgen]
pub fn set_mode(mode: &str) {
    PIPELINE.with(|p| {
        let borrow = p.borrow();
        if let Some(ref pipeline) = *borrow {
            let config = &pipeline.config;
            let sample_rate = config.sample_rate;
            let fft_size = config.fft_size as u32;
            drop(borrow);
            configure(sample_rate, fft_size, mode);
        }
    });
}
