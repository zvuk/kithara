use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[cfg(feature = "broadcast")]
use kithara::broadcast::BroadcastConfigPatch;
#[cfg(feature = "gui")]
use kithara::bufpool::PoolError;
#[cfg(feature = "gui")]
use kithara::ui::{
    draw::DrawBuffers,
    source::{DrawPoolLimits, UiConfig},
};
use kithara::{
    analysis::{BeatAnalysisConfig, BeatAnalysisConfigPatchError},
    assets::{AssetLayoutRegistry, AssetStoreConfigPatch, FlushPolicyPatch, StorageBackend},
    audio::AudioConfigPatch,
    download::DownloaderConfigPatch,
    file::FileConfigPatch,
    hls::HlsConfigPatch,
    net::NetOptionsPatch,
    play::{
        PlayWorkerConfigPatch, PlaybackResamplerBackend, PlayerConfigPatch, policy::DomainKeyPolicy,
    },
    queue::QueueConfigPatch,
    worker::{DispatcherConfigPatch, WorkerConfigPatch},
};
use serde_yaml_ng::Value;

use super::{
    env::{MissingEnv, expand},
    layouts::asset_layouts,
    merge::merge,
    policy::{PolicyError, drm_policy},
    schema::Document,
};
use crate::{
    baked::{BAKED_DOCUMENT, secret},
    config::AppConfigPatch,
    pools::PoolsSection,
};

/// Path the baked document is reported under in parse errors.
const BAKED_PATH: &str = "<baked app.yaml>";

/// The configuration this process runs on, and the document it came from.
#[derive(Clone)]
#[non_exhaustive]
#[derive(derive_more::Debug)]
pub struct Config {
    #[debug(skip)]
    document: Document,
    /// The merged document before expansion. Kept so a dump can print
    /// references rather than the secrets behind them.
    source: Value,
}

/// Why a document could not be turned into a configuration.
#[derive(Debug, derive_more::Display, derive_more::Error)]
#[error(ignore)]
#[non_exhaustive]
pub enum LoadError {
    /// A path the operator named does not exist.
    #[display("configuration file not found: {}", _0.display())]
    Missing(PathBuf),
    /// A document could not be read from disk.
    #[display("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[error(source)]
        source: io::Error,
    },
    /// A document's text is not YAML.
    #[display("cannot parse {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[error(source)]
        source: serde_yaml_ng::Error,
    },
    /// A document does not match the schema -- either the merged tree, or an
    /// overlay whose root is not a mapping, refused before any merge.
    #[display("cannot parse {resource}: {detail}")]
    Schema { resource: String, detail: String },
    /// A reference the document names resolved nowhere.
    #[display("{_0}")]
    Env(#[error(source)] MissingEnv),
}

impl Config {
    /// Knobs the document's `app:` section sets on the built [`AppConfig`].
    ///
    /// [`AppConfig`]: crate::config::AppConfig
    #[must_use]
    pub fn app(&self) -> AppConfigPatch {
        self.document.app.clone()
    }

    /// The media-identity registry the asset store reads.
    #[must_use]
    pub fn asset_layouts(&self) -> AssetLayoutRegistry {
        asset_layouts(&self.document.assets)
    }

    /// Knobs the document sets on the asset store. A document that names no
    /// backend resolves to [`StorageBackend::default`] — a stable root under
    /// the system temp directory — and deliberately not to
    /// `AssetStore::open`'s own fallback, which is a fresh unique directory
    /// per launch and would move the on-disk cache every run.
    #[must_use]
    pub fn assets_store(&self) -> AssetStoreConfigPatch {
        let mut store = self.document.assets_store.clone();
        store
            .backend
            .get_or_insert_with(|| Some(StorageBackend::default()));
        store
    }

    /// Knobs the document sets on every track's audio pipeline. `audio:`,
    /// `hls:`, and `file:` are the document's only spelling for the three
    /// configurations a track is opened with -- `resource.audio`,
    /// `resource.hls`, and `resource.file` are refused -- and each rides to
    /// its construction site as a patch, because none of those configurations
    /// exists until a track does.
    #[must_use]
    pub fn audio(&self) -> AudioConfigPatch {
        self.document.audio.clone()
    }

