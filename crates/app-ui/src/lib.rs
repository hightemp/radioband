//! Radioband — Browser-based RTL-SDR receiver.
//!
//! Main UI entry point: sets up DOM, WebUSB, Web Worker, and Audio.

mod waterfall;

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    console, Document, Event, HtmlButtonElement, HtmlCanvasElement,
    HtmlInputElement, HtmlSelectElement, MessageEvent, Worker,
};

use audio_bridge::AudioBridge;
use usb_rtl::{request_device, RtlSdr, RtlSdrConfig};
use waterfall::WaterfallRenderer;

// ── Shared State ───────────────────────────────────────────────────────────

/// UI-only state that never touches async.  `sdr` lives in its own RefCell
/// so we can borrow it independently and never hold the main state lock
/// across an `.await`.
struct AppState {
    worker: Worker,
    audio: Option<AudioBridge>,
    renderer: Option<WaterfallRenderer>,
    running: bool,
    mock_mode: bool,
    frequency: u32,
    gain: i32,
    mode: String,
    sample_rate: u32,
    fft_size: u32,
}

/// SDR device lives in its own cell so it can be borrowed independently
/// from the UI state.  This prevents `RefCell already borrowed` panics
/// when an async USB transfer yields back to the event loop.
type SdrCell = Rc<RefCell<Option<RtlSdr>>>;

// ── Entry Point ────────────────────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn main() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    // Don't initialise UI if we're in a worker context
    let global = js_sys::global();
    if !Reflect::has(&global, &"document".into()).unwrap_or(false) {
        return Ok(());
    }

    let document = web_sys::window()
        .ok_or("no window")?
        .document()
        .ok_or("no document")?;

    build_ui(&document)?;
    init_app(&document)?;

    console::log_1(&"Radioband: UI initialized".into());
    Ok(())
}

// ── UI Construction ────────────────────────────────────────────────────────

fn build_ui(doc: &Document) -> Result<(), JsValue> {
    let app = doc.get_element_by_id("app").ok_or("no #app element")?;
    app.set_inner_html(
        r#"
        <header>
            <h1>📻 Radioband</h1>
            <span id="status" class="status">Disconnected</span>
        </header>
        <div class="controls">
            <div class="control-group">
                <button id="btn-connect" class="btn btn-primary">Connect RTL-SDR</button>
                <button id="btn-mock" class="btn btn-secondary">Mock Mode</button>
                <button id="btn-disconnect" class="btn btn-danger" disabled>Disconnect</button>
            </div>
            <div class="control-group">
                <label>Frequency (MHz)
                    <input id="input-freq" type="number" value="100.0" step="0.1" min="24" max="1766" />
                </label>
                <button id="btn-set-freq" class="btn">Set</button>
            </div>
            <div class="control-group">
                <label>Gain
                    <input id="input-gain" type="range" min="0" max="500" value="0" step="10" />
                    <span id="gain-value">Auto</span>
                </label>
            </div>
            <div class="control-group">
                <label>Mode
                    <select id="select-mode">
                        <option value="wfm" selected>WFM</option>
                        <option value="nfm">NFM</option>
                        <option value="am">AM</option>
                    </select>
                </label>
            </div>
            <div class="control-group">
                <button id="btn-play" class="btn btn-primary" disabled>▶ Play</button>
                <button id="btn-stop" class="btn" disabled>⏹ Stop</button>
            </div>
        </div>
        <div class="display-container">
            <canvas id="waterfall-canvas" width="1024" height="600"></canvas>
        </div>
        <div id="error-bar" class="error-bar" style="display:none"></div>
    "#,
    );
    Ok(())
}

// ── App Initialization ─────────────────────────────────────────────────────

