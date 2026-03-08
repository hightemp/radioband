// Radioband — DSP Web Worker
// Loads the sdr-worker WASM module (built with --target no-modules)
// and bridges postMessage ↔ Rust DSP pipeline.

/* global importScripts, wasm_bindgen, self */

importScripts('./worker-pkg/sdr_worker.js');

const wasm = wasm_bindgen;

async function start() {
    await wasm('./worker-pkg/sdr_worker_bg.wasm');
    wasm.init_worker();

    self.onmessage = function (e) {
        const msg = e.data;
        if (!msg || !msg.type) return;

        switch (msg.type) {
            case 'configure': {
                wasm.configure(
                    msg.sampleRate || 2400000,
                    msg.fftSize || 2048,
                    msg.mode || 'wfm',
                );
                break;
            }

            case 'iq_data': {
                // msg.data is Uint8Array of raw IQ from RTL-SDR
                const result = wasm.process_iq(msg.data);
                if (result) {
                    self.postMessage(result);
                }
                break;
            }

            case 'mock': {
                // Generate mock IQ data in Rust and process it
                const numBytes = msg.numBytes || 16384;
                const mockIq = wasm.generate_mock_iq(numBytes);
                const result = wasm.process_iq(mockIq);
                if (result) {
                    self.postMessage(result);
                }
                break;
            }

            case 'set_mode': {
                wasm.set_mode(msg.mode || 'wfm');
                break;
            }

            case 'clear_waterfall': {
                wasm.clear_waterfall();
                break;
            }

            default:
                console.warn('Worker: unknown message type', msg.type);
        }
    };

    self.postMessage({ type: 'ready' });
}

start().catch(function (err) {
    console.error('Worker init failed:', err);
    self.postMessage({ type: 'error', message: String(err) });
});
