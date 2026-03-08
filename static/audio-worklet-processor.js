// Radioband — AudioWorklet Processor
// Receives PCM Float32 chunks via its MessagePort and outputs them to the
// audio graph.  Uses a simple ring buffer to bridge the potentially bursty
// arrival of data from the DSP worker with the fixed 128-sample render
// quantum of the Web Audio API.

class RadiobandProcessor extends AudioWorkletProcessor {
    constructor() {
        super();

        // Ring buffer — 48 kHz × 0.5 s = 24 000 samples
        this.bufferSize = 24000;
        this.buffer = new Float32Array(this.bufferSize);
        this.writePos = 0;
        this.readPos = 0;
        this.count = 0; // number of samples available

        this.port.onmessage = (e) => {
            const data = e.data; // Float32Array
            if (!(data instanceof Float32Array)) return;

            for (let i = 0; i < data.length; i++) {
                if (this.count < this.bufferSize) {
                    this.buffer[this.writePos] = data[i];
                    this.writePos = (this.writePos + 1) % this.bufferSize;
                    this.count++;
                }
                // else: drop sample (buffer full)
            }
        };
    }

    process(_inputs, outputs, _parameters) {
        const output = outputs[0];
        if (!output || output.length === 0) return true;

        const channel = output[0]; // mono
        for (let i = 0; i < channel.length; i++) {
            if (this.count > 0) {
                channel[i] = this.buffer[this.readPos];
                this.readPos = (this.readPos + 1) % this.bufferSize;
                this.count--;
            } else {
                channel[i] = 0; // underrun — output silence
            }
        }

        // Copy to other channels if stereo output is expected
        for (let ch = 1; ch < output.length; ch++) {
            output[ch].set(channel);
        }

        return true;
    }
}

registerProcessor('radioband-processor', RadiobandProcessor);
