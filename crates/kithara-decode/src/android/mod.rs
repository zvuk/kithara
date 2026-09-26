//! Android `MediaCodec` decoding with segment-aware fMP4 and exact MPEG readers.
//! Other standalone containers retain the native extractor's track format.
//! Unsupported formats fail without selecting another decoder backend.

mod backend;
pub(crate) mod codec;
pub(crate) mod demuxer;
pub(crate) mod extractor;

pub(crate) use backend::output_spec;
pub(crate) use codec::AndroidCodec;
pub(crate) use demuxer::AndroidMediaExtractorDemuxer;
