//! WASM HLS player with AudioWorklet output.
//!
//! Uses `thread_local!` for the audio pipeline to avoid wasm-bindgen's
//! `Rc<WasmRefCell<>>` wrapper, which breaks with shared memory (atomics)
//! because `Rc` uses non-atomic reference counting.

use std::{cell::RefCell, time::Duration};

use kithara_audio::{Audio, AudioConfig, EventBus, web_audio::PcmRingBuffer};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::{Stream, ThreadPool};
use tracing::{debug, info, warn};
use url::Url;
use wasm_bindgen::prelude::*;

/// Buffer size for PCM transfer from decode pipeline to ring buffer.
const PCM_BUF_SIZE: usize = 4096;

thread_local! {
    /// Audio pipeline stored in thread-local to avoid Rc issues with shared memory.
    static AUDIO: RefCell<Option<Audio<Stream<Hls>>>> = const { RefCell::new(None) };
}

/// HLS audio player for the browser.
///
/// Holds ring buffer and playback state. The audio decode pipeline
/// is stored in a thread-local (see [`load_hls`]).
#[wasm_bindgen]
pub struct WasmPlayer {
    /// PCM ring buffer shared with AudioWorklet.
    ring: PcmRingBuffer,
    /// Whether playback is active.
    playing: bool,
    /// Reusable PCM buffer to avoid per-frame allocation.
    pcm_buf: Vec<f32>,
}

#[wasm_bindgen]
impl WasmPlayer {
    /// Create a new player instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        // Panic hook + tracing already set in #[wasm_bindgen(start)].
        info!("WasmPlayer created");

        Self {
            ring: PcmRingBuffer::with_defaults(2, 44100),
            playing: false,
            pcm_buf: vec![0.0f32; PCM_BUF_SIZE],
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
        let position = Duration::from_secs_f64(position_ms / 1000.0);
        debug!(position_ms, "Seek");
        self.ring.reset();
        AUDIO.with(|a| {
            if let Some(ref mut audio) = *a.borrow_mut() {
                if let Err(e) = audio.seek(position) {
                    warn!(?e, "Seek failed");
                }
            }
        });
    }

    /// Fill the PCM ring buffer with decoded samples.
    ///
    /// Called from JS in a loop (via `requestAnimationFrame`).
    /// Returns the number of samples written, or 0 if paused/EOF/ring full.
    ///
    /// Backpressure: reads from the decoder only as many samples as the ring
    /// buffer can accept. This prevents `position()` from racing ahead of
    /// actual audio output.
    pub fn fill_buffer(&mut self) -> u32 {
        if !self.playing {
            return 0;
        }

        // Backpressure: don't read more than the ring can accept.
        let space = self.ring.space_available() as usize;
        if space == 0 {
            return 0;
        }
        let read_len = space.min(self.pcm_buf.len());

        AUDIO.with(|a| {
            let mut a = a.borrow_mut();
            let Some(ref mut audio) = *a else {
                return 0;
            };

            let n = audio.read(&mut self.pcm_buf[..read_len]);
            if n == 0 {
                if audio.is_eof() {
                    info!("End of stream");
                    self.playing = false;
                }
                return 0;
            }
            self.ring.write(&self.pcm_buf[..n]) as u32
        })
    }

    /// Get current position in milliseconds.
    #[must_use]
    pub fn get_position_ms(&self) -> f64 {
        AUDIO.with(|a| {
            a.borrow()
                .as_ref()
                .map_or(0.0, |audio| audio.position().as_secs_f64() * 1000.0)
        })
    }

    /// Get total duration in milliseconds (0 if unknown).
    #[must_use]
    pub fn get_duration_ms(&self) -> f64 {
        AUDIO.with(|a| {
            a.borrow()
                .as_ref()
                .and_then(|audio| audio.duration())
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
        })
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
        AUDIO.with(|a| a.borrow().as_ref().map_or(false, |audio| audio.is_eof()))
    }

    /// Update ring buffer after load. Called from JS after `load_hls` resolves.
    pub fn update_ring(&mut self, channels: u32, sample_rate: u32) {
        info!(channels, sample_rate, "Updating ring buffer");
        self.ring = PcmRingBuffer::with_defaults(channels, sample_rate);
    }
}

/// Load an HLS playlist and create the decode pipeline.
///
/// Free async function (not a method) to avoid wasm-bindgen's Rc wrapper
/// which breaks with shared memory. Returns `[channels, sample_rate, duration_ms]`.
#[wasm_bindgen]
pub async fn load_hls(url: String) -> Result<Box<[f64]>, JsValue> {
    load_hls_inner(url, None).await
}

/// Load HLS with explicit media info hint (for test fixtures that use
/// formats not auto-detectable from HLS playlist metadata, e.g. WAV).
pub async fn load_hls_with_media_info(
    url: String,
    media_info: kithara_stream::MediaInfo,
) -> Result<Box<[f64]>, JsValue> {
    load_hls_inner(url, Some(media_info)).await
}

async fn load_hls_inner(
    url: String,
    media_info: Option<kithara_stream::MediaInfo>,
) -> Result<Box<[f64]>, JsValue> {
    info!(url = %url, "Loading HLS stream");

    let parsed_url: Url = url
        .parse()
        .map_err(|e| JsValue::from_str(&format!("Invalid URL: {e}")))?;

    let pool = ThreadPool::global();
    let bus = EventBus::new(128);

    let hls_config = HlsConfig::new(parsed_url)
        .with_thread_pool(pool)
        .with_events(bus)
        .with_ephemeral(true)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });

    let mut config = AudioConfig::<Hls>::new(hls_config);
    if let Some(info) = media_info {
        config = config.with_media_info(info);
    }

    info!("Creating Audio pipeline (Audio::new)...");
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .map_err(|e| JsValue::from_str(&format!("Failed to create audio pipeline: {e}")))?;
    info!("Audio pipeline created");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio format detected"
    );

    audio.preload();

    let duration_ms = audio.duration().map_or(0.0, |d| d.as_secs_f64() * 1000.0);

    if duration_ms > 0.0 {
        info!(duration_ms, "Duration known");
    }

    AUDIO.with(|a| {
        *a.borrow_mut() = Some(audio);
    });

    Ok(Box::new([
        f64::from(spec.channels),
        f64::from(spec.sample_rate),
        duration_ms,
    ]))
}
