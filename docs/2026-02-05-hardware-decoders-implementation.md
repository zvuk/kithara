# Hardware Decoders Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove legacy decoder, add `Decoder<D>` generic wrapper, implement Apple/Android hardware decoder stubs, migrate kithara-audio to new API.

**Architecture:**
- `Decoder<D>` generic wrapper with unified `new()` constructor
- Apple/Android as feature-gated placeholder modules (actual FFI implementation later)
- DecoderFactory with hardware fallback chain
- kithara-audio uses `InnerDecoder` trait backed by new API

**Tech Stack:** Rust, Symphonia, feature flags (`apple`, `android`)

---

## Task 1: Remove legacy.rs and old DecodeError from types.rs

**Files:**
- Delete: `crates/kithara-decode/src/legacy.rs`
- Modify: `crates/kithara-decode/src/types.rs:21-42` (remove old DecodeError)
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Read current exports and identify what to remove**

Check lib.rs for legacy exports that need updating.

**Step 2: Remove legacy.rs module declaration from lib.rs**

Remove:
```rust
#[allow(deprecated)]
mod legacy;
```

And remove legacy exports:
```rust
#[allow(deprecated)]
pub use legacy::{CachedCodecInfo, Decoder, InnerDecoder};
```

**Step 3: Remove DecodeError from types.rs**

Remove `DecodeError` enum (lines 21-42) and `DecodeResult` type alias from types.rs.
Keep only `PcmSpec`, `PcmChunk`, `TrackMetadata`.

**Step 4: Update lib.rs exports**

Change:
```rust
pub use types::{DecodeError, DecodeResult, PcmChunk, PcmSpec, TrackMetadata};
```
To:
```rust
pub use error::{DecodeError, DecodeResult};
pub use types::{PcmChunk, PcmSpec, TrackMetadata};
```

**Step 5: Delete legacy.rs file**

```bash
rm crates/kithara-decode/src/legacy.rs
```

**Step 6: Run tests**

```bash
cargo test -p kithara-decode
```

Expected: Tests related to legacy.rs will fail. That's expected — we'll fix in Task 2.

**Step 7: Commit**

```bash
git add -A
git commit -m "refactor(decode): remove legacy.rs and old DecodeError"
```

---

## Task 2: Export new traits and types from lib.rs

