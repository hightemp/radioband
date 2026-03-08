//! Audio output bridge using Web Audio API + AudioWorklet.
//!
//! The AudioWorklet processor is a thin JS file that receives
//! PCM chunks via its MessagePort and writes them to the output.

use js_sys::{Array, Float32Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioContext, AudioWorkletNode, AudioWorkletNodeOptions, GainNode, MessagePort};

/// Manages the Web Audio pipeline.
pub struct AudioBridge {
    ctx: AudioContext,
    worklet_node: Option<AudioWorkletNode>,
    port: Option<MessagePort>,
    gain_node: Option<GainNode>,
}

impl AudioBridge {
    /// Create a new AudioBridge. Call `init()` before use.
    pub fn new() -> Result<Self, JsValue> {
        let ctx = AudioContext::new()?;
        Ok(AudioBridge {
            ctx,
            worklet_node: None,
            port: None,
            gain_node: None,
        })
    }

    /// Initialise the AudioWorklet. Must be called after user gesture.
    /// `worklet_url` is the path to audio-worklet-processor.js.
    pub async fn init(&mut self, worklet_url: &str) -> Result<(), JsValue> {
        // Resume context (may be suspended due to autoplay policy)
        let _ = JsFuture::from(self.ctx.resume()?).await;

        // Load worklet module
        let worklet = self.ctx.audio_worklet()?;
        JsFuture::from(worklet.add_module(worklet_url)?).await?;

        // Create worklet node
        let options = AudioWorkletNodeOptions::new();
        let outputs = Array::of1(&JsValue::from(1)); // mono
        options.set_number_of_outputs(1);
        options.set_output_channel_count(&outputs);

        let node =
            AudioWorkletNode::new_with_options(&self.ctx, "radioband-processor", &options)?;

        // Create GainNode for volume control (default gain = 1.0)
        let gain_node = GainNode::new(&self.ctx)?;
        gain_node.gain().set_value(1.0);

        // Wire: AudioWorkletNode → GainNode → destination
        node.connect_with_audio_node(&gain_node)?;
        gain_node.connect_with_audio_node(&self.ctx.destination())?;

        web_sys::console::log_1(&"AudioBridge: GainNode inserted into audio chain".into());

        // Get MessagePort for sending PCM data
        let port = node.port()?;
        self.port = Some(port);
        self.worklet_node = Some(node);
        self.gain_node = Some(gain_node);

        web_sys::console::log_1(&"AudioBridge: worklet initialized".into());
        Ok(())
    }

    /// Feed PCM audio samples to the worklet.
    pub fn feed_pcm(&self, samples: &[f32]) -> Result<(), JsValue> {
        if let Some(ref port) = self.port {
            let array = Float32Array::from(samples);
            port.post_message(&array)?;
        }
        Ok(())
    }

    /// Resume playback (required after user gesture in some browsers).
    pub async fn resume(&self) -> Result<(), JsValue> {
        JsFuture::from(self.ctx.resume()?).await?;
        Ok(())
    }

    /// Suspend playback.
    pub async fn suspend(&self) -> Result<(), JsValue> {
        JsFuture::from(self.ctx.suspend()?).await?;
        Ok(())
    }

    pub fn sample_rate(&self) -> f32 {
        self.ctx.sample_rate()
    }

    pub fn is_ready(&self) -> bool {
        self.worklet_node.is_some()
    }

    /// Set the output volume. `value`: 0.0 = silence, 1.0 = normal, 2.0 = 2x amplified.
    pub fn set_volume(&self, value: f32) {
        let clamped = value.clamp(0.0, 2.0);
        if let Some(ref gain) = self.gain_node {
            gain.gain().set_value(clamped);
            web_sys::console::log_1(
                &format!("AudioBridge: volume set to {:.2}", clamped).into(),
            );
        }
    }

    /// Get the current volume (0.0–2.0).
    pub fn volume(&self) -> f32 {
        self.gain_node
            .as_ref()
            .map(|g| g.gain().value())
            .unwrap_or(1.0)
    }
}