    /// What the document's `beat:` section says about source beat analysis,
    /// merged onto the crate's own defaults. The backend the analyzer resamples
    /// through is the caller's, never a document key.
    ///
    /// # Errors
    /// Returns the [`BeatAnalysisConfigPatchError`] the merged policy was
    /// refused with, naming the section that carried the refused value, rather
    /// than searching a tempo band the detector never scores.
    pub fn beat(
        &self,
    ) -> Result<BeatAnalysisConfig<PlaybackResamplerBackend>, BeatAnalysisConfigPatchError> {
        let mut config = BeatAnalysisConfig::default();
        config.apply(self.document.beat.clone())?;
        Ok(config)
    }

    /// Knobs the document sets on this session's live broadcast. Applied at
    /// the construction site in `main`, where the worker and pools a
    /// `BroadcastConfig` is built from exist.
    #[cfg(feature = "broadcast")]
    #[must_use]
    pub fn broadcast(&self) -> BroadcastConfigPatch {
        self.document.broadcast.clone()
    }

    /// Knobs the document sets on the background dispatchers the app builds.
    /// Applied at each construction site, which keeps its own thread name.
    #[must_use]
    pub fn dispatcher(&self) -> DispatcherConfigPatch {
        self.document.dispatcher.clone()
    }

    /// Knobs the document sets on the shared downloader, the ABR controller
    /// it owns among them: `abr_settings` is a nested patch, so
    /// `downloader.abr_settings` is the document's only spelling for
    /// `kithara-abr`'s configuration. Applied at the construction site in
    /// `main`, where the HTTP client the downloader is built around exists.
    #[must_use]
    pub fn downloader(&self) -> DownloaderConfigPatch {
        self.document.downloader.clone()
    }

    /// The DRM policy the key registry resolves through.
    ///
    /// # Errors
    /// Returns an error when a provider declares a policy that cannot be
    /// honoured -- a reserved header, a salt of zero length, or a hex salt
    /// of odd length.
    pub fn drm_policy(&self) -> Result<DomainKeyPolicy, PolicyError> {
        drm_policy(&self.document.drm)
    }

    /// The effective configuration as a document. Printed before expansion, so
    /// a dump names `$KITHARA_...` rather than handing out the secret behind it.
    #[must_use]
    pub fn dump(&self) -> String {
        serde_yaml_ng::to_string(&self.source)
            .unwrap_or_else(|e| format!("cannot render the configuration: {e}"))
    }

    /// Knobs the document sets on every file track's stream.
    #[must_use]
    pub fn file(&self) -> FileConfigPatch {
        self.document.file.clone()
    }

    /// Knobs the document sets on the asset store's flush policy.
    #[must_use]
    pub fn flush(&self) -> FlushPolicyPatch {
        self.document.flush.clone()
    }

    /// Knobs the document sets on every HLS track's stream.
    #[must_use]
    pub fn hls(&self) -> HlsConfigPatch {
        self.document.hls.clone()
    }

    /// Read the configuration: the baked document, an overlay laid on top, then
    /// environment references expanded over the result.
    ///
    /// `explicit` is a path the operator named and must exist; `beside` is the
    /// conventional file next to the executable and may be absent.
    ///
    /// # Errors
    /// Returns [`LoadError`] when a named file is missing or unreadable, a
    /// document does not match the schema, or a reference resolves nowhere.
    pub fn load(explicit: Option<&Path>, beside: Option<&Path>) -> Result<Self, LoadError> {
        Self::load_with(explicit, beside, &secret)
    }

