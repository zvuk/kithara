use std::{fmt, num::NonZeroU32, path::PathBuf};

use bon::Builder;
#[cfg(feature = "gui")]
use kithara::ui::source::UiConfig;
use kithara::{
    analysis::BeatAnalysisConfig,
    audio::AudioConfigPatch,
    drm::KeyProcessorRegistry,
    file::FileConfigPatch,
    hls::HlsConfigPatch,
    platform::{CancelToken, sync::Arc},
    play::{PlayerConfigPatch, policy::DomainKeyPolicy},
    prelude::PlaybackResamplerBackend,
    queue::QueueConfigPatch,
    stream::dl::Downloader,
    worker::{DispatcherConfigPatch, Worker},
};
use kithara_macros::Patch;
use url::Url;

#[cfg(feature = "broadcast")]
use crate::pools::AppPools;
use crate::{
    pools::{AppStore, AppWorker},
    theme::Palette,
};

#[cfg(feature = "broadcast")]
/// Feature-selected live broadcast configuration.
pub type AppBroadcastConfig = kithara::broadcast::BroadcastConfig<AppPools>;
#[cfg(not(feature = "broadcast"))]
/// Empty broadcast configuration for builds without the service.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct AppBroadcastConfig;

/// App-owned snapshot of one DRM policy and its ordinary resolver registry.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(opt_in, get)]
pub struct AppDrm {
    policy: Arc<DomainKeyPolicy>,
    #[field(get)]
    registry: KeyProcessorRegistry,
}

impl AppDrm {
    /// Register one immutable domain policy and retain the same policy for
    /// resource-header selection.
    #[must_use]
    pub fn new(policy: DomainKeyPolicy) -> Self {
        let policy = Arc::new(policy);
        let mut registry = KeyProcessorRegistry::new();
        registry.register(policy.clone());
        Self { policy, registry }
    }

    /// Return resource headers selected by the same registered policy.
    #[must_use]
    pub fn resource_headers(&self, url: &Url) -> Option<kithara::net::Headers> {
        self.policy.resource_headers(url)
    }
}

