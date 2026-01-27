//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`Decoder`] - Generic decoder running in a separate thread
//! - [`SymphoniaDecoder`] - Symphonia-based audio decoder (internal)
//! - [`AudioSyncReader`] - rodio::Source adapter (requires `rodio` feature)
//!
//! ## Target API
//!
//! ```ignore
//! use kithara_decode::{Decoder, DecoderConfig};
//! use kithara_hls::{Hls, HlsConfig};
//! use kithara_stream::Stream;
//!
//! // HLS stream with decoding
//! let config = DecoderConfig::<Hls>::new(hls_config).streaming();
//! let decoder = Decoder::<Stream<Hls>>::new(config).await?;
//! sink.append(decoder);  // rodio compatible
//!
//! // Or read PCM from channel directly
//! while let Ok(chunk) = decoder.pcm_rx().recv() {
//!     play_audio(chunk);
//! }
//! ```

#![forbid(unsafe_code)]

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod decode_pipeline;
mod decoder;
mod source_reader;
mod symphonia_mod;
mod types;

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use decode_pipeline::{DecodeOptions, Decoder, DecoderConfig};
pub use decoder::InnerDecoder;
pub use source_reader::SourceReader;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};

// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
