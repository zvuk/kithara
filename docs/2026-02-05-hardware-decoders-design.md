# Hardware Decoders & Legacy Removal Design

## Overview

Полная миграция kithara-decode: удаление legacy, добавление Apple/Android hardware декодеров, унификация API.

## Scope

1. Удалить `legacy.rs` и старый `DecodeError` из `types.rs`
2. Добавить `Decoder<D>` generic wrapper
3. Реализовать Apple декодер (AudioConverter)
4. Реализовать Android декодер (MediaCodec)
5. Обновить DecoderFactory с hardware fallback
6. Мигрировать kithara-audio на новый API

## File Structure

```
kithara-decode/src/
├── lib.rs           // pub exports
├── error.rs         // DecodeError (единственный)
├── types.rs         // PcmChunk, PcmSpec, TrackMetadata
├── traits.rs        // AudioDecoder, CodecType, codec markers
├── decoder.rs       // Decoder<D> generic wrapper (NEW)
├── factory.rs       // DecoderFactory с hardware fallback
├── symphonia.rs     // Symphonia<C>, SymphoniaAac/Mp3/Flac/Vorbis
├── apple.rs         // Apple<C>, AppleAac/Mp3/Flac/Alac
└── android.rs       // Android<C>, AndroidAac/Mp3/Flac/Alac
```

## Decoder<D> Generic Wrapper

```rust
pub struct Decoder<D: AudioDecoder> {
    inner: D,
}

impl<D: AudioDecoder> Decoder<D> {
    pub fn new<R>(source: R, config: D::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        Ok(Self {
            inner: D::create(source, config)?,
        })
    }

    pub fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        self.inner.next_chunk()
    }

    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    pub fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }
}

// Usage:
let dec = Decoder::<SymphoniaAac>::new(stream, SymphoniaConfig::default())?;
let dec = Decoder::<AppleAac>::new(stream, AppleConfig::default())?;
let dec = Decoder::<AndroidMp3>::new(stream, AndroidConfig::default())?;
```

## Apple Decoder (AudioConverter)

```rust
// apple.rs
#[cfg(any(target_os = "macos", target_os = "ios"))]

use coreaudio_sys::*;

#[derive(Debug, Clone, Default)]
pub struct AppleConfig {
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

struct AppleInner {
    converter: AudioConverterRef,
    source: Box<dyn Read + Seek + Send>,
    input_asbd: AudioStreamBasicDescription,
    output_asbd: AudioStreamBasicDescription,
    spec: PcmSpec,
    position: Duration,
    duration: Option<Duration>,
    read_buffer: Vec<u8>,
}

pub struct Apple<C: CodecType> {
    inner: AppleInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> AudioDecoder for Apple<C> {
    type Config = AppleConfig;
    // delegates to inner
}

// Type aliases
pub type AppleAac = Apple<Aac>;
pub type AppleMp3 = Apple<Mp3>;
pub type AppleFlac = Apple<Flac>;
pub type AppleAlac = Apple<Alac>;
```

Key: `AudioConverterFillComplexBuffer` with input callback reads compressed data, outputs PCM. Seek = reset converter + seek source.

## Android Decoder (MediaCodec)

```rust
// android.rs
#[cfg(target_os = "android")]

use mediacodec::{MediaCodec, MediaFormat};

#[derive(Debug, Clone, Default)]
pub struct AndroidConfig {
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

struct AndroidInner {
    codec: MediaCodec,
    source: Box<dyn Read + Seek + Send>,
    spec: PcmSpec,
    position: Duration,
    duration: Option<Duration>,
    input_buffer: Vec<u8>,
    eos_sent: bool,
}

pub struct Android<C: CodecType> {
    inner: AndroidInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> AudioDecoder for Android<C> {
    type Config = AndroidConfig;
    // delegates to inner
}

// Type aliases
pub type AndroidAac = Android<Aac>;
pub type AndroidMp3 = Android<Mp3>;
pub type AndroidFlac = Android<Flac>;
pub type AndroidAlac = Android<Alac>;
```