**Files:**
- Modify: `crates/kithara-decode/src/lib.rs`
- Modify: `crates/kithara-decode/src/error.rs` (remove #[allow(dead_code)])
- Modify: `crates/kithara-decode/src/traits.rs` (remove #[allow(dead_code)])
- Modify: `crates/kithara-decode/src/symphonia.rs` (remove #[allow(dead_code)])
- Modify: `crates/kithara-decode/src/factory.rs` (remove #[allow(dead_code)])

**Step 1: Update lib.rs with new public API**

```rust
//! # Kithara Decode
//!
//! Audio decoding library with pluggable backends.
//!
//! Provides generic decoder infrastructure supporting Symphonia (software),
//! Apple AudioToolbox, and Android MediaCodec backends.

#![forbid(unsafe_code)]

mod decoder;
mod error;
mod factory;
mod symphonia;
mod traits;
mod types;

// Error types
pub use error::{DecodeError, DecodeResult};

// Core types
pub use types::{PcmChunk, PcmSpec, TrackMetadata};

// Traits and codec markers
pub use traits::{Aac, Alac, AudioDecoder, CodecType, Flac, Mp3, Vorbis};

// Generic wrapper
pub use decoder::Decoder;

// Symphonia backend
pub use symphonia::{
    Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
};

// Factory for runtime selection
pub use factory::{CodecSelector, DecoderConfig, DecoderFactory, ProbeHint};

// Re-export stream types for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
```

**Step 2: Remove dead_code allows from modules**

In each file (error.rs, traits.rs, symphonia.rs, factory.rs), remove:
```rust
#![allow(dead_code)]
```
and
```rust
#[allow(dead_code)]
```

**Step 3: Run tests**

```bash
cargo test -p kithara-decode
```

Expected: Compilation errors about missing `decoder` module. That's Task 3.

**Step 4: Commit (after Task 3)**

---

## Task 3: Create Decoder<D> generic wrapper

**Files:**
- Create: `crates/kithara-decode/src/decoder.rs`

**Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::symphonia::SymphoniaConfig;
    use std::io::Cursor;

    fn create_test_wav() -> Vec<u8> {
        // Minimal WAV header + data
        // ... (copy from symphonia.rs tests)
    }

    #[test]
    fn test_decoder_wrapper_create() {
        let wav = create_test_wav();
        let cursor = Cursor::new(wav);

        let decoder = Decoder::<SymphoniaMp3>::new(cursor, SymphoniaConfig::default());
        // Will fail initially — SymphoniaMp3 doesn't decode WAV
    }
}
```

**Step 2: Implement Decoder<D>**

```rust
//! Generic decoder wrapper.

use std::{
    io::{Read, Seek},
    time::Duration,
};

use crate::{
    error::DecodeResult,
    traits::AudioDecoder,
    types::{PcmChunk, PcmSpec},
};

/// Generic decoder wrapper providing unified API.
///
/// Wraps any [`AudioDecoder`] implementation, delegating all operations
/// to the inner decoder.
///
/// # Example
///
/// ```ignore
/// use kithara_decode::{Decoder, SymphoniaAac, SymphoniaConfig};
///
/// let decoder = Decoder::<SymphoniaAac>::new(file, SymphoniaConfig::default())?;
/// while let Some(chunk) = decoder.next_chunk()? {
///     // Process PCM
/// }
/// ```
pub struct Decoder<D: AudioDecoder> {
    inner: D,
}

impl<D: AudioDecoder> Decoder<D> {
    /// Create a new decoder from a Read + Seek source.
    pub fn new<R>(source: R, config: D::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        Ok(Self {
            inner: D::create(source, config)?,
        })
    }

    /// Decode the next chunk of PCM data.
    pub fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        self.inner.next_chunk()
    }

    /// Get PCM output specification.
    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    /// Seek to a time position.
    pub fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    /// Get current playback position.
    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// Get total duration if known.
    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    /// Get reference to inner decoder.
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// Get mutable reference to inner decoder.
    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    /// Consume wrapper and return inner decoder.
    pub fn into_inner(self) -> D {
        self.inner
    }
}
```

**Step 3: Run tests**

```bash
cargo test -p kithara-decode
```

**Step 4: Commit**

```bash
git add crates/kithara-decode/src/decoder.rs
git commit -m "feat(decode): add Decoder<D> generic wrapper"
```

---

## Task 4: Add InnerDecoder trait for kithara-audio compatibility

**Files:**
- Modify: `crates/kithara-decode/src/traits.rs`
- Modify: `crates/kithara-decode/src/lib.rs`

**Step 1: Add InnerDecoder trait**

This trait bridges old API (kithara-audio expects InnerDecoder) with new AudioDecoder.

```rust
/// Legacy trait for audio decoders.
///
/// This trait is used by kithara-audio for runtime polymorphism.
/// New code should use [`AudioDecoder`] directly.
pub trait InnerDecoder: Send + 'static {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>>;
    fn spec(&self) -> PcmSpec;
    fn seek(&mut self, pos: Duration) -> DecodeResult<()>;
    fn update_byte_len(&self, len: u64);
    fn duration(&self) -> Option<Duration>;
}
```

**Step 2: Implement InnerDecoder for Symphonia<C>**

Add to symphonia.rs:
```rust
impl<C: CodecType> InnerDecoder for Symphonia<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        AudioDecoder::next_chunk(self)
    }

    fn spec(&self) -> PcmSpec {
        AudioDecoder::spec(self)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        AudioDecoder::seek(self, pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.update_byte_len(len);
    }

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }
}
```

**Step 3: Export InnerDecoder from lib.rs**

```rust
pub use traits::{..., InnerDecoder};
```

**Step 4: Run tests**

```bash
cargo test -p kithara-decode
```

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(decode): add InnerDecoder trait for kithara-audio compatibility"
```