fn init_app(doc: &Document) -> Result<(), JsValue> {
    // Create Web Worker for DSP
    let worker = Worker::new("./worker.js")?;

    let state = Rc::new(RefCell::new(AppState {
        worker,
        audio: None,
        renderer: None,
        running: false,
        mock_mode: false,
        frequency: 100_000_000,
        gain: 0,
        mode: "wfm".to_string(),
        sample_rate: 2_400_000,
        fft_size: 2048,
    }));

    let sdr_cell: SdrCell = Rc::new(RefCell::new(None));

    // Set up waterfall renderer
    {
        let canvas: HtmlCanvasElement = doc
            .get_element_by_id("waterfall-canvas")
            .ok_or("no canvas")?
            .dyn_into()?;
        let renderer = WaterfallRenderer::new(canvas)?;
        state.borrow_mut().renderer = Some(renderer);
    }

    // ── Worker message handler ─────────────────────────────────────────
    {
        let state_c = state.clone();
        let handler = Closure::wrap(Box::new(move |event: MessageEvent| {
            let data = event.data();
            if data.is_object() {
                let obj: &Object = data.unchecked_ref();

                // Check for spectrum data
                if let Ok(spectrum_val) = Reflect::get(obj, &"spectrum".into()) {
                    if !spectrum_val.is_undefined() {
                        let spectrum: Float32Array = spectrum_val.unchecked_into();
                        let mut spec_vec = vec![0.0f32; spectrum.length() as usize];
                        spectrum.copy_to(&mut spec_vec);

                        // Audio data
                        if let Ok(audio_val) = Reflect::get(obj, &"audio".into()) {
                            if !audio_val.is_undefined() {
                                let audio: Float32Array = audio_val.unchecked_into();
                                let mut audio_vec = vec![0.0f32; audio.length() as usize];
                                audio.copy_to(&mut audio_vec);

                                let s = state_c.borrow();
                                if let Some(ref ab) = s.audio {
                                    let _ = ab.feed_pcm(&audio_vec);
                                }
                            }
                        }

                        // Waterfall data
                        let wf_data = Reflect::get(obj, &"waterfall".into()).ok();
                        let wf_width = Reflect::get(obj, &"wfWidth".into())
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0) as u32;
                        let wf_height = Reflect::get(obj, &"wfHeight".into())
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0) as u32;

                        let mut s = state_c.borrow_mut();
                        if let Some(ref mut renderer) = s.renderer {
                            let mut wf_vec = Vec::new();
                            if let Some(ref wf_val) = wf_data {
                                if !wf_val.is_undefined() {
                                    let wf_arr: Uint8Array = wf_val.clone().unchecked_into();
                                    wf_vec = vec![0u8; wf_arr.length() as usize];
                                    wf_arr.copy_to(&mut wf_vec);
                                }
                            }
                            let _ = renderer.render(&spec_vec, &wf_vec, wf_width, wf_height);
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(_)>);
        state.borrow().worker.set_onmessage(Some(handler.as_ref().unchecked_ref()));
        handler.forget();
    }

    // ── Connect button ─────────────────────────────────────────────────
    {
        let state_c = state.clone();
        let sdr_c = sdr_cell.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            let state_cc = state_c.clone();
            let sdr_cc = sdr_c.clone();
            let doc_cc = doc_c.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match connect_device(&state_cc, &sdr_cc, &doc_cc).await {
                    Ok(_) => set_status(&doc_cc, "Connected", "connected"),
                    Err(e) => show_error(&doc_cc, &format!("Connect failed: {:?}", e)),
                }
            });
        }) as Box<dyn FnMut(_)>);
        let btn: HtmlButtonElement = doc.get_element_by_id("btn-connect").unwrap().dyn_into()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Mock Mode button ───────────────────────────────────────────────
    {
        let state_c = state.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            let state_cc = state_c.clone();
            let doc_cc = doc_c.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match start_mock_mode(&state_cc, &doc_cc).await {
                    Ok(_) => set_status(&doc_cc, "Mock Mode", "mock"),
                    Err(e) => show_error(&doc_cc, &format!("Mock init failed: {:?}", e)),
                }
            });
        }) as Box<dyn FnMut(_)>);
        let btn: HtmlButtonElement = doc.get_element_by_id("btn-mock").unwrap().dyn_into()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Disconnect button ──────────────────────────────────────────────
    {
        let state_c = state.clone();
        let sdr_c = sdr_cell.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            let state_cc = state_c.clone();
            let sdr_cc = sdr_c.clone();
            let doc_cc = doc_c.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = disconnect(&state_cc, &sdr_cc, &doc_cc).await;
            });
        }) as Box<dyn FnMut(_)>);
        let btn: HtmlButtonElement = doc
            .get_element_by_id("btn-disconnect")
            .unwrap()
            .dyn_into()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Play button ────────────────────────────────────────────────────
    {
        let state_c = state.clone();
        let sdr_c = sdr_cell.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            let state_cc = state_c.clone();
            let sdr_cc = sdr_c.clone();
            let doc_cc = doc_c.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match start_streaming(&state_cc, &sdr_cc, &doc_cc).await {
                    Ok(_) => set_status(&doc_cc, "Streaming", "streaming"),
                    Err(e) => show_error(&doc_cc, &format!("Play failed: {:?}", e)),
                }
            });
        }) as Box<dyn FnMut(_)>);
        let btn: HtmlButtonElement = doc.get_element_by_id("btn-play").unwrap().dyn_into()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Stop button ────────────────────────────────────────────────────
    {
        let state_c = state.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            state_c.borrow_mut().running = false;
            set_status(&doc_c, "Stopped", "connected");
            set_btn_enabled(&doc_c, "btn-play", true);
            set_btn_enabled(&doc_c, "btn-stop", false);
        }) as Box<dyn FnMut(_)>);
        let btn: HtmlButtonElement = doc.get_element_by_id("btn-stop").unwrap().dyn_into()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Set Frequency button ───────────────────────────────────────────
    {
        let state_c = state.clone();
        let sdr_c = sdr_cell.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            let state_cc = state_c.clone();
            let sdr_cc = sdr_c.clone();
            let doc_cc = doc_c.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(input) = doc_cc.get_element_by_id("input-freq") {
                    let input: HtmlInputElement = input.unchecked_into();
                    if let Ok(mhz) = input.value().parse::<f64>() {
                        let freq = (mhz * 1_000_000.0) as u32;
                        state_cc.borrow_mut().frequency = freq;
                        // Take SDR out so no borrow spans the awaits
                        let sdr_opt = sdr_cc.borrow_mut().take();
                        if let Some(mut sdr) = sdr_opt {
                            let _ = sdr.set_center_freq(freq).await;
                            let _ = sdr.reset_buffer().await;
                            *sdr_cc.borrow_mut() = Some(sdr);
                        }
                    }
                }
            });
        }) as Box<dyn FnMut(_)>);
        let btn: HtmlButtonElement = doc
            .get_element_by_id("btn-set-freq")
            .unwrap()
            .dyn_into()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Gain slider ────────────────────────────────────────────────────
    {
        let state_c = state.clone();
        let doc_c = doc.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            if let Some(input) = doc_c.get_element_by_id("input-gain") {
                let input: HtmlInputElement = input.unchecked_into();
                if let Ok(val) = input.value().parse::<i32>() {
                    state_c.borrow_mut().gain = val;
                    if let Some(label) = doc_c.get_element_by_id("gain-value") {
                        let text = if val == 0 {
                            "Auto".to_string()
                        } else {
                            format!("{:.1} dB", val as f32 / 10.0)
                        };
                        label.set_text_content(Some(&text));
                    }
                }
            }
        }) as Box<dyn FnMut(_)>);
        let input: HtmlInputElement = doc
            .get_element_by_id("input-gain")
            .unwrap()
            .dyn_into()?;
        input.add_event_listener_with_callback("input", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // ── Mode selector ──────────────────────────────────────────────────
    {
        let state_c = state.clone();
        let handler = Closure::wrap(Box::new(move |_: Event| {
            let doc = web_sys::window().unwrap().document().unwrap();
            if let Some(sel) = doc.get_element_by_id("select-mode") {
                let sel: HtmlSelectElement = sel.unchecked_into();
                let mode = sel.value();
                let s = state_c.borrow();
                // Tell worker about mode change
                let msg = Object::new();
                let _ = Reflect::set(&msg, &"type".into(), &"set_mode".into());
                let _ = Reflect::set(&msg, &"mode".into(), &JsValue::from_str(&mode));
                let _ = s.worker.post_message(&msg);
                drop(s);
                state_c.borrow_mut().mode = mode;
            }
        }) as Box<dyn FnMut(_)>);
        let sel: HtmlSelectElement = doc
            .get_element_by_id("select-mode")
            .unwrap()
            .dyn_into()?;
        sel.add_event_listener_with_callback("change", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    Ok(())
}

// ── Device Connection ──────────────────────────────────────────────────────

async fn connect_device(
    state: &Rc<RefCell<AppState>>,
    sdr_cell: &SdrCell,
    doc: &Document,
) -> Result<(), JsValue> {
    let device = request_device().await?;
    let config = {
        let s = state.borrow();
        RtlSdrConfig {
            center_freq: s.frequency,
            sample_rate: s.sample_rate,
            gain: s.gain,
        }
    };
    let sdr = RtlSdr::new(device, &config).await?;

    // Init audio
    let mut audio = AudioBridge::new()?;
    audio.init("./audio-worklet-processor.js").await?;

    // Configure worker
    {
        let s = state.borrow();
        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &"configure".into())?;
        Reflect::set(&msg, &"sampleRate".into(), &JsValue::from(s.sample_rate))?;
        Reflect::set(&msg, &"fftSize".into(), &JsValue::from(s.fft_size))?;
        Reflect::set(&msg, &"mode".into(), &JsValue::from_str(&s.mode))?;
        s.worker.post_message(&msg)?;
    }

    *sdr_cell.borrow_mut() = Some(sdr);
    state.borrow_mut().audio = Some(audio);
    state.borrow_mut().mock_mode = false;

    set_btn_enabled(doc, "btn-connect", false);
    set_btn_enabled(doc, "btn-mock", false);
    set_btn_enabled(doc, "btn-disconnect", true);
    set_btn_enabled(doc, "btn-play", true);

    Ok(())
}

async fn start_mock_mode(
    state: &Rc<RefCell<AppState>>,
    doc: &Document,
) -> Result<(), JsValue> {
    // Init audio
    let mut audio = AudioBridge::new()?;
    audio.init("./audio-worklet-processor.js").await?;

    // Configure worker
    {
        let s = state.borrow();
        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &"configure".into())?;
        Reflect::set(&msg, &"sampleRate".into(), &JsValue::from(s.sample_rate))?;
        Reflect::set(&msg, &"fftSize".into(), &JsValue::from(s.fft_size))?;
        Reflect::set(&msg, &"mode".into(), &JsValue::from_str(&s.mode))?;
        s.worker.post_message(&msg)?;
    }

    state.borrow_mut().audio = Some(audio);
    state.borrow_mut().mock_mode = true;

    set_btn_enabled(doc, "btn-connect", false);
    set_btn_enabled(doc, "btn-mock", false);
    set_btn_enabled(doc, "btn-disconnect", true);
    set_btn_enabled(doc, "btn-play", true);

    Ok(())
}

