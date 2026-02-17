// pcm-processor.js — AudioWorklet processor for Kithara WASM player.
//
// Reads interleaved f32 PCM from a ring buffer in SharedArrayBuffer
// (WASM linear memory). Synchronized via atomic write_head / read_head.

class PcmProcessor extends AudioWorkletProcessor {
    constructor(options) {
        super();
        this.initialized = false;
        this.port.onmessage = (e) => {
            if (e.data.type === 'init') {
                this.memory = e.data.memory;
                this.bufByteOffset = e.data.bufByteOffset;
                this.capacity = e.data.capacity;
                this.mask = this.capacity - 1;
                this.writeHeadByteOffset = e.data.writeHeadByteOffset;
                this.readHeadByteOffset = e.data.readHeadByteOffset;
                this.channels = e.data.channels;

                // Create typed array views over SharedArrayBuffer.
                this.headsView = new Int32Array(this.memory);
                this.samplesView = new Float32Array(this.memory);
                this.bufBaseIndex = this.bufByteOffset / 4;
                this.writeHeadIndex = this.writeHeadByteOffset / 4;
                this.readHeadIndex = this.readHeadByteOffset / 4;

                this.initialized = true;
            }
        };
    }

    process(inputs, outputs, parameters) {
        if (!this.initialized) {
            return true;
        }

        const output = outputs[0];
        const numOutputChannels = output.length;
        const frameCount = output[0].length; // typically 128

        // Read atomic heads.
        const writeHead = Atomics.load(this.headsView, this.writeHeadIndex);
        const readHead = Atomics.load(this.headsView, this.readHeadIndex);

        // Available samples (wrapping subtraction with unsigned semantics).
        const available = (writeHead - readHead) >>> 0;
        const samplesNeeded = frameCount * this.channels;
        const samplesToRead = Math.min(available, samplesNeeded);

        if (samplesToRead < this.channels) {
            // Underrun — output silence.
            for (let ch = 0; ch < numOutputChannels; ch++) {
                output[ch].fill(0);
            }
            return true;
        }

        // Deinterleave samples from ring buffer into output channels.
        const frames = Math.floor(samplesToRead / this.channels);

        for (let frame = 0; frame < frames; frame++) {
            for (let ch = 0; ch < Math.min(numOutputChannels, this.channels); ch++) {
                const sampleIdx = ((readHead + frame * this.channels + ch) & this.mask) >>> 0;
                output[ch][frame] = this.samplesView[this.bufBaseIndex + sampleIdx];
            }
        }

        // Fill remaining frames with silence (if underrun).
        for (let ch = 0; ch < numOutputChannels; ch++) {
            for (let i = frames; i < frameCount; i++) {
                output[ch][i] = 0;
            }
        }

        // Advance read head.
        const newReadHead = (readHead + frames * this.channels) >>> 0;
        Atomics.store(this.headsView, this.readHeadIndex, newReadHead);

        return true;
    }
}

registerProcessor('pcm-processor', PcmProcessor);
