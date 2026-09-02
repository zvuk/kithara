use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_events::EventBus;
use kithara_platform::CancelToken;
use kithara_resampler::{NoResamplerBackend, ResamplerBackend};
use kithara_stream::{MediaInfo, StreamType};

use crate::{pipeline::config::AudioDecoderConfig, traits::AudioObserver};

struct Consts;

impl Consts {
    /// Chunks buffered before preload readiness is signalled.
    const PRELOAD_CHUNKS: usize = 3;
}

/// The consumer's thread capability: how it wakes the decode worker after
/// draining its ring, and how its reader-born events reach the bus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConsumerWakeMode {
    /// Arm a coalesced scheduler pass without signaling a thread gate.
    #[default]
    RealtimeDeferred,
    /// Unpark the worker's thread, for a consumer off the real-time thread.
    /// Marks the consumer's read path as free to block, so reader-born events
    /// publish inline instead of waiting for a scheduler-shell flush.
    ImmediateOffRt,
}

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
#[derive(Builder, fieldwork::Fieldwork)]
#[builder(start_fn = for_stream)]
#[non_exhaustive]
#[fieldwork(opt_in, get)]
pub struct AudioConfig<T: StreamType, B = NoResamplerBackend> {
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    #[builder(start_fn)]
    #[field(get)]
    pub(crate) stream: T::Config,
    /// Decoder construction settings, including decoder-side resampling.
    #[builder(default)]
    #[field(get)]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// Consumer wake capability for ring pops and reader-event delivery.
    /// `block_on_underrun` always resolves the effective mode to
    /// [`ConsumerWakeMode::ImmediateOffRt`].
    #[builder(default)]
    #[field(get, copy)]
    pub(crate) consumer_wake_mode: ConsumerWakeMode,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = NonZeroUsize::new(Consts::PRELOAD_CHUNKS).expect("preload chunk count is non-zero"))]
    #[field(get, copy)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[builder(name = events)]
    pub(crate) bus: Option<EventBus>,
    /// Master cancel token for the audio pipeline.
    pub(crate) cancel: Option<CancelToken>,
    /// Optional format hint (file extension like "mp3", "wav")
    pub(crate) hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    #[field(get, copy)]
    pub(crate) host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub(crate) media_info: Option<MediaInfo>,
    /// Optional bounded, nonblocking observer of decoder-output PCM.
    /// [`kithara_signal::AudioChunk::meta`] describes its post-conversion format;
    /// it runs before playback effects and owns any asynchronous copy.
    pub(crate) observer: Option<Box<dyn AudioObserver>>,
    /// Make a producer-ring underrun block (engine-aware park) instead of
    /// surfacing an empty outcome. Offline (faster-than-real-time) consumers
    /// opt in so `read` / `next_chunk` wait for the decode worker instead of
    /// returning `Pending` / zero frames the caller would have to sleep-poll.
    /// Real-time hosts must keep the default (`false`): the audio callback
    /// can never block.
    #[builder(default)]
    #[field(get)]
    pub(crate) block_on_underrun: bool,
    /// Explicit PCM ring depth in producer chunks. `None` lets the playback
    /// composition root resolve the depth from its render geometry.
    #[field(get, copy)]
    pub(crate) audio_buffer_chunks: Option<usize>,
}

impl<T, B> AudioConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    /// Return the configured event bus.
    #[must_use]
    pub const fn bus(&self) -> Option<&EventBus> {
        self.bus.as_ref()
    }

    /// Return the configured cancellation token.
    #[must_use]
    pub const fn cancel(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Return the optional format hint.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Return the media information hint.
    #[must_use]
    pub const fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }
}