Pattern: queue input buffers → dequeue output buffers. Seek = flush + reset EOS flag.

## DecoderFactory with Hardware Fallback

```rust
impl DecoderFactory {
    pub fn create<R>(
        source: R,
        selector: CodecSelector,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn AudioDecoder<Config = ()>>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let codec = match selector {
            CodecSelector::Exact(c) => c,
            CodecSelector::Probe(hint) => Self::probe_codec(&hint)?,
            CodecSelector::Auto => Self::probe_codec(&ProbeHint::default())?,
        };

        // Hardware fallback chain
        if config.prefer_hardware {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let Ok(dec) = Self::try_apple(&source, codec, &config) {
                return Ok(dec);
            }

            #[cfg(target_os = "android")]
            if let Ok(dec) = Self::try_android(&source, codec, &config) {
                return Ok(dec);
            }
        }

        // Symphonia fallback (always available)
        Self::try_symphonia(source, codec, &config)
    }
}
```

## kithara-audio Migration

**Before:**
```rust
use kithara_decode::{InnerDecoder, Decoder};
let decoder = Decoder::new_from_media_info(stream, media_info, byte_len)?;
```

**After:**
```rust
use kithara_decode::{Decoder, DecoderFactory, CodecSelector, DecoderConfig, SymphoniaAac};

// Static type (compile-time known):
let decoder = Decoder::<SymphoniaAac>::new(stream, SymphoniaConfig::default())?;

// Runtime selection:
let decoder = DecoderFactory::create(
    stream,
    CodecSelector::Exact(media_info.codec),
    DecoderConfig {
        prefer_hardware: config.prefer_hardware,
        byte_len_handle: Some(byte_len.clone()),
        gapless: true,
    },
)?;
```

**ABR switch:**
```rust
// Recreate decoder for new variant
let new_decoder = DecoderFactory::create(
    new_stream,
    CodecSelector::Exact(new_media_info.codec),
    config.clone(),
)?;
```

## Cargo.toml Features

```toml
[features]
default = ["symphonia"]
symphonia = ["dep:symphonia", "dep:symphonia-core"]
apple = ["dep:coreaudio-sys"]
android = ["dep:mediacodec"]

[dependencies]
coreaudio-sys = { version = "0.2", optional = true }
mediacodec = { version = "0.3", optional = true }
```

## Public API (lib.rs)

```rust
// Core
pub use error::{DecodeError, DecodeResult};
pub use types::{PcmChunk, PcmSpec, TrackMetadata};
pub use traits::{AudioDecoder, CodecType, Aac, Mp3, Flac, Alac, Vorbis};

// Generic wrapper
pub use decoder::Decoder;

// Factory
pub use factory::{DecoderFactory, DecoderConfig, CodecSelector, ProbeHint};

// Symphonia (always available)
pub use symphonia::{
    Symphonia, SymphoniaConfig,
    SymphoniaAac, SymphoniaMp3, SymphoniaFlac, SymphoniaVorbis,
};

// Apple (feature + platform)
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub use apple::{Apple, AppleConfig, AppleAac, AppleMp3, AppleFlac, AppleAlac};

// Android (feature + platform)
#[cfg(all(feature = "android", target_os = "android"))]
pub use android::{Android, AndroidConfig, AndroidAac, AndroidMp3, AndroidFlac, AndroidAlac};
```

## Key Decisions

1. **Full migration** — remove legacy, update kithara-audio, add hardware decoders
2. **All codecs** — AAC, MP3, FLAC, ALAC for hardware
3. **Auto fallback** — DecoderFactory tries hardware → Symphonia
4. **Feature flags + cfg** — platform-specific code properly gated
5. **Decoder<D>::new()** — single constructor, generic wrapper
6. **ABR via recreate** — create new decoder with CodecSelector::Exact