async fn disconnect(
    state: &Rc<RefCell<AppState>>,
    sdr_cell: &SdrCell,
    doc: &Document,
) -> Result<(), JsValue> {
    state.borrow_mut().running = false;

    // Take SDR out so no borrow is held across await
    let sdr_opt = sdr_cell.borrow_mut().take();
    if let Some(sdr) = sdr_opt {
        let _ = sdr.close().await;
    }

    // Take audio out so no borrow is held across await
    let audio_opt = state.borrow_mut().audio.take();
    if let Some(audio) = audio_opt {
        let _ = audio.suspend().await;
    }

    set_status(doc, "Disconnected", "disconnected");
    set_btn_enabled(doc, "btn-connect", true);
    set_btn_enabled(doc, "btn-mock", true);
    set_btn_enabled(doc, "btn-disconnect", false);
    set_btn_enabled(doc, "btn-play", false);
    set_btn_enabled(doc, "btn-stop", false);

    Ok(())
}

// ── Streaming ──────────────────────────────────────────────────────────────

async fn start_streaming(
    state: &Rc<RefCell<AppState>>,
    sdr_cell: &SdrCell,
    doc: &Document,
) -> Result<(), JsValue> {
    state.borrow_mut().running = true;

    // Take audio out temporarily so the borrow doesn't span the await
    let audio_opt = state.borrow_mut().audio.take();
    if let Some(ref audio) = audio_opt {
        audio.resume().await?;
    }
    state.borrow_mut().audio = audio_opt;

    set_btn_enabled(doc, "btn-play", false);
    set_btn_enabled(doc, "btn-stop", true);

    let state_c = state.clone();
    let sdr_c = sdr_cell.clone();
    wasm_bindgen_futures::spawn_local(async move {
        read_loop(state_c, sdr_c).await;
    });

    Ok(())
}

