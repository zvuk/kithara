//! WASM HLS player with AudioWorklet output.

use kithara_audio::web_audio::PcmRingBuffer;
use tracing::{debug, info};
use wasm_bindgen::prelude::*;

/// HLS audio player for the browser.
///
/// Decodes HLS audio via Symphonia and outputs PCM through
/// a ring buffer readable by an AudioWorklet.
#[wasm_bindgen]
pub struct WasmPlayer {
    /// PCM ring buffer shared with AudioWorklet.
    ring: PcmRingBuffer,
    /// Whether playback is active.
    playing: bool,
    // Audio pipeline will be added when we wire up the full HLS pipeline.
    // For now, this is a skeleton that compiles.
}

#[wasm_bindgen]
impl WasmPlayer {
    /// Create a new player instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();
        tracing_wasm::set_as_global_default();

        info!("WasmPlayer created");

        Self {
            ring: PcmRingBuffer::with_defaults(2, 44100),
            playing: false,
        }
    }

    /// Start playback.
    pub fn play(&mut self) {
        self.playing = true;
        info!("Play");
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        self.playing = false;
        info!("Pause");
    }

    /// Seek to position in milliseconds.
    pub fn seek(&mut self, position_ms: f64) {
        debug!(position_ms, "Seek");
        self.ring.reset();
    }

    /// Fill the PCM ring buffer with decoded samples.
    ///
    /// Called from JS in a loop (via `requestAnimationFrame` or `setTimeout`).
    /// Returns the number of samples written, or 0 if paused/EOF.
    pub fn fill_buffer(&mut self) -> u32 {
        if !self.playing {
            return 0;
        }

        // TODO: wire up Audio<Stream<Hls>> pipeline.
        // For now, generate a test tone (440 Hz sine wave).
        let sample_rate = f64::from(self.ring.sample_rate());
        let channels = self.ring.channels();
        let mut buf = [0.0f32; 2048];
        let frames = buf.len() / channels as usize;

        for frame in 0..frames {
            #[expect(clippy::cast_precision_loss)]
            let t = frame as f64 / sample_rate;
            let sample = (t * 440.0 * 2.0 * std::f64::consts::PI).sin() as f32 * 0.3;
            for ch in 0..channels as usize {
                buf[frame * channels as usize + ch] = sample;
            }
        }

        self.ring.write(&buf) as u32
    }

    /// Get current position in milliseconds.
    #[must_use]
    pub fn get_position_ms(&self) -> f64 {
        0.0 // TODO: wire up audio.position()
    }

    /// Get total duration in milliseconds (0 if unknown).
    #[must_use]
    pub fn get_duration_ms(&self) -> f64 {
        0.0 // TODO: wire up audio.duration()
    }

    /// Get pointer to PCM ring buffer data (byte offset in WASM memory).
    #[must_use]
    pub fn ring_buf_ptr(&self) -> u32 {
        self.ring.buf_ptr() as u32
    }

    /// Get ring buffer capacity in samples.
    #[must_use]
    pub fn ring_capacity(&self) -> u32 {
        self.ring.capacity()
    }

    /// Get pointer to write_head atomic (byte offset in WASM memory).
    #[must_use]
    pub fn ring_write_head_ptr(&self) -> u32 {
        self.ring.write_head_ptr() as u32
    }

    /// Get pointer to read_head atomic (byte offset in WASM memory).
    #[must_use]
    pub fn ring_read_head_ptr(&self) -> u32 {
        self.ring.read_head_ptr() as u32
    }

    /// Get number of channels.
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.ring.channels()
    }

    /// Get sample rate.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.ring.sample_rate()
    }

    /// Check if playback is active.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Check if end of stream reached.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        false // TODO
    }
}