---

## Task 5: Create Apple decoder placeholder (feature-gated)

**Files:**
- Create: `crates/kithara-decode/src/apple.rs`
- Modify: `crates/kithara-decode/src/lib.rs`
- Modify: `crates/kithara-decode/Cargo.toml`

**Step 1: Add feature to Cargo.toml**

```toml
[features]
default = []
apple = []
```

**Step 2: Create apple.rs with placeholder**

```rust
//! Apple AudioToolbox decoder backend (placeholder).
//!
//! This module will implement hardware-accelerated audio decoding
//! using Apple's AudioToolbox framework via AudioConverter APIs.
//!
//! Currently a placeholder — actual FFI implementation pending.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use std::{
    io::{Read, Seek},
    marker::PhantomData,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, Alac, AudioDecoder, CodecType, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmSpec},
};

/// Configuration for Apple AudioToolbox decoder.
#[derive(Debug, Clone, Default)]
pub struct AppleConfig {
    /// Handle for dynamic byte length updates.
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

/// Apple AudioToolbox decoder (placeholder).
pub struct Apple<C: CodecType> {
    _codec: PhantomData<C>,
}

impl<C: CodecType> AudioDecoder for Apple<C> {
    type Config = AppleConfig;

    fn create<R>(_source: R, _config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
        Self: Sized,
    {
        Err(DecodeError::Backend(Box::new(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Apple decoder not yet implemented",
        ))))
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        unreachable!("Apple decoder not yet implemented")
    }

    fn spec(&self) -> PcmSpec {
        unreachable!("Apple decoder not yet implemented")
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        unreachable!("Apple decoder not yet implemented")
    }

    fn position(&self) -> Duration {
        unreachable!("Apple decoder not yet implemented")
    }
}

impl<C: CodecType> InnerDecoder for Apple<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        AudioDecoder::next_chunk(self)
    }

    fn spec(&self) -> PcmSpec {
        AudioDecoder::spec(self)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        AudioDecoder::seek(self, pos)
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }
}

/// Apple AAC decoder.
pub type AppleAac = Apple<Aac>;

/// Apple MP3 decoder.
pub type AppleMp3 = Apple<Mp3>;

/// Apple FLAC decoder.
pub type AppleFlac = Apple<Flac>;

/// Apple ALAC decoder.
pub type AppleAlac = Apple<Alac>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_apple_config_default() {
        let config = AppleConfig::default();
        assert!(config.byte_len_handle.is_none());
    }

    #[test]
    fn test_apple_decoder_not_implemented() {
        let cursor = Cursor::new(vec![0u8; 100]);
        let result = AppleAac::create(cursor, AppleConfig::default());
        assert!(result.is_err());
    }
}
```

**Step 3: Add conditional export to lib.rs**

```rust
// Apple backend (feature + platform gated)
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple;
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub use apple::{Apple, AppleAac, AppleAlac, AppleConfig, AppleFlac, AppleMp3};
```

**Step 4: Run tests**

```bash
cargo test -p kithara-decode
cargo test -p kithara-decode --features apple  # on macOS
```

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(decode): add Apple decoder placeholder (feature-gated)"
```

---

## Task 6: Create Android decoder placeholder (feature-gated)

**Files:**
- Create: `crates/kithara-decode/src/android.rs`
- Modify: `crates/kithara-decode/src/lib.rs`
- Modify: `crates/kithara-decode/Cargo.toml`

**Step 1: Add feature to Cargo.toml**

```toml
[features]
default = []
apple = []
android = []
```

**Step 2: Create android.rs with placeholder**

Similar structure to apple.rs but with `#[cfg(target_os = "android")]`.

**Step 3: Add conditional export to lib.rs**

```rust
// Android backend (feature + platform gated)
#[cfg(all(feature = "android", target_os = "android"))]
mod android;
#[cfg(all(feature = "android", target_os = "android"))]
pub use android::{Android, AndroidAac, AndroidAlac, AndroidConfig, AndroidFlac, AndroidMp3};
```

