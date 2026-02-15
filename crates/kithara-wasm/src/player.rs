//! WASM HLS player with AudioWorklet output.

use std::time::Duration;

use kithara_audio::{Audio, AudioConfig, EventBus, web_audio::PcmRingBuffer};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::{Stream, ThreadPool};
use tracing::{debug, info, warn};
use url::Url;
use wasm_bindgen::prelude::*;

/// Buffer size for PCM transfer from decode pipeline to ring buffer.
const PCM_BUF_SIZE: usize = 4096;

/// HLS audio player for the browser.
///
/// Decodes HLS audio via Symphonia and outputs PCM through
/// a ring buffer readable by an AudioWorklet.
#[wasm_bindgen]
pub struct WasmPlayer {
    /// PCM ring buffer shared with AudioWorklet.
    ring: PcmRingBuffer,
    /// Audio decode pipeline (set after `load()`).
    audio: Option<Audio<Stream<Hls>>>,
    /// Whether playback is active.
    playing: bool,
    /// Reusable PCM buffer to avoid per-frame allocation.
    pcm_buf: Vec<f32>,
    /// Event bus shared with HLS pipeline.
    bus: EventBus,
}

#[wasm_bindgen]
impl WasmPlayer {
    /// Create a new player instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();
        tracing_wasm::set_as_global_default();

        info!("WasmPlayer created");

        let bus = EventBus::new(128);

        Self {
            ring: PcmRingBuffer::with_defaults(2, 44100),
            audio: None,
            playing: false,
            pcm_buf: vec![0.0f32; PCM_BUF_SIZE],
            bus,
        }
    }

    /// Load an HLS playlist URL. Returns a Promise.
    ///
    /// Creates the full decode pipeline: HLS fetch -> Symphonia decode -> PCM.
    pub async fn load(&mut self, url: String) -> Result<(), JsValue> {
        info!(url = %url, "Loading HLS stream");

        let parsed_url: Url = url
            .parse()
            .map_err(|e| JsValue::from_str(&format!("Invalid URL: {e}")))?;

        let pool = ThreadPool::global();
        let bus = self.bus.clone();

        let hls_config = HlsConfig::new(parsed_url)
            .with_thread_pool(pool)
            .with_events(bus)
            .with_ephemeral(true)
            .with_abr(AbrOptions {
                mode: AbrMode::Auto(Some(0)),
                ..Default::default()
            });

        let config = AudioConfig::<Hls>::new(hls_config);

        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .map_err(|e| JsValue::from_str(&format!("Failed to create audio pipeline: {e}")))?;

        // Update ring buffer to match detected audio format.
        let spec = audio.spec();
        info!(
            sample_rate = spec.sample_rate,
            channels = spec.channels,
            "Audio format detected"
        );
        self.ring = PcmRingBuffer::with_defaults(u32::from(spec.channels), spec.sample_rate);

        // Enable non-blocking reads (after preload, read uses try_recv).
        audio.preload();

        if let Some(dur) = audio.duration() {
            info!(duration_secs = dur.as_secs_f64(), "Duration known");
        }

        self.audio = Some(audio);
        Ok(())
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
        let position = Duration::from_secs_f64(position_ms / 1000.0);
        debug!(position_ms, "Seek");
        self.ring.reset();
        if let Some(ref mut audio) = self.audio {
            if let Err(e) = audio.seek(position) {
                warn!(?e, "Seek failed");
            }
        }
    }

    /// Fill the PCM ring buffer with decoded samples.
    ///
    /// Called from JS in a loop (via `requestAnimationFrame` or `setTimeout`).
    /// Returns the number of samples written, or 0 if paused/EOF.
    pub fn fill_buffer(&mut self) -> u32 {
        if !self.playing {
            return 0;
        }
        let Some(ref mut audio) = self.audio else {
            return 0;
        };

        let n = audio.read(&mut self.pcm_buf);
        if n == 0 {
            if audio.is_eof() {
                info!("End of stream");
                self.playing = false;
            }
            return 0;
        }
        self.ring.write(&self.pcm_buf[..n]) as u32
    }

    /// Get current position in milliseconds.
    #[must_use]
    pub fn get_position_ms(&self) -> f64 {
        self.audio
            .as_ref()
            .map_or(0.0, |a| a.position().as_secs_f64() * 1000.0)
    }

    /// Get total duration in milliseconds (0 if unknown).
    #[must_use]
    pub fn get_duration_ms(&self) -> f64 {
        self.audio
            .as_ref()
            .and_then(|a| a.duration())
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
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
        self.audio.as_ref().map_or(false, |a| a.is_eof())
    }
}
