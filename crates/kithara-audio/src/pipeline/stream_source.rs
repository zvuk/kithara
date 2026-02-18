//! Stream-based audio source with format change detection.

#[path = "stream_source_core.rs"]
mod stream_source_core;

pub(super) use stream_source_core::{
    DecoderFactory, OffsetReader, SharedStream, StreamAudioSource,
};