**Step 4: Run tests**

```bash
cargo test -p kithara-decode
```

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(decode): add Android decoder placeholder (feature-gated)"
```

---

## Task 7: Update DecoderFactory with hardware fallback

**Files:**
- Modify: `crates/kithara-decode/src/factory.rs`

**Step 1: Update factory to support hardware fallback**

Add hardware decoder attempts when `prefer_hardware: true`:

```rust
pub fn create<R>(
    source: R,
    selector: CodecSelector,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>>
where
    R: Read + Seek + Send + Sync + 'static,
{
    let codec = match selector {
        CodecSelector::Exact(c) => c,
        CodecSelector::Probe(hint) => Self::probe_codec(&hint)?,
        CodecSelector::Auto => return Err(DecodeError::ProbeFailed),
    };

    // Try hardware backends first if preferred
    if config.prefer_hardware {
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        if let Ok(dec) = Self::try_apple(&source, codec, &config) {
            return Ok(dec);
        }

        #[cfg(all(feature = "android", target_os = "android"))]
        if let Ok(dec) = Self::try_android(&source, codec, &config) {
            return Ok(dec);
        }
    }

    // Symphonia fallback
    Self::create_symphonia_decoder(source, codec, config)
}
```

Change return type from `Box<dyn AudioDecoder<Config = SymphoniaConfig>>` to `Box<dyn InnerDecoder>`.

**Step 2: Run tests**

```bash
cargo test -p kithara-decode
```

**Step 3: Commit**

```bash
git add -A
git commit -m "feat(decode): update DecoderFactory with hardware fallback chain"
```

---

## Task 8: Migrate kithara-audio to new API

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/stream_source.rs`
- Possibly: `crates/kithara-audio/Cargo.toml` (if needed)

**Step 1: Update imports**

Replace:
```rust
use kithara_decode::{InnerDecoder, PcmChunk, PcmSpec};
```
With:
```rust
use kithara_decode::{InnerDecoder, PcmChunk, PcmSpec, DecodeResult};
```

No structural changes needed — `InnerDecoder` trait is preserved.

**Step 2: Update DecoderFactory type in stream_source.rs**

The `DecoderFactory<T>` type alias uses `Box<dyn InnerDecoder>` which is still valid.

**Step 3: Run kithara-audio tests**

```bash
cargo test -p kithara-audio
```

**Step 4: Run full workspace tests**

```bash
cargo test --workspace
```

**Step 5: Commit**

```bash
git add -A
git commit -m "refactor(audio): migrate to new kithara-decode API"
```

---

## Task 9: Cleanup and final verification

**Files:**
- All modified files

**Step 1: Format code**

```bash
cargo fmt --all
```

**Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

**Step 3: Run all tests**

```bash
cargo test --workspace
```

**Step 4: Verify documentation builds**

```bash
cargo doc --workspace --no-deps
```

**Step 5: Final commit if needed**

```bash
git add -A
git commit -m "chore(decode): cleanup and formatting"
```

---

## Summary

| Task | Description | Key Changes |
|------|-------------|-------------|
| 1 | Remove legacy.rs | Delete old decoder, clean types.rs |
| 2 | Export new API | Update lib.rs, remove dead_code |
| 3 | Decoder<D> wrapper | New generic wrapper |
| 4 | InnerDecoder trait | Compatibility bridge for kithara-audio |
| 5 | Apple placeholder | Feature-gated stub |
| 6 | Android placeholder | Feature-gated stub |
| 7 | Factory fallback | Hardware → Symphonia chain |
| 8 | kithara-audio migration | Use new InnerDecoder |
| 9 | Cleanup | Format, lint, test |

## Expected Test Results

After all tasks:
- `cargo test -p kithara-decode`: All tests pass (minus legacy tests)
- `cargo test -p kithara-audio`: All tests pass
- `cargo test --workspace`: All tests pass
