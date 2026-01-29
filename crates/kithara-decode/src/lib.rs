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
//!
//! // Events
//! let mut events = decoder.events();
//! while let Ok(event) = events.recv().await {
//!     match event {
//!         DecoderEvent::Stream(e) => println!("Stream: {:?}", e),
//!         DecoderEvent::Decode(e) => println!("Decode: {:?}", e),
//!     }
//! }
//! ```

#![forbid(unsafe_code)]

// Internal modules
mod decoder;
mod events;
mod pipeline;
mod reader;
mod symphonia;
#[cfg(feature = "rodio")]
mod sync;
mod types;

// Public API exports
pub use decoder::InnerDecoder;
pub use events::{DecodeEvent, DecoderEvent};
// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
pub use pipeline::{DecodeOptions, Decoder, DecoderConfig};
pub use reader::SourceReader;
pub use symphonia::{CachedCodecInfo, SymphoniaDecoder};
#[cfg(feature = "rodio")]
pub use sync::AudioSyncReader;
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};
