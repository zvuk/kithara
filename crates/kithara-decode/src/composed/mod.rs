#[cfg(test)]
use fixtures::{
    counting as test_counting_codec, drain as test_eof_drain_codec, frame as test_frame,
    stub as test_stub_codec,
};
#[cfg(test)]
use kithara_platform::time::Duration;
#[cfg(test)]
use kithara_signal::AudioSpec;
#[cfg(all(test, any(feature = "symphonia", android_backend)))]
use kithara_stream::PendingReason;
#[cfg(test)]
use kithara_stream::{AudioCodec, NotReadyCause};
#[cfg(test)]
use kithara_test_utils::kithara;
pub(crate) use state::{ComposedDecoder, DecoderRuntime};
use state::{HeadStrip, PcmOriginState, Prefill};
#[cfg(test)]
use test_frame::write_silent_test_frame;
#[cfg(test)]
use trim::frames_to_trim;

#[cfg(test)]
use crate::{PcmOrigin, demuxer::Demuxer, error::DecodeResult, traits::DecoderChunkOutcome};
#[cfg(test)]
use crate::{consts, traits::DecoderSeekOutcome};
#[cfg(all(test, any(feature = "symphonia", android_backend)))]
use crate::{error::DecodeError, traits::Decoder};

mod decoder;
mod state;
mod trim;

#[cfg(test)]
#[macro_use]
mod fixtures;
#[cfg(test)]
#[cfg(any(feature = "symphonia", android_backend))]
mod default_priming_tests;
#[cfg(test)]
mod eof_drain_tests;
#[cfg(test)]
mod hook_tests;
#[cfg(test)]
mod pool_budget_tests;
#[cfg(test)]
mod seek_trim_tests;
#[cfg(test)]
#[cfg(any(feature = "symphonia", android_backend))]
mod smoke_tests;

#[cfg(test)]
mod media_time_tests;
#[cfg(test)]
pub(crate) mod origin_tests;