    /// An explicit `null` for a named key blanks that key, but an empty file at the root is treated
    /// as one left to fill in later rather than an override that wipes the document.
    fn load_with(
        explicit: Option<&Path>,
        beside: Option<&Path>,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, LoadError> {
        let mut source: Value =
            serde_yaml_ng::from_str(BAKED_DOCUMENT).map_err(|source| LoadError::Parse {
                source,
                path: PathBuf::from(BAKED_PATH),
            })?;

        let overlay_path = Self::overlay_path(explicit, beside)?;
        if let Some(path) = overlay_path.as_deref() {
            match Self::read(path)? {
                Value::Null => {}
                over @ Value::Mapping(_) => merge(&mut source, over),
                _ => {
                    return Err(LoadError::Schema {
                        resource: path.display().to_string(),
                        detail: "the root of a configuration document must be a mapping"
                            .to_string(),
                    });
                }
            }
        }

        let mut expanded = source.clone();
        expand(&mut expanded, lookup).map_err(LoadError::Env)?;

        let resource = overlay_path.as_deref().map_or_else(
            || BAKED_PATH.to_string(),
            |path| format!("{BAKED_PATH} merged with {}", path.display()),
        );
        let document = serde_yaml_ng::from_value(expanded).map_err(|_| LoadError::Schema {
            resource,
            detail: schema_detail(&source),
        })?;

        Ok(Self { document, source })
    }

    /// The HTTP options the document names.
    #[must_use]
    pub fn net(&self) -> NetOptionsPatch {
        self.document.net.clone()
    }

    fn overlay_path(
        explicit: Option<&Path>,
        beside: Option<&Path>,
    ) -> Result<Option<PathBuf>, LoadError> {
        if let Some(path) = explicit {
            return if path.exists() {
                Ok(Some(path.to_path_buf()))
            } else {
                Err(LoadError::Missing(path.to_path_buf()))
            };
        }
        Ok(beside.filter(|path| path.exists()).map(Path::to_path_buf))
    }

    /// Knobs the document sets on the one playback worker every deck
    /// shares. Applied at the construction site in `main`, where the pools it
    /// is built from exist.
    #[must_use]
    pub fn play_worker(&self) -> PlayWorkerConfigPatch {
        self.document.play_worker.clone()
    }

    /// Knobs the document sets on the player, threaded into every deck's
    /// `PlayerConfig`.
    #[must_use]
    pub fn player(&self) -> PlayerConfigPatch {
        self.document.player.clone()
    }

    /// Knobs the document sets on the application's buffer pools.
    #[must_use]
    pub fn pools(&self) -> PoolsSection {
        self.document.pools.clone()
    }

    /// Knobs the document sets on the queue.
    #[must_use]
    pub fn queue(&self) -> QueueConfigPatch {
        self.document.queue.clone()
    }

    fn read(path: &Path) -> Result<Value, LoadError> {
        let text = fs::read_to_string(path).map_err(|source| LoadError::Read {
            source,
            path: path.to_path_buf(),
        })?;
        serde_yaml_ng::from_str(&text).map_err(|source| LoadError::Parse {
            source,
            path: path.to_path_buf(),
        })
    }

    /// Tracks the document opens with.
    #[must_use]
    pub fn tracks(&self) -> &[String] {
        &self.document.playlist.tracks
    }

    /// Knobs the document sets on the compiled UI: the crate default, then
    /// the document's `ui:` section, with `draw_buffers` built from the
    /// document's `draw_pool:` section rather than patched on afterwards.
    /// `DrawPoolLimits` only reaches a `DrawBuffers` through
    /// `DrawBuffers::try_new`, so the document's draw-pool limits must be read
    /// before that value is constructed -- see `UiConfig::draw_buffers` and
    /// `DrawPoolLimits` in `kithara-ui`. The composition lives here rather
    /// than at the construction site so a test can reach the same code the
    /// binary runs.
    ///
    /// # Errors
    /// Returns the [`PoolError`] the document's `draw_pool:` section failed
    /// the generated draw-buffer schema with, rather than aborting the
    /// process on a value a configuration document can now name.
    #[cfg(feature = "gui")]
    pub fn ui(&self) -> Result<UiConfig, PoolError> {
        let mut draw_pool = DrawPoolLimits::default();
        draw_pool.apply(self.document.draw_pool.clone());
        let mut config = UiConfig::builder()
            .draw_buffers(DrawBuffers::try_new(draw_pool)?)
            .build();
        config.apply(self.document.ui.clone());
        Ok(config)
    }

    /// Knobs the document sets on the compute worker.
    #[must_use]
    pub fn worker(&self) -> WorkerConfigPatch {
        self.document.worker.clone()
    }
}

/// The message for a schema failure, taken from the pre-expansion tree: the
/// offending position still names its `$KITHARA_...` reference there, which is
/// both what the operator has to fix and safe to log. A failure the expanded
/// tree alone has is reported without the value that caused it.
///
/// This is an error-*reporting* rule, not a state-resolution fallback chain:
/// the loader's value path stays single and unchanged -- `document` above is
/// always built from `expanded`. This second, throwaway deserialization only
/// runs on the error path, to find a safe-to-log message.
fn schema_detail(source: &Value) -> String {
    serde_yaml_ng::from_value::<Document>(source.clone()).map_or_else(
        |error| error.to_string(),
        |_| "a resolved environment value does not match the schema".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        num::{NonZeroU32, NonZeroUsize},
        path::PathBuf,
    };

    use kithara::{
        assets::FlushPolicy,
        hls::SizeProbeMethod,
        host::HostConfig,
        net::{Compression, NetOptions},
        platform::{CancelToken, time::Duration, tokio::runtime::Handle},
        worker::ComputePool,
    };
    use tempfile::TempDir;

    use super::{BAKED_PATH, Config, LoadError, StorageBackend};
    use crate::{
        config::AppConfig,
        pools::{self, AppPools},
        theme::{Palette, Rgb},
    };

    fn tempdir() -> TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    fn write(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(format!("{name}.yaml"));
        fs::write(&path, contents).expect("write the test document");
        path
    }

    /// Answers exactly the references the baked document names, so success-path
    /// tests do not depend on the ambient process environment.
    fn env(name: &str) -> Option<String> {
        match name {
            "KITHARA_DRM_PROD_KEY"
            | "KITHARA_DRM_PROD_AUTH_TOKEN"
            | "KITHARA_DRM_PROD_SP_ZV_TOKEN"
            | "KITHARA_DRM_STAGE_KEY"
            | "KITHARA_DRM_STAGE_AUTH_TOKEN" => Some("test-value".to_string()),
            _ => None,
        }
    }

    #[kithara::test(native, flash(false))]
    fn no_file_leaves_the_baked_document_in_force() {
        let config = Config::load_with(None, None, &env).expect("the baked document stands alone");

        assert_eq!(
            config.hls().size_probe_method,
            Some(SizeProbeMethod::RangeGet),
            "the shipped document selects range_get"
        );
        assert!(!config.tracks().is_empty());

        let crossfade = config
            .player()
            .crossfade_duration
            .expect("the shipped document names a crossfade");
        assert!(
            (crossfade - 5.0).abs() < f32::EPSILON,
            "the shipped document pins the 5-second crossfade against the crate default of 1.0"
        );
    }

    #[kithara::test(native, flash(false))]
    fn the_browser_overlay_leaves_no_provider_and_four_streams() {
        let overlay = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/app.web.yaml"));

        let config = Config::load_with(Some(&overlay), None, &|_| None)
            .expect("the browser document resolves no reference");

        assert!(config.document.drm.providers.is_empty());
        assert_eq!(
            config.tracks(),
            [
                "https://stream.silvercomet.top/track.mp3",
                "https://stream.silvercomet.top/hls/master.m3u8",
                "https://stream.silvercomet.top/drm/master.m3u8",
                "https://stream.silvercomet.top/tones/master.m3u8",
            ]
        );
        let store = config.assets_store();
        assert_eq!(store.backend, Some(Some(StorageBackend::Memory)));
        assert_eq!(
            store
                .cache_capacity
                .map(|value| value.map(NonZeroUsize::get)),
            Some(Some(128))
        );
        assert_eq!(store.max_bytes, Some(Some(128 * 1024 * 1024)));
    }

    #[kithara::test(native, flash(false))]
    fn a_file_overrides_only_what_it_names() {
        let dir = tempdir();
        let path = write(
            &dir,
            "overrides-one-field",
            "hls:\n  size_probe_method: head\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        assert_eq!(config.hls().size_probe_method, Some(SizeProbeMethod::Head));
        assert!(
            !config.tracks().is_empty(),
            "a section the overlay never names keeps its baked value"
        );
    }

    /// `network` moved `compression` to `net`: the overlay's value must reach
    /// the options the application builds, not the crate's own
    /// `NetOptions::compression` default (`Compression::all()`). `ZSTD` alone
    /// is a value that default cannot produce, so a regression that silently
    /// falls back to the crate default is caught rather than matched by
    /// coincidence.
    #[kithara::test(native, flash(false))]
    fn the_net_section_compression_reaches_the_options_the_app_builds() {
        let dir = tempdir();
        let path = write(
            &dir,
            "net-compression-only",
            "net:\n  compression: [zstd]\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        let mut net = NetOptions::builder().build();
        net.apply(config.net());

        assert_eq!(
            net.compression,
            Compression::ZSTD,
            "the document's `net.compression` reaches the options the app builds"
        );
    }

    /// The worker's two keys survive the load pipeline and stay distinct:
    /// `max_compute_tasks` and `pool` reach the application in one patch.
    /// `WorkerConfig`'s own fields are `pub(crate)`, so the application can
    /// only see what the accessor hands it — that the patch then writes the
    /// ceiling and converts the pool is pinned inside `kithara-worker` by
    /// `a_patch_writes_only_the_field_it_names` and
    /// `a_pool_section_carries_the_documents_thread_count_and_name`.
    #[kithara::test(native, flash(false))]
    fn the_worker_keys_survive_the_load_pipeline() {
        let dir = tempdir();
        let path = write(
            &dir,
            "worker-and-pool",
            "worker:\n  max_compute_tasks: 4\n  pool:\n    mode: disabled\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let worker = config.worker();

        assert_eq!(worker.max_compute_tasks.map(NonZeroUsize::get), Some(4));
        assert!(
            matches!(worker.pool, Some(ComputePool::Disabled {})),
            "the accessor hands the application the mode the document named"
        );
    }

    /// A document's `queue` key reaches the accessor unchanged, and a knob it
    /// never names stays absent so the crate default that built `QueueConfig`
    /// stands.
    #[kithara::test(native, flash(false))]
    fn the_queue_section_survives_the_load_pipeline() {
        let dir = tempdir();
        let path = write(&dir, "queue", "queue:\n  max_concurrent_loads: 5\n");

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        assert_eq!(
            config.queue().max_concurrent_loads.map(NonZeroUsize::get),
            Some(5)
        );
        assert!(
            config.queue().max_history_size.is_none(),
            "a knob the document does not name reaches the app empty"
        );
    }

    /// The downloader's own keys and the ABR controller nested under them both
    /// survive the load pipeline. `DownloaderConfig`'s fields are `pub(crate)`,
    /// so the application only sees what the accessor hands it — that the patch
    /// then writes through, nested settings included, is pinned inside
    /// `kithara-stream` by `a_patch_writes_only_the_concurrency_it_names` and
    /// `a_nested_abr_patch_reaches_the_downloader`.
    #[kithara::test(native, flash(false))]
    fn the_downloader_section_carries_its_nested_abr_settings() {
        let dir = tempdir();
        let path = write(
            &dir,
            "downloader-and-abr",
            "downloader:\n  max_concurrent: 8\n  abr_settings:\n    min_switch_interval: 45s\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let downloader = config.downloader();

        assert_eq!(downloader.max_concurrent, Some(8));
        assert_eq!(
            downloader.abr_settings.min_switch_interval,
            Some(Duration::from_secs(45)),
            "`downloader.abr_settings` is the document's only spelling for the ABR controller"
        );
        assert!(
            downloader.soft_timeout.is_none(),
            "a knob the document does not name reaches the app empty"
        );
    }

    /// A document's `flush` key reaches the policy the asset store's hub is
    /// built with, and a knob it never names keeps the crate default.
    #[kithara::test(native, flash(false))]
    fn the_flush_section_reaches_the_policy_the_app_builds() {
        let dir = tempdir();
        let path = write(&dir, "flush", "flush:\n  debounce: 250ms\n");

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        let mut policy = FlushPolicy::default();
        policy.apply(config.flush());

        assert_eq!(policy.debounce, Duration::from_millis(250));
        assert_eq!(
            policy.force_every_n_ops,
            FlushPolicy::default().force_every_n_ops,
            "a knob the document does not name keeps the crate default"
        );
    }

    /// The three sections a track is opened with each carry their own patch,
    /// and the baked `hls.size_probe_method` an overlay never names survives
    /// alongside them.
    #[kithara::test(native, flash(false))]
    fn the_audio_hls_and_file_sections_each_reach_their_own_patch() {
        let dir = tempdir();
        let path = write(
            &dir,
            "audio-and-file",
            "audio:\n  preload_chunks: 7\nfile:\n  reader_event_capacity: 512\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        assert_eq!(config.audio().preload_chunks, NonZeroUsize::new(7));
        assert_eq!(config.file().reader_event_capacity, Some(512));
        assert_eq!(
            config.hls().size_probe_method,
            Some(SizeProbeMethod::RangeGet),
            "a section the overlay never names keeps its baked value"
        );
    }

    /// `ui` and `draw_pool` are separate document sections because
    /// `UiConfig.draw_buffers` is a *built* value: `DrawPoolLimits` only
    /// reaches it through `DrawBuffers::try_new`, so `Config::ui`
    /// reads `draw_pool` before building it rather than patching `ui` onto
    /// the result afterwards. This test proves that both sections reach one
    /// `UiConfig` through that same code -- the code the binary runs --
    /// using `131072`, `4`, and `7`, values no crate default produces
    /// (`max_arena_bytes` defaults to 65536, `max_buffers` to 64,
    /// `command_capacity` to 512). It does not prove that an unnamed field
    /// keeps a *merge-seeded* value rather than a whole-struct reset: the
    /// base `Config::ui` applies onto is `DrawPoolLimits::default`
    /// itself, so a reset and a real merge are indistinguishable at this
    /// site. That property is proved separately, by the seeded unit tests in
    /// `kithara-ui`'s `source::config` module, which apply a patch onto a
    /// value they seeded themselves.
    #[cfg(feature = "gui")]
    #[kithara::test(native, flash(false))]
    fn the_ui_and_draw_pool_sections_compose_one_ui_config() {
        let dir = tempdir();
        let path = write(
            &dir,
            "ui-and-draw-pool",
            "ui:\n  max_arena_bytes: 131072\ndraw_pool:\n  max_buffers: 4\n  command_capacity: 7\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let ui = config
            .ui()
            .unwrap_or_else(|error| panic!("the document's draw-pool limits must build: {error}"));

        assert_eq!(ui.max_arena_bytes, 131_072);
        assert_eq!(ui.draw_buffers.limits().max_buffers, 4);
        assert_eq!(ui.draw_buffers.limits().command_capacity, 7);
        assert_eq!(
            ui.draw_buffers.limits().path_capacity,
            128,
            "a default-vs-default check: the document never names path_capacity, and \
             DrawPoolLimits::default already produces 128, so this does not tell a merge \
             apart from a reset"
        );
    }

    /// The document's `beat:` section reaches the analyzer configuration the
    /// application hands the analysis service. `22_050` is the crate's own
    /// detector rate, so the overlay names one it cannot produce by accident.
    #[kithara::test(native, flash(false))]
    fn the_beat_section_reaches_the_analyzer_configuration() {
        let dir = tempdir();
        let path = write(&dir, "beat-target-rate", "beat:\n  target_rate: 32000\n");

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let beat = config
            .beat()
            .unwrap_or_else(|error| panic!("the document's beat section must merge: {error}"));

        assert_eq!(beat.target_rate, 32_000);
        assert_eq!(
            beat.detector_window_seconds, 30,
            "a key the document never names keeps the analyzer's own default"
        );
    }

    /// A tempo band the periodicity comb never scores stops the launch under
    /// the key that carried it rather than being clamped into something the
    /// detector can search.
    #[cfg(feature = "beat-dsp")]
    #[kithara::test(native, flash(false))]
    fn a_beat_section_the_detector_refuses_names_the_key_that_carried_it() {
        let dir = tempdir();
        let path = write(
            &dir,
            "beat-unscored-band",
            "beat:\n  tempo:\n    low: 30.0\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        let error = config
            .beat()
            .expect_err("a band past the scored hypotheses must not reach the detector");

        assert!(
            format!("{error}").starts_with("tempo: "),
            "the refusal names the section that carried it, read as {error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn the_pools_section_survives_the_load_pipeline() {
        let dir = tempdir();
        let path = write(
            &dir,
            "pools",
            "pools:\n  budget_bytes: 1048576\n  bytes:\n    max_buffers: 64\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let section = config.pools();

        assert_eq!(section.budget_bytes, Some(1_048_576));
        assert_eq!(section.bytes.max_buffers, Some(64));
        assert!(
            section.samples.max_buffers.is_none(),
            "a pool the document does not name reaches the builder empty"
        );
    }

    #[kithara::test(native, flash(false))]
    fn the_assets_store_section_survives_the_load_pipeline() {
        let dir = tempdir();
        let path = write(
            &dir,
            "assets-store",
            "assets_store:\n  cache_capacity: 32\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let settings = config.assets_store();

        assert_eq!(
            settings
                .cache_capacity
                .map(|value| value.map(NonZeroUsize::get)),
            Some(Some(32))
        );
        assert!(
            settings.max_bytes.is_none(),
            "a knob the document does not name reaches the app empty"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_silent_document_leaves_the_store_on_the_stable_default_root() {
        let dir = tempdir();
        let path = write(&dir, "silent-store", "assets_store:\n  max_assets: 8\n");

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        assert_eq!(
            config.assets_store().backend,
            Some(Some(StorageBackend::default())),
            "an unnamed backend must resolve to the stable default root, not \
             to the fresh per-launch temp directory `AssetStore::open` falls \
             back to on its own"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_document_that_names_a_backend_gets_that_one() {
        let dir = tempdir();
        let path = write(
            &dir,
            "named-store",
            "assets_store:\n  backend:\n    kind: memory\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        assert_eq!(
            config.assets_store().backend,
            Some(Some(StorageBackend::Memory))
        );
    }

    /// The Host owns the output rate -- it refuses a player whose rate
    /// disagrees, and `Deck::build` reads every player's rate back off
    /// `Host::requested_sample_rate` -- so `PlayerConfig::sample_rate` carries
    /// `#[patch(skip)]` and the document names the rate once. It names it
    /// under `app`, not `host`: `HostConfig` is a session-mode enum with no
    /// patch of its own, so `main` reads the key off the built `AppConfig` and
    /// hands it to the Host builder. Seeded off the Host's own default (44100)
    /// so the assertion cannot pass on a key that never arrived.
    #[kithara::test(native, flash(false))]
    fn a_document_names_the_output_rate_on_the_app_section() {
        let dir = tempdir();
        let path = write(&dir, "output-rate", "app:\n  sample_rate: 48000\n");

        let document = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        let host = HostConfig::<AppPools>::builder()
            .maybe_sample_rate_hint(document.app().sample_rate.flatten())
            .build();

        assert_eq!(host.sample_rate().get(), 48_000);
    }

    fn assembled(dir: &TempDir, name: &str, app: &str, shutdown: &CancelToken) -> AppConfig {
        let path = write(
            dir,
            name,
            &format!("assets_store:\n  backend:\n    kind: memory\n{app}"),
        );
        let document = Config::load_with(Some(&path), None, &env).expect("the overlay loads");
        AppConfig::assemble()
            .document(&document)
            .pools(pools::build(&document.pools()).expect("valid app pool policy"))
            .shutdown(shutdown.child())
            .runtime(Handle::current())
            .ui_package(PathBuf::from("/shipped/ui"))
            .call()
            .expect("the document assembles")
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn the_app_section_lands_on_the_assembled_configuration() {
        let dir = tempdir();
        let shutdown = CancelToken::root();

        let named = assembled(
            &dir,
            "named",
            concat!(
                "app:\n  ui_package: /document/ui\n  sample_rate: 48000\n",
                "  palette:\n    accent: [10, 20, 30]\n",
            ),
            &shutdown,
        );
        assert_eq!(named.ui_package, Some(PathBuf::from("/document/ui")));
        assert_eq!(named.palette.accent, Rgb(10, 20, 30));
        assert_eq!(named.palette.bg, Palette::default().bg);
        assert_eq!(named.sample_rate, NonZeroU32::new(48_000));

        let silent = assembled(&dir, "silent", "", &shutdown);
        assert_eq!(silent.ui_package, Some(PathBuf::from("/shipped/ui")));
        assert_eq!(silent.palette.accent, Palette::default().accent);
        assert_eq!(silent.sample_rate, None);

        shutdown.cancel();
    }

    /// The proof the section is gone rather than merely unread: `network` no
    /// longer exists on `Document` at all (`size_probe_method` moved to `hls`,
    /// `compression` moved to `net` earlier), so naming `network` at the top
    /// level is refused by `deny_unknown_fields` through the whole
    /// merge-expand-type pipeline instead of silently parsing and being
    /// ignored.
    #[kithara::test(native, flash(false))]
    fn a_network_section_is_rejected() {
        let dir = tempdir();
        let path = write(
            &dir,
            "stale-section",
            "network:\n  size_probe_method: head\n",
        );

        let error =
            Config::load_with(Some(&path), None, &env).expect_err("network was renamed to hls");

        let report = error.to_string();
        assert!(matches!(error, LoadError::Schema { .. }), "{report}");
        assert!(report.contains("network"), "{report}");
    }

    #[kithara::test(native, flash(false))]
    fn an_empty_overlay_leaves_the_baked_document_in_force() {
        let dir = tempdir();
        let path = write(&dir, "empty", "");

        let config = Config::load_with(Some(&path), None, &env).expect("an empty overlay loads");

        assert_eq!(
            config.hls().size_probe_method,
            Some(SizeProbeMethod::RangeGet)
        );
        assert!(
            !config.tracks().is_empty(),
            "a file with nothing in it overrides nothing"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_overlay_of_comments_alone_leaves_the_baked_document_in_force() {
        let dir = tempdir();
        let path = write(&dir, "comments", "# fill this in later\n");

        let config = Config::load_with(Some(&path), None, &env).expect("an empty overlay loads");

        assert!(!config.tracks().is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn an_overlay_whose_root_is_a_sequence_names_that_file() {
        let dir = tempdir();
        let path = write(&dir, "sequence-root", "- a\n- b\n");

        let error =
            Config::load_with(Some(&path), None, &env).expect_err("a document root is a mapping");

        let report = error.to_string();
        assert!(matches!(error, LoadError::Schema { .. }), "{report}");
        assert!(report.contains("sequence-root.yaml"), "{report}");
        assert!(
            !report.contains(BAKED_PATH),
            "one file is at fault, not the merge: {report}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_path_named_explicitly_must_exist() {
        let dir = tempdir();
        let missing = dir.path().join("absent.yaml");

        let error = Config::load(Some(&missing), None).expect_err("the operator named this file");

        assert!(matches!(error, LoadError::Missing(_)));
    }

    #[kithara::test(native, flash(false))]
    fn a_file_beside_the_binary_may_be_absent() {
        let dir = tempdir();
        let absent = dir.path().join("not-there.yaml");

        Config::load_with(None, Some(&absent), &env).expect("an unnamed file is optional");
    }

    #[kithara::test(native, flash(false))]
    fn an_unresolved_reference_refuses_to_start() {
        let dir = tempdir();
        let path = write(
            &dir,
            "unresolved-reference",
            concat!(
                "drm:\n  providers:\n    - name: x\n      domains: [x.test]\n",
                "      cipher_key: $KITHARA_DEFINITELY_UNSET_IN_TESTS\n",
            ),
        );

        let error = Config::load(Some(&path), None).expect_err("the reference resolves nowhere");

        assert!(
            error
                .to_string()
                .contains("KITHARA_DEFINITELY_UNSET_IN_TESTS"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_malformed_file_names_its_path() {
        let dir = tempdir();
        let path = write(&dir, "malformed", "hls: [not, a, mapping]\n");

        let error = Config::load_with(Some(&path), None, &env).expect_err("the shape is wrong");

        let report = error.to_string();
        assert!(report.contains("malformed.yaml"), "{report}");
        assert!(report.contains("<baked app.yaml>"), "{report}");
    }

    #[kithara::test(native, flash(false))]
    fn a_reference_in_a_typed_field_reports_the_reference_not_its_value() {
        let dir = tempdir();
        let path = write(
            &dir,
            "typed-field",
            "player:\n  crossfade_duration: $KITHARA_DRM_PROD_KEY\n",
        );

        let error =
            Config::load_with(Some(&path), None, &env).expect_err("a string is not a float");

        let report = error.to_string();
        assert!(report.contains("$KITHARA_DRM_PROD_KEY"), "{report}");
        assert!(!report.contains("test-value"), "{report}");
    }

    #[kithara::test(native, flash(false))]
    fn the_dump_carries_references_not_secrets() {
        let config = Config::load_with(None, None, &env).expect("the baked document stands alone");

        let dump = config.dump();

        assert!(
            dump.contains("$KITHARA_DRM_PROD_KEY"),
            "the dump prints the reference, not what it resolves to"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_debug_render_carries_references_not_secrets() {
        let config = Config::load_with(None, None, &env).expect("the baked document stands alone");

        let rendered = format!("{config:?}");

        assert!(rendered.contains("$KITHARA_DRM_PROD_KEY"), "{rendered}");
        assert!(!rendered.contains("test-value"), "{rendered}");
    }
}
