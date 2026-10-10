#![forbid(unsafe_code)]

//! Unified facade for audio streaming and decoding through `Resource`.
//! `ResourceConfig` combines a source with the caller's `AssetStore`, typed pools
//! and `PlayWorker`. `Resource::open` opens decoded interleaved PCM through the
//! common `AudioReader` read/seek interface; `ReadOutcome` distinguishes frames,
//! pending work and end of input.

#[cfg(feature = "audio")]
pub mod audio {
    pub use kithara_audio::*;
}

#[cfg(feature = "analysis")]
pub mod analysis {
    pub use kithara_analysis::*;
}

#[cfg(feature = "waveform")]
pub mod waveform {
    pub use kithara_waveform::*;
}

#[cfg(feature = "beat")]
pub mod beat {
    pub use kithara_beat::*;
}

#[cfg(feature = "broadcast")]
pub mod broadcast {
    pub use kithara_broadcast::*;
}

#[cfg(feature = "bufpool")]
pub mod bufpool {
    pub use kithara_bufpool::*;
}

#[cfg(feature = "decode")]
pub mod decode {
    pub use kithara_decode::*;
}

#[cfg(feature = "dsp")]
pub mod dsp {
    pub use kithara_dsp::*;
}

#[cfg(feature = "effects")]
pub mod effects {
    pub use kithara_effects::*;
}

#[cfg(feature = "encode")]
pub mod encode {
    pub use kithara_encode::*;
}

#[cfg(feature = "output")]
pub mod output {
    pub use kithara_output::*;
}

#[cfg(feature = "record")]
pub mod record {
    pub use kithara_record::*;
}

#[cfg(feature = "events")]
pub mod events {
    pub use kithara_events::*;
}

#[cfg(feature = "host")]
pub mod host {
    pub use kithara_host::*;
}

#[cfg(feature = "platform")]
pub mod platform {
    pub use kithara_platform::*;
}

#[cfg(feature = "play")]
pub mod play {
    pub use kithara_play::*;
}

#[cfg(feature = "resampler")]
pub mod resampler {
    pub use kithara_resampler::*;
}

#[cfg(feature = "signal")]
pub mod signal {
    pub use kithara_signal::*;
}

#[cfg(feature = "queue")]
pub mod queue {
    pub use kithara_queue::*;
}

#[cfg(feature = "link")]
pub mod link {
    pub use kithara_link::*;
}

#[cfg(feature = "download")]
pub mod download {
    pub use kithara_download::*;
}

#[cfg(feature = "stream")]
pub mod stream {
    pub use kithara_stream::*;
}

#[cfg(feature = "stretch")]
pub mod stretch {
    pub use kithara_stretch::*;
}

#[cfg(feature = "ui")]
pub mod ui {
    pub use kithara_ui::*;
}

#[cfg(feature = "usdt")]
pub mod usdt {
    pub use kithara_test_utils::probe::operation_id;
}

#[cfg(feature = "warp")]
pub mod warp {
    pub use kithara_warp::*;
}

#[cfg(feature = "worker")]
pub mod worker {
    pub use kithara_worker::*;
}

#[cfg(feature = "file")]
pub mod file {
    pub use kithara_file::*;
}

#[cfg(feature = "abr")]
pub mod abr {
    pub use kithara_abr::*;
}

#[cfg(feature = "drm")]
pub mod drm {
    pub use kithara_drm::*;
}

#[cfg(feature = "hls")]
pub mod hls {
    pub use kithara_hls::*;
}

#[cfg(feature = "assets")]
pub mod assets {
    pub use kithara_assets::*;
}

#[cfg(feature = "net")]
pub mod net {
    pub use kithara_net::*;
}

#[cfg(feature = "storage")]
pub mod storage {
    pub use kithara_storage::*;
}

pub use kithara_test_macros::{
    allow_block, fixture, mock, no_block, rtsan_forbid_blocking, test, test_utils_flash as flash,
};
#[cfg(feature = "warp")]
pub use kithara_warp::{GridSegment, RegionPlan, RegionPlanError, StretchKind};

#[cfg(feature = "mock")]
pub mod mock {
    #[cfg(feature = "audio")]
    pub use kithara_audio::mock::*;
    #[cfg(feature = "beat")]
    pub use kithara_beat::{BeatGridModel, BeatGridState, Meter};
    #[cfg(feature = "decode")]
    pub use kithara_decode::mock::*;
    #[cfg(feature = "play")]
    pub use kithara_play::mock::*;
    #[cfg(feature = "stream")]
    pub use kithara_stream::mock::*;
}

/// Prelude — flat imports for common types.
pub mod prelude {
    #[cfg(feature = "abr")]
    pub use kithara_abr::AbrMode;
    #[cfg(feature = "analysis")]
    pub use kithara_analysis::{
        AnalysisDemand, AnalysisToken, AnalysisWorker, AnalysisWorkerConfig, TrackAnalysis,
    };
    #[cfg(feature = "assets")]
    pub use kithara_assets::{AssetStore, StorageBackend};
    #[cfg(feature = "audio")]
    pub use kithara_audio::{
        Audio, AudioConfig, AudioControl, AudioEvent, AudioRead, AudioReader, AudioSession,
        ResamplerQuality,
    };
    #[cfg(feature = "beat")]
    pub use kithara_beat::{BeatGridModel, BeatGridState, Meter};
    #[cfg(feature = "decode")]
    pub use kithara_decode::{DecodeError, DecodeResult, DecoderTrackInfo, TrackMetadata};
    #[cfg(feature = "download")]
    pub use kithara_download::{Downloader, DownloaderConfig};
    #[cfg(feature = "events")]
    pub use kithara_events::{BusScope, Event, EventBus, EventReceiver};
    #[cfg(feature = "file")]
    pub use kithara_file::{File, FileConfig, FileEvent};
    #[cfg(feature = "hls")]
    pub use kithara_hls::{Hls, HlsConfig, HlsEvent};
    #[cfg(feature = "host")]
    pub use kithara_host::{Host, HostConfig, TransportEvent};
    #[cfg(feature = "play")]
    pub use kithara_play::{
        ArtifactSource, EngineLoadSnapshot, PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend,
        PlayerConfig, PlayerImpl, Resource, ResourceConfig, ResourceSrc, ServiceClass, SourceType,
    };
    #[cfg(feature = "queue")]
    pub use kithara_queue::{Queue, QueueConfig, QueueEvent, TrackEntry, TrackSource};
    #[cfg(feature = "signal")]
    pub use kithara_signal::{AudioChunkInfo, AudioSpec};
    #[cfg(feature = "storage")]
    pub use kithara_storage::{OpenMode, StorageError, StorageResource};
    #[cfg(feature = "stream")]
    pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType};
    #[cfg(feature = "stretch")]
    pub use kithara_stretch::{ElasticConfig, ElasticEngine, StretchKind as StretchEngineKind};
    #[cfg(feature = "warp")]
    pub use kithara_warp::{GridSegment, RegionPlan, RegionPlanError, StretchKind};
    #[cfg(feature = "waveform")]
    pub use kithara_waveform::{Bucket, Waveform};
}
