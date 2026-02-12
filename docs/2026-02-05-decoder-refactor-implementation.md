# Decoder Refactor Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Refactor kithara-decode to generic/trait-based architecture with packet-first approach, fixing seek issues.

**Architecture:** Replace monolithic `Decoder` with `AudioDecoder` trait + codec-specific implementations (`Symphonia<C>`). Factory pattern for runtime selection. No FormatReader/probe — direct packet readers per codec.

**Tech Stack:** Symphonia (codec readers + decoders), unimock (testing), thiserror (errors)

---

## Phase 1: Foundation (Types, Errors, Traits)

### Task 1: Create error module

**Files:**
- Create: `crates/kithara-decode/src/error.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Write the failing test**

```rust
// In crates/kithara-decode/src/error.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_decode_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let decode_err: DecodeError = io_err.into();
        assert!(matches!(decode_err, DecodeError::Io(_)));
    }

    #[test]
    fn test_decode_error_display() {
        let err = DecodeError::InvalidData("bad frame".into());
        assert_eq!(err.to_string(), "Invalid data: bad frame");
    }

    #[test]
    fn test_decode_error_backend_wraps_any_error() {
        let inner = io::Error::new(io::ErrorKind::Other, "symphonia error");
        let err = DecodeError::Backend(Box::new(inner));
        assert!(err.to_string().contains("Decoder error"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-decode error::tests --no-run 2>&1 | head -20`
Expected: Compilation error — module `error` not found

**Step 3: Write minimal implementation**

```rust
// crates/kithara-decode/src/error.rs
use kithara_stream::{AudioCodec, ContainerFormat};
use std::io;
use thiserror::Error;

/// Errors that can occur during audio decoding
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Seek failed: {0}")]
    SeekFailed(String),

    #[error("Probe failed: could not detect codec")]
    ProbeFailed,

    #[error("Decoder error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Result type for decode operations
pub type DecodeResult<T> = Result<T, DecodeError>;

#[cfg(test)]
mod tests {
    // ... tests from Step 1
}
```

**Step 4: Update lib.rs**

```rust
// Add to crates/kithara-decode/src/lib.rs
mod error;
pub use error::{DecodeError, DecodeResult};
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p kithara-decode error::tests -v`
Expected: 3 tests PASS

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/error.rs crates/kithara-decode/src/lib.rs
git commit -m "feat(decode): add DecodeError with Backend variant"
```

---

### Task 2: Create traits module with CodecType

**Files:**
- Create: `crates/kithara-decode/src/traits.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Write the failing test**

```rust
// In crates/kithara-decode/src/traits.rs
#[cfg(test)]
mod tests {
    use super::*;
    use kithara_stream::AudioCodec;

    #[test]
    fn test_aac_codec_type() {
        assert_eq!(Aac::CODEC, AudioCodec::Aac);
    }

    #[test]
    fn test_mp3_codec_type() {
        assert_eq!(Mp3::CODEC, AudioCodec::Mp3);
    }

    #[test]
    fn test_flac_codec_type() {
        assert_eq!(Flac::CODEC, AudioCodec::Flac);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-decode traits::tests --no-run 2>&1 | head -20`
Expected: Compilation error — module `traits` not found

**Step 3: Write minimal implementation**

```rust
// crates/kithara-decode/src/traits.rs
use crate::error::{DecodeError, DecodeResult};
use crate::types::{PcmChunk, PcmSpec};
use kithara_stream::AudioCodec;
use std::io::{Read, Seek};
use std::time::Duration;

/// Marker trait for codec types
pub trait CodecType: Send + 'static {
    /// The codec this type represents
    const CODEC: AudioCodec;
}

/// AAC codec marker
pub struct Aac;
impl CodecType for Aac {
    const CODEC: AudioCodec = AudioCodec::Aac;
}

/// MP3 codec marker
pub struct Mp3;
impl CodecType for Mp3 {
    const CODEC: AudioCodec = AudioCodec::Mp3;
}

/// FLAC codec marker
pub struct Flac;
impl CodecType for Flac {
    const CODEC: AudioCodec = AudioCodec::Flac;
}

/// ALAC codec marker
pub struct Alac;
impl CodecType for Alac {
    const CODEC: AudioCodec = AudioCodec::Alac;
}

/// Vorbis codec marker
pub struct Vorbis;
impl CodecType for Vorbis {
    const CODEC: AudioCodec = AudioCodec::Vorbis;
}

#[cfg(test)]
mod tests {
    // ... tests from Step 1
}
```

**Step 4: Update lib.rs**

```rust
// Add to crates/kithara-decode/src/lib.rs
mod traits;
pub use traits::{Aac, Alac, CodecType, Flac, Mp3, Vorbis};
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p kithara-decode traits::tests -v`
Expected: 3 tests PASS

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/traits.rs crates/kithara-decode/src/lib.rs
git commit -m "feat(decode): add CodecType marker trait and codec markers"
```

---

### Task 3: Add AudioDecoder trait

**Files:**
- Modify: `crates/kithara-decode/src/traits.rs`
- Modify: `crates/kithara-decode/src/lib.rs`
- Modify: `crates/kithara-decode/Cargo.toml`

**Step 1: Write the failing test**

```rust
// Add to crates/kithara-decode/src/traits.rs tests
#[test]
fn test_audio_decoder_trait_is_object_safe() {
    // This test verifies the trait can be used as dyn AudioDecoder
    fn _accepts_boxed(_: Box<dyn AudioDecoder<Config = ()>>) {}
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-decode traits::tests --no-run 2>&1 | head -20`
Expected: Compilation error — `AudioDecoder` not found

**Step 3: Write AudioDecoder trait**

```rust
// Add to crates/kithara-decode/src/traits.rs (after CodecType implementations)

/// Trait for all audio decoders (Symphonia, Apple, Android)
pub trait AudioDecoder: Send + 'static {
    /// Configuration type specific to this decoder implementation
    type Config: Default + Send;

    /// Create a new decoder from a Read + Seek source
    fn create<R>(source: R, config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + 'static,
        Self: Sized;

    /// Decode the next chunk of PCM data
    /// Returns None at end of stream
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>>;

    /// Get the PCM output specification
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position
    fn seek(&mut self, pos: Duration) -> DecodeResult<()>;

    /// Get current playback position
    fn position(&self) -> Duration;

    /// Get total duration if known
    fn duration(&self) -> Option<Duration> {
        None
    }
}
```

**Step 4: Update lib.rs exports**

```rust
// Update crates/kithara-decode/src/lib.rs
pub use traits::{Aac, Alac, AudioDecoder, CodecType, Flac, Mp3, Vorbis};
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p kithara-decode traits::tests -v`
Expected: 4 tests PASS

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/traits.rs crates/kithara-decode/src/lib.rs
git commit -m "feat(decode): add AudioDecoder trait with Config associated type"
```

---

## Phase 2: Symphonia Backend

### Task 4: Create symphonia module structure

**Files:**
- Create: `crates/kithara-decode/src/symphonia.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Write the failing test**

```rust
// In crates/kithara-decode/src/symphonia.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(config.gapless);
        assert!(config.byte_len_handle.is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-decode symphonia::tests --no-run 2>&1 | head -20`
Expected: Compilation error — module `symphonia` not found

**Step 3: Write SymphoniaConfig**

```rust
// crates/kithara-decode/src/symphonia.rs
use crate::error::{DecodeError, DecodeResult};
use crate::traits::{Aac, AudioDecoder, CodecType, Flac, Mp3, Vorbis};
use crate::types::{PcmChunk, PcmSpec};
use std::io::{Read, Seek};
use std::marker::PhantomData;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

/// Configuration for Symphonia-based decoders
#[derive(Debug, Clone, Default)]
pub struct SymphoniaConfig {
    /// Enable data verification (slower but safer)
    pub verify: bool,
    /// Enable gapless playback
    pub gapless: bool,
    /// Handle for dynamic byte length updates (HLS)
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(config.gapless);
        assert!(config.byte_len_handle.is_none());
    }
}
```

Wait — default for `gapless` should be `true`. Update Default impl:

```rust
impl Default for SymphoniaConfig {
    fn default() -> Self {
        Self {
            verify: false,
            gapless: true,
            byte_len_handle: None,
        }
    }
}
```

**Step 4: Update lib.rs**

```rust
// Add to crates/kithara-decode/src/lib.rs
mod symphonia;
pub use symphonia::SymphoniaConfig;
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p kithara-decode symphonia::tests -v`
Expected: 1 test PASS

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/symphonia.rs crates/kithara-decode/src/lib.rs
git commit -m "feat(decode): add SymphoniaConfig"
```

---

### Task 5: Implement Symphonia<C> struct and AAC decoder

**Files:**
- Modify: `crates/kithara-decode/src/symphonia.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Write the failing test**

```rust
// Add to crates/kithara-decode/src/symphonia.rs tests
use std::io::Cursor;

#[test]
fn test_symphonia_aac_decodes_adts() {
    // ADTS frame: sync word (0xFFF), profile, sample rate, channels, frame length
    // This is a minimal valid ADTS header (no actual audio data)
    let adts_header = [
        0xFF, 0xF1, // Sync word + MPEG-4, Layer 0, no CRC
        0x50, 0x80, // AAC-LC, 44100Hz, private=0, channel=2
        0x00, 0x1F, // frame_length high bits
        0xFC,       // frame_length low bits + buffer fullness
    ];

    let source = Cursor::new(adts_header.to_vec());
    let result = SymphoniaAac::create(source, SymphoniaConfig::default());

    // Should fail gracefully with InvalidData (not panic)
    assert!(result.is_err());
}

#[test]
fn test_symphonia_types_exist() {
    // Verify type aliases compile
    fn _check_aac(_: SymphoniaAac) {}
    fn _check_mp3(_: SymphoniaMp3) {}
    fn _check_flac(_: SymphoniaFlac) {}
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-decode symphonia::tests --no-run 2>&1 | head -20`
Expected: Compilation error — `SymphoniaAac` not found

**Step 3: Write Symphonia struct**

```rust
// Add to crates/kithara-decode/src/symphonia.rs

use symphonia::core::audio::AudioBufferRef;
use symphonia::core::codecs::{CodecRegistry, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Inner implementation shared across all Symphonia codecs
struct SymphoniaInner {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    spec: PcmSpec,
    position: Duration,
    duration: Option<Duration>,
    byte_len_handle: Option<Arc<AtomicU64>>,
}

/// Generic Symphonia-based decoder
pub struct Symphonia<C: CodecType> {
    inner: SymphoniaInner,
    _codec: PhantomData<C>,
}

// Type aliases for convenience
pub type SymphoniaAac = Symphonia<Aac>;
pub type SymphoniaMp3 = Symphonia<Mp3>;
pub type SymphoniaFlac = Symphonia<Flac>;
pub type SymphoniaVorbis = Symphonia<Vorbis>;

impl<C: CodecType> AudioDecoder for Symphonia<C> {
    type Config = SymphoniaConfig;

    fn create<R>(source: R, config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + 'static,
    {
        let inner = SymphoniaInner::new(source, C::CODEC, config)?;
        Ok(Self {
            inner,
            _codec: PhantomData,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn position(&self) -> Duration {
        self.inner.position
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }
}

impl SymphoniaInner {
    fn new<R>(source: R, codec: kithara_stream::AudioCodec, config: SymphoniaConfig) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + 'static,
    {
        // Create media source stream
        let mss = MediaSourceStream::new(Box::new(ReadSeekAdapter::new(source)), Default::default());

        // Set up hint based on codec
        let mut hint = Hint::new();
        match codec {
            kithara_stream::AudioCodec::Aac => hint.with_extension("aac"),
            kithara_stream::AudioCodec::Mp3 => hint.with_extension("mp3"),
            kithara_stream::AudioCodec::Flac => hint.with_extension("flac"),
            kithara_stream::AudioCodec::Vorbis => hint.with_extension("ogg"),
            kithara_stream::AudioCodec::Alac => hint.with_extension("m4a"),
            _ => &mut hint,
        };

        // Probe the format
        let format_opts = FormatOptions {
            enable_gapless: config.gapless,
            ..Default::default()
        };
        let metadata_opts = MetadataOptions::default();

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        let format_reader = probed.format;

        // Find audio track
        let track = format_reader
            .default_track()
            .ok_or_else(|| DecodeError::InvalidData("no audio track found".into()))?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        // Extract spec
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| DecodeError::InvalidData("unknown sample rate".into()))?;
        let channels = codec_params
            .channels
            .map(|c| c.count() as u16)
            .ok_or_else(|| DecodeError::InvalidData("unknown channel count".into()))?;

        let spec = PcmSpec {
            sample_rate,
            channels,
        };

        // Calculate duration
        let duration = codec_params.n_frames.and_then(|frames| {
            Some(Duration::from_secs_f64(frames as f64 / sample_rate as f64))
        });

        // Create decoder
        let decoder_opts = DecoderOptions {
            verify: config.verify,
        };
        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &decoder_opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
            position: Duration::ZERO,
            duration,
            byte_len_handle: config.byte_len_handle,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };

            // Skip packets from other tracks
            if packet.track_id() != self.track_id {
                continue;
            }

            // Decode packet
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };

            // Convert to PcmChunk
            let chunk = self.convert_to_chunk(decoded, &packet)?;
            self.position = chunk.timestamp + chunk_duration(&chunk);
            return Ok(Some(chunk));
        }
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        let seek_to = SeekTo::Time {
            time: symphonia::core::units::Time::from(pos.as_secs_f64()),
            track_id: Some(self.track_id),
        };

        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| DecodeError::SeekFailed(e.to_string()))?;

        self.decoder.reset();
        self.position = pos;
        Ok(())
    }

    fn convert_to_chunk(&self, decoded: AudioBufferRef, packet: &symphonia::core::formats::Packet) -> DecodeResult<PcmChunk> {
        use symphonia::core::audio::Signal;
        use symphonia::core::conv::IntoSample;

        let timestamp = packet
            .ts()
            .checked_mul(1_000_000_000)
            .and_then(|ns| ns.checked_div(self.spec.sample_rate as u64))
            .map(Duration::from_nanos)
            .unwrap_or(self.position);

        let frames = decoded.frames();
        let channels = self.spec.channels as usize;
        let total_samples = frames * channels;

        // Allocate from pool (placeholder - will use actual pool later)
        let mut samples = vec![0.0f32; total_samples];

        // Convert to interleaved f32
        match decoded {
            AudioBufferRef::F32(buf) => {
                for (i, frame) in buf.frames().enumerate() {
                    for (c, &sample) in frame.iter().enumerate() {
                        samples[i * channels + c] = sample;
                    }
                }
            }
            AudioBufferRef::S16(buf) => {
                for (i, frame) in buf.frames().enumerate() {
                    for (c, &sample) in frame.iter().enumerate() {
                        samples[i * channels + c] = sample.into_sample();
                    }
                }
            }
            AudioBufferRef::S32(buf) => {
                for (i, frame) in buf.frames().enumerate() {
                    for (c, &sample) in frame.iter().enumerate() {
                        samples[i * channels + c] = sample.into_sample();
                    }
                }
            }
            _ => {
                return Err(DecodeError::InvalidData("unsupported sample format".into()));
            }
        }

        Ok(PcmChunk {
            samples,
            spec: self.spec,
            timestamp,
        })
    }
}

fn chunk_duration(chunk: &PcmChunk) -> Duration {
    let frames = chunk.samples.len() / chunk.spec.channels as usize;
    Duration::from_secs_f64(frames as f64 / chunk.spec.sample_rate as f64)
}

/// Adapter to make Read + Seek work as MediaSource
struct ReadSeekAdapter<R> {
    inner: R,
    byte_len: Option<u64>,
}

impl<R: Read + Seek> ReadSeekAdapter<R> {
    fn new(mut inner: R) -> Self {
        let byte_len = inner.seek(std::io::SeekFrom::End(0)).ok();
        let _ = inner.seek(std::io::SeekFrom::Start(0));
        Self { inner, byte_len }
    }
}

impl<R: Read + Seek + Send + Sync> symphonia::core::io::MediaSource for ReadSeekAdapter<R> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

impl<R: Read> Read for ReadSeekAdapter<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for ReadSeekAdapter<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}
```

**Step 4: Update types.rs to have simpler PcmChunk**

Note: The current types.rs has `PcmChunk` with `PooledBuffer`. For now we'll use `Vec<f32>` and integrate with pool later.

```rust
// Temporarily update PcmChunk in types.rs or create new version
// This will be refined in a later task
```

**Step 5: Update lib.rs exports**

```rust
// Add to crates/kithara-decode/src/lib.rs
pub use symphonia::{
    Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
};
```

**Step 6: Run test to verify it passes**

Run: `cargo test -p kithara-decode symphonia::tests -v`
Expected: 3 tests PASS

**Step 7: Commit**

```bash
git add crates/kithara-decode/src/symphonia.rs crates/kithara-decode/src/lib.rs
git commit -m "feat(decode): implement Symphonia<C> generic decoder"
```

---

### Task 6: Add integration test with real WAV file

**Files:**
- Create: `crates/kithara-decode/tests/symphonia_integration.rs`

**Step 1: Write the test**

```rust
// crates/kithara-decode/tests/symphonia_integration.rs
use kithara_decode::{AudioDecoder, SymphoniaConfig, SymphoniaMp3};
use std::io::Cursor;
use std::time::Duration;

// Generate a minimal valid WAV file for testing
fn create_test_wav() -> Vec<u8> {
    let sample_rate = 44100u32;
    let channels = 2u16;
    let bits_per_sample = 16u16;
    let num_samples = 4410; // 0.1 seconds
    let data_size = (num_samples * channels as u32 * (bits_per_sample / 8) as u32) as u32;

    let mut wav = Vec::new();

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * channels as u32 * bits_per_sample as u32 / 8).to_le_bytes());
    wav.extend_from_slice(&(channels * bits_per_sample / 8).to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    // Silence samples
    for _ in 0..num_samples * channels as u32 {
        wav.extend_from_slice(&0i16.to_le_bytes());
    }

    wav
}

#[test]
fn test_decode_wav_file() {
    // Note: WAV doesn't fit cleanly into our codec type system
    // This test verifies the underlying Symphonia integration works
    // For WAV we may need a separate SymphoniaWav type
}

#[test]
fn test_decoder_spec() {
    // Test that spec is correctly extracted
    // Will be implemented with actual test files
}
```

**Step 2: Run test**

Run: `cargo test -p kithara-decode --test symphonia_integration -v`

**Step 3: Commit**

```bash
git add crates/kithara-decode/tests/symphonia_integration.rs
git commit -m "test(decode): add symphonia integration test scaffold"
```

---

## Phase 3: Factory

### Task 7: Create factory module

**Files:**
- Create: `crates/kithara-decode/src/factory.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Write the failing test**

```rust
// In crates/kithara-decode/src/factory.rs
#[cfg(test)]
mod tests {
    use super::*;
    use kithara_stream::AudioCodec;

    #[test]
    fn test_codec_selector_exact() {
        let selector = CodecSelector::Exact(AudioCodec::Aac);
        assert!(matches!(selector, CodecSelector::Exact(AudioCodec::Aac)));
    }

    #[test]
    fn test_probe_hint_default() {
        let hint = ProbeHint::default();
        assert!(hint.codec.is_none());
        assert!(hint.container.is_none());
        assert!(hint.extension.is_none());
        assert!(hint.mime.is_none());
    }

    #[test]
    fn test_decoder_config_default() {
        let config = DecoderConfig::default();
        assert!(!config.prefer_hardware);
        assert!(config.byte_len_handle.is_none());
        assert!(config.gapless);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-decode factory::tests --no-run 2>&1 | head -20`
Expected: Compilation error — module `factory` not found

**Step 3: Write factory types**

```rust
// crates/kithara-decode/src/factory.rs
use crate::error::{DecodeError, DecodeResult};
use crate::traits::AudioDecoder;
use crate::symphonia::{
    Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
};
use kithara_stream::{AudioCodec, ContainerFormat};
use std::io::{Read, Seek};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// Selector for choosing how to detect/specify the codec
#[derive(Debug, Clone)]
pub enum CodecSelector {
    /// Known codec — no probing needed
    Exact(AudioCodec),
    /// Probe with hints
    Probe(ProbeHint),
    /// Full auto-probe
    Auto,
}

/// Hints for codec probing
#[derive(Debug, Clone, Default)]
pub struct ProbeHint {
    pub codec: Option<AudioCodec>,
    pub container: Option<ContainerFormat>,
    pub extension: Option<String>,
    pub mime: Option<String>,
}

/// Configuration for DecoderFactory
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// Prefer hardware decoder (Apple/Android) when available
    pub prefer_hardware: bool,
    /// Handle for dynamic byte length updates (HLS)
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Enable gapless playback
    pub gapless: bool,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            prefer_hardware: false,
            byte_len_handle: None,
            gapless: true,
        }
    }
}

/// Factory for creating decoders with runtime backend selection
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with automatic backend selection
    pub fn create<R>(
        source: R,
        selector: CodecSelector,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn AudioDecoder<Config = SymphoniaConfig>>>
    where
        R: Read + Seek + Send + 'static,
    {
        let codec = match selector {
            CodecSelector::Exact(c) => c,
            CodecSelector::Probe(hint) => Self::probe_codec(&hint)?,
            CodecSelector::Auto => Self::probe_codec(&ProbeHint::default())?,
        };

        Self::create_for_codec(source, codec, config)
    }

    fn create_for_codec<R>(
        source: R,
        codec: AudioCodec,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn AudioDecoder<Config = SymphoniaConfig>>>
    where
        R: Read + Seek + Send + 'static,
    {
        let symphonia_config = SymphoniaConfig {
            verify: false,
            gapless: config.gapless,
            byte_len_handle: config.byte_len_handle,
        };

        match codec {
            AudioCodec::Aac => Ok(Box::new(SymphoniaAac::create(source, symphonia_config)?)),
            AudioCodec::Mp3 => Ok(Box::new(SymphoniaMp3::create(source, symphonia_config)?)),
            AudioCodec::Flac => Ok(Box::new(SymphoniaFlac::create(source, symphonia_config)?)),
            AudioCodec::Vorbis => Ok(Box::new(SymphoniaVorbis::create(source, symphonia_config)?)),
            other => Err(DecodeError::UnsupportedCodec(other)),
        }
    }

    fn probe_codec(hint: &ProbeHint) -> DecodeResult<AudioCodec> {
        // Use hint if available
        if let Some(codec) = hint.codec {
            return Ok(codec);
        }

        // Try to infer from extension
        if let Some(ext) = &hint.extension {
            match ext.to_lowercase().as_str() {
                "aac" | "m4a" | "mp4" => return Ok(AudioCodec::Aac),
                "mp3" => return Ok(AudioCodec::Mp3),
                "flac" => return Ok(AudioCodec::Flac),
                "ogg" => return Ok(AudioCodec::Vorbis),
                _ => {}
            }
        }

        // Try to infer from MIME
        if let Some(mime) = &hint.mime {
            match mime.as_str() {
                "audio/aac" | "audio/mp4" | "audio/x-m4a" => return Ok(AudioCodec::Aac),
                "audio/mpeg" | "audio/mp3" => return Ok(AudioCodec::Mp3),
                "audio/flac" => return Ok(AudioCodec::Flac),
                "audio/ogg" => return Ok(AudioCodec::Vorbis),
                _ => {}
            }
        }

        Err(DecodeError::ProbeFailed)
    }
}

#[cfg(test)]
mod tests {
    // ... tests from Step 1
}
```

**Step 4: Update lib.rs**

```rust
// Add to crates/kithara-decode/src/lib.rs
mod factory;
pub use factory::{CodecSelector, DecoderConfig, DecoderFactory, ProbeHint};
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p kithara-decode factory::tests -v`
Expected: 3 tests PASS

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/factory.rs crates/kithara-decode/src/lib.rs
git commit -m "feat(decode): add DecoderFactory with CodecSelector and ProbeHint"
```

---

## Phase 4: Cleanup and Migration

### Task 8: Update types.rs for new architecture

**Files:**
- Modify: `crates/kithara-decode/src/types.rs`

**Step 1: Review and update PcmChunk**

The current PcmChunk uses `PooledBuffer`. We need to:
1. Keep backward compatibility with existing types
2. Add `timestamp` field
3. Ensure PcmSpec matches new design

This task will update types to match the new architecture while maintaining API compatibility where possible.

**Step 2: Run all tests**

Run: `cargo test -p kithara-decode -v`

**Step 3: Commit**

```bash
git add crates/kithara-decode/src/types.rs
git commit -m "refactor(decode): update types for new architecture"
```

---

### Task 9: Deprecate old decoder.rs

**Files:**
- Rename: `crates/kithara-decode/src/decoder.rs` → `crates/kithara-decode/src/legacy.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Rename and add deprecation notice**

```rust
// crates/kithara-decode/src/legacy.rs (top of file)
//! Legacy decoder implementation.
//!
//! This module is deprecated. Use the new generic decoders:
//! - `SymphoniaAac`, `SymphoniaMp3`, `SymphoniaFlac` for direct use
//! - `DecoderFactory::create()` for runtime selection
#![deprecated(since = "0.2.0", note = "Use SymphoniaAac/Mp3/Flac or DecoderFactory instead")]
```

**Step 2: Update lib.rs**

```rust
// Keep old exports for backward compatibility
#[allow(deprecated)]
mod legacy;
#[allow(deprecated)]
pub use legacy::{CachedCodecInfo, Decoder, InnerDecoder};
```

**Step 3: Run tests to ensure backward compatibility**

Run: `cargo test --workspace`

**Step 4: Commit**

```bash
git mv crates/kithara-decode/src/decoder.rs crates/kithara-decode/src/legacy.rs
git add crates/kithara-decode/src/lib.rs
git commit -m "refactor(decode): deprecate legacy Decoder in favor of Symphonia<C>"
```

---

## Phase 5: Integration with kithara-audio (Future)

### Task 10: Update kithara-audio to use new decoders

This is a larger task that will be planned separately once the new decoder API is stable.

**Key changes needed:**
1. Update `StreamSource` to use `DecoderFactory`
2. Remove `FormatReader` dependency
3. Fix seek issues with proper packet-based approach

---

## Summary

| Phase | Tasks | Description |
|-------|-------|-------------|
| 1 | 1-3 | Foundation: error, traits, AudioDecoder |
| 2 | 4-6 | Symphonia backend implementation |
| 3 | 7 | DecoderFactory |
| 4 | 8-9 | Types cleanup, deprecate legacy |
| 5 | 10+ | kithara-audio integration (future) |

**Estimated commits:** 9
**Breaking changes:** None (old API deprecated but available)