/// Main read loop: reads IQ from USB (or mock) and sends to worker.
///
/// Key invariant: we never hold a `RefCell` borrow across an `.await`.
/// For the USB path we *take* the SDR out of its cell, perform the
/// blocking read with full ownership, then put it back.
async fn read_loop(state: Rc<RefCell<AppState>>, sdr_cell: SdrCell) {
    console::log_1(&"read_loop: started".into());
    let mut frame_count: u32 = 0;
    loop {
        let is_running = state.borrow().running;
        if !is_running {
            console::log_1(&"read_loop: stopped (running=false)".into());
            break;
        }

        let is_mock = state.borrow().mock_mode;

        if is_mock {
            // Ask worker to generate mock data and process it
            {
                let s = state.borrow();
                let msg = Object::new();
                let _ = Reflect::set(&msg, &"type".into(), &"mock".into());
                let _ = Reflect::set(&msg, &"numBytes".into(), &JsValue::from(16384u32));
                let _ = s.worker.post_message(&msg);
            }
        } else {
            // Take SDR out so no borrow is held across the USB await
            let sdr_opt = sdr_cell.borrow_mut().take();
            if let Some(sdr) = sdr_opt {
                if frame_count == 0 {
                    console::log_1(&"read_loop: calling read_block...".into());
                }
                let result = sdr.read_block().await;
                // Put SDR back immediately
                *sdr_cell.borrow_mut() = Some(sdr);

                match result {
                    Ok(data) => {
                        frame_count += 1;
                        if frame_count <= 3 || frame_count % 100 == 0 {
                            console::log_1(
                                &format!(
                                    "read_loop: frame {} — {} bytes read",
                                    frame_count,
                                    data.len()
                                )
                                .into(),
                            );
                        }
                        let s = state.borrow();
                        let array = Uint8Array::from(data.as_slice());
                        let msg = Object::new();
                        let _ = Reflect::set(&msg, &"type".into(), &"iq_data".into());
                        let _ = Reflect::set(&msg, &"data".into(), &array);
                        let _ = s.worker.post_message(&msg);
                    }
                    Err(e) => {
                        console::error_1(&format!("USB read error: {:?}", e).into());
                        state.borrow_mut().running = false;
                        break;
                    }
                }
            } else {
                console::error_1(&"read_loop: sdr_cell is None!".into());
                state.borrow_mut().running = false;
                break;
            }
        }

        // Yield to browser — roughly 30fps
        sleep_ms(16).await;
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn set_status(doc: &Document, text: &str, class: &str) {
    if let Some(el) = doc.get_element_by_id("status") {
        el.set_text_content(Some(text));
        el.set_class_name(&format!("status {}", class));
    }
}

fn show_error(doc: &Document, text: &str) {
    if let Some(el) = doc.get_element_by_id("error-bar") {
        el.set_text_content(Some(text));
        let _ = el.set_attribute("style", "display:block");
    }
    console::error_1(&JsValue::from_str(text));
}

fn set_btn_enabled(doc: &Document, id: &str, enabled: bool) {
    if let Some(el) = doc.get_element_by_id(id) {
        let btn: &HtmlButtonElement = el.unchecked_ref();
        btn.set_disabled(!enabled);
    }
}

async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let window = web_sys::window().unwrap();
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
    });
    let _ = JsFuture::from(promise).await;
}
