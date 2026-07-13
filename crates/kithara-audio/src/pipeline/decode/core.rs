use std::sync::atomic::{AtomicU32, Ordering};

use kithara_decode::{DecodeError, Decoder, GaplessMode};
use kithara_platform::sync::Arc;
use kithara_stream::{MediaInfo, StreamType};

use crate::pipeline::{gapless::GaplessStage, stream::shared::SharedStream};

/// Factory closure that creates a new decoder from stream, media info, and base offset.
///
/// Production creates a Symphonia decoder via `OffsetReader`; tests may return
/// a mock decoder without real I/O. Interrupted construction remains distinct
/// from a hard decoder or codec error so recreation can wait for source bytes.
pub(crate) type DecoderFactory<T> = Arc<
    dyn Fn(SharedStream<T>, MediaInfo, u64) -> Result<Box<dyn Decoder>, DecodeError> + Send + Sync,
>;

/// Decoder construction state shared by initial installation and later rebuilds.
pub(crate) struct DecodeInit<T: StreamType> {
    pub(crate) decoder: Box<dyn Decoder>,
    pub(crate) decoder_factory: DecoderFactory<T>,
    pub(crate) gapless_mode: GaplessMode,
    pub(crate) host_sample_rate: Arc<AtomicU32>,
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) recreate_on_host_rate_change: bool,
}

impl<T: StreamType> DecodeInit<T> {
    pub(crate) fn build_gapless(&self) -> GaplessStage {
        GaplessStage::build(
            self.decoder.as_ref(),
            self.gapless_mode,
            self.media_info.as_ref(),
        )
    }

    pub(crate) fn decoder_host_sample_rate(&self) -> u32 {
        self.host_sample_rate.load(Ordering::Acquire)
    }
}