/// Application configuration passed to the GUI frontend.
///
/// Shared owners and the downloader are mandatory; product knobs carry the
/// crate's own defaults, which the configuration document patches through
/// [`AppConfigPatch`].
#[derive(Clone, Builder, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppConfig {
    /// App-owned DRM policy and its opaque key-request registry.
    #[patch(skip)]
    pub drm: AppDrm,
    /// App-wide shared asset store.
    #[patch(skip)]
    pub store: AppStore,
    /// Source beat-analysis tunables.
    #[builder(default)]
    #[patch(skip)]
    pub beat_analysis: BeatAnalysisConfig<PlaybackResamplerBackend>,
    /// Fixed source duration covered by one progressive analysis chunk.
    #[builder(default = NonZeroU32::new(16).unwrap_or(NonZeroU32::MIN))]
    pub analysis_chunk_seconds: NonZeroU32,
    /// One playback worker shared by every deck in this app session.
    #[patch(skip)]
    pub worker: AppWorker,
    /// Optional base runtime shared by playback, analysis, and app-owned
    /// background dispatchers. Production supplies one; focused consumers may
    /// let each domain worker own its standalone base.
    #[patch(skip)]
    pub base_worker: Option<Worker>,
    /// App master cancel. Single owner for the whole app subtree; the
    /// queue, player, stores, and UI listener all derive children from
    /// it (see `main.rs`). The chain flag reaches the playback worker and HLS
    /// coord lock-free `is_cancelled()` reads; every subsystem derives its
    /// own [`CancelToken::child`] from this consumer-top master.
    #[patch(skip)]
    pub shutdown: CancelToken,
    /// Shared HTTP downloader for every track.
    #[patch(skip)]
    pub downloader: Downloader,
    /// Color palette for the UI.
    #[builder(default)]
    #[patch(skip)]
    pub palette: Palette,
    /// What the document's `audio:` section says about every track's audio
    /// pipeline, carried as a patch because no `AudioConfig` exists until a
    /// track does. Reached through `audio`, not through [`AppConfigPatch`].
    #[builder(default)]
    #[patch(skip)]
    pub audio: AudioConfigPatch,
    /// What the document's `hls:` section says about every HLS track. Carried
    /// as a patch for the same reason [`AppConfig::audio`] is.
    #[builder(default)]
    #[patch(skip)]
    pub hls: HlsConfigPatch,
    /// What the document's `file:` section says about every file track.
    /// Carried as a patch for the same reason [`AppConfig::audio`] is.
    #[builder(default)]
    #[patch(skip)]
    pub file: FileConfigPatch,
    /// UI-level knobs threaded into every compiled document, including the
    /// draw-pool limits [`UiConfig::draw_buffers`] is built from. A document
    /// reaches these through `ui` and `draw_pool` — the same as
    /// [`AppConfig::audio`] is reached through `audio` — not through
    /// [`AppConfigPatch`].
    #[cfg(feature = "gui")]
    #[builder(default)]
    #[patch(skip)]
    pub ui: UiConfig,
    /// Log filter directives.
    #[builder(default)]
    pub log_directives: Vec<String>,
    /// Audio file URLs or paths to play.
    #[builder(default)]
    #[patch(skip)]
    pub tracks: Vec<String>,
    /// Accept invalid TLS certificates. Test servers only.
    #[builder(default = false)]
    #[patch(skip)]
    pub should_accept_invalid_certs: bool,
    /// What the document's `player:` section says about every deck's player,
    /// carried as a patch because no `PlayerConfig` exists until a deck does.
    /// Reached through `player`, not through [`AppConfigPatch`].
    #[builder(default)]
    #[patch(skip)]
    pub player: PlayerConfigPatch,
    /// Complete live-broadcast construction config for this app session. The
    /// document's `broadcast:` section is applied to it in `main`, where the
    /// worker and pools it is built from exist; nothing here carries a second
    /// spelling of those knobs.
    #[patch(skip)]
    pub broadcast: Option<AppBroadcastConfig>,
    /// Upper bound on waveform buckets (native = one per FFT window). Only
    /// caps very long tracks, to bound the cached blob.
    #[builder(default = 96_000)]
    pub waveform_max_buckets: usize,
    /// Band count of the EQ layout every deck's player graph is built with.
    #[builder(default = 3)]
    pub eq_bands: usize,
    /// Output rate this application asks its audio session for. `None` leaves
    /// `HostConfig`'s own default standing: the Host owns the product default
    /// and refuses a player whose rate disagrees, so this names an override
    /// and every deck's player still reads the rate back off the Host.
    pub sample_rate: Option<NonZeroU32>,
    /// Native output callback size the audio session is asked for. `None`
    /// leaves the backend's own block size in place.
    pub output_block_frames: Option<NonZeroU32>,
    /// Where this application reads its UI package from. What is found there
    /// is laid over the documents this build carries, so the interface can be
    /// changed without a rebuild. A path that does not exist means no package
    /// was laid out and the build's own documents draw; `None` means this
    /// configuration names no package at all.
    #[patch(skip)]
    pub ui_package: Option<PathBuf>,
    /// What the document's `queue:` section says about every deck's queue,
    /// carried as a patch for the same reason [`AppConfig::player`] is.
    #[builder(default)]
    #[patch(skip)]
    pub queue: QueueConfigPatch,
    /// What the document's `dispatcher:` section says about the background
    /// dispatchers the app builds, carried as a patch for the same reason
    /// [`AppConfig::player`] is: each construction site keeps its own thread
    /// name and lays this over the rest.
    #[builder(default)]
    #[patch(skip)]
    pub dispatcher: DispatcherConfigPatch,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut builder = f.debug_struct("AppConfig");
        builder
            .field("drm", &self.drm)
            .field("palette", &self.palette)
            .field("log_directives", &self.log_directives)
            .field("tracks", &self.tracks)
            .field("worker", &self.worker)
            .field(
                "base_worker_cancelled",
                &self.base_worker.as_ref().map(Worker::is_cancelled),
            )
            .field(
                "should_accept_invalid_certs",
                &self.should_accept_invalid_certs,
            )
            .field("player", &self.player)
            .field("broadcast", &self.broadcast)
            .field("waveform_max_buckets", &self.waveform_max_buckets)
            .field("eq_bands", &self.eq_bands)
            .field("sample_rate", &self.sample_rate)
            .field("output_block_frames", &self.output_block_frames)
            .field("beat_analysis", &self.beat_analysis)
            .field("analysis_chunk_seconds", &self.analysis_chunk_seconds)
            .field("audio", &self.audio)
            .field("hls", &self.hls)
            .field("file", &self.file)
            .field("queue", &self.queue)
            .field("dispatcher", &self.dispatcher);
        #[cfg(feature = "gui")]
        builder.field("ui", &self.ui);
        builder.finish_non_exhaustive()
    }
}
