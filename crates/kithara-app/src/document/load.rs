use std::{
    fmt, fs, io,
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
    assets::{AssetLayoutRegistry, AssetStoreConfigPatch, StorageBackend},
    audio::AudioConfigPatch,
    file::FileConfigPatch,
    hls::HlsConfigPatch,
    net::NetOptionsPatch,
    play::{PlayWorkerConfigPatch, PlayerConfigPatch, policy::DomainKeyPolicy},
    queue::QueueConfigPatch,
    worker::{ComputePool, DispatcherConfigPatch, WorkerConfigPatch},
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
    baked::{BAKED_DOCUMENT, baked_env},
    config::AppConfigPatch,
    pools::PoolsSection,
};

/// Path the baked document is reported under in parse errors.
const BAKED_PATH: &str = "<baked app.yaml>";

/// The configuration this process runs on, and the document it came from.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
    document: Document,
    /// The merged document before expansion. Kept so a dump can print
    /// references rather than the secrets behind them.
    source: Value,
}

impl fmt::Debug for Config {
    /// Renders the pre-expansion document: the typed one holds resolved
    /// values, and a `Debug` that prints them defeats [`Config::dump`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Why a document could not be turned into a configuration.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    /// A path the operator named does not exist.
    Missing(PathBuf),
    /// A document could not be read from disk.
    Read { path: PathBuf, source: io::Error },
    /// A document's text is not YAML.
    Parse {
        path: PathBuf,
        source: serde_yaml_ng::Error,
    },
    /// A document does not match the schema -- either the merged tree, or an
    /// overlay whose root is not a mapping, refused before any merge.
    Schema { resource: String, detail: String },
    /// A reference the document names resolved nowhere.
    Env(MissingEnv),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(f, "configuration file not found: {}", path.display()),
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Parse { path, source } => write!(f, "cannot parse {}: {source}", path.display()),
            Self::Schema { resource, detail } => write!(f, "cannot parse {resource}: {detail}"),
            Self::Env(missing) => write!(f, "{missing}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Env(missing) => Some(missing),
            Self::Schema { .. } | Self::Missing(_) => None,
        }
    }
}

impl Config {
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
        Self::load_with(explicit, beside, &|name| {
            std::env::var(name).ok().or_else(|| baked_env(name))
        })
    }

    fn load_with(
        explicit: Option<&Path>,
        beside: Option<&Path>,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, LoadError> {
        let mut source: Value =
            serde_yaml_ng::from_str(BAKED_DOCUMENT).map_err(|source| LoadError::Parse {
                path: PathBuf::from(BAKED_PATH),
                source,
            })?;

        let overlay_path = Self::overlay_path(explicit, beside)?;
        if let Some(path) = overlay_path.as_deref() {
            match Self::read(path)? {
                // A named key's explicit `null` blanks that key; the root has no
                // key, so an empty file is a file left to fill in later, not an
                // override that wipes the document.
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

    fn read(path: &Path) -> Result<Value, LoadError> {
        let text = fs::read_to_string(path).map_err(|source| LoadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_yaml_ng::from_str(&text).map_err(|source| LoadError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Tracks the document opens with.
    #[must_use]
    pub fn tracks(&self) -> &[String] {
        &self.document.playlist.tracks
    }

    /// The HTTP options the document names.
    #[must_use]
    pub fn net(&self) -> NetOptionsPatch {
        self.document.net.clone()
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

    /// Knobs the document sets on every HLS track's stream.
    #[must_use]
    pub fn hls(&self) -> HlsConfigPatch {
        self.document.hls.clone()
    }

    /// Knobs the document sets on every file track's stream.
    #[must_use]
    pub fn file(&self) -> FileConfigPatch {
        self.document.file.clone()
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

    /// Knobs the document sets on the player, threaded into every deck's
    /// `PlayerConfig`.
    #[must_use]
    pub fn player(&self) -> PlayerConfigPatch {
        self.document.player.clone()
    }

    /// Knobs the document sets on the built [`AppConfig`]. Fields the document
    /// never names stay `None`, leaving what the builder produced in place.
    ///
    /// [`AppConfig`]: crate::config::AppConfig
    #[must_use]
    pub fn app(&self) -> AppConfigPatch {
        self.document.app.clone()
    }

    /// Knobs the document sets on the compute worker.
    #[must_use]
    pub fn worker(&self) -> WorkerConfigPatch {
        self.document.worker.clone()
    }

    /// Knobs the document sets on the one playback worker every deck
    /// shares. Applied at the construction site in `main`, where the pools it
    /// is built from exist.
    #[must_use]
    pub fn play_worker(&self) -> PlayWorkerConfigPatch {
        self.document.play_worker.clone()
    }

    /// Knobs the document sets on the background dispatchers the app builds.
    /// Applied at each construction site, which keeps its own thread name.
    #[must_use]
    pub fn dispatcher(&self) -> DispatcherConfigPatch {
        self.document.dispatcher.clone()
    }

    /// Knobs the document sets on the application's buffer pools.
    #[must_use]
    pub fn pools(&self) -> PoolsSection {
        self.document.pools.clone()
    }

    /// Knobs the document sets on the asset store. A document that names no
    /// backend resolves to [`StorageBackend::default`] — a stable root under
    /// the system temp directory — and deliberately not to
    /// `AssetStore::open`'s own fallback, which is a fresh unique directory
    /// per launch and would move the on-disk cache every run.
    #[must_use]
    pub fn assets_store(&self) -> AssetStoreConfigPatch {
        let mut store = self.document.assets_store.clone();
        store.backend.get_or_insert_with(StorageBackend::default);
        store
    }

    /// Knobs the document sets on the queue.
    #[must_use]
    pub fn queue(&self) -> QueueConfigPatch {
        self.document.queue.clone()
    }

    /// Knobs the document sets on this session's live broadcast. Applied at
    /// the construction site in `main`, where the worker and pools a
    /// `BroadcastConfig` is built from exist.
    #[cfg(feature = "broadcast")]
    #[must_use]
    pub fn broadcast(&self) -> BroadcastConfigPatch {
        self.document.broadcast.clone()
    }

    /// The compute pool the document names, when it names one. `None` leaves
    /// the pool the caller already installed standing.
    #[must_use]
    pub fn worker_pool(&self) -> Option<ComputePool> {
        self.document.worker_pool.clone()
    }

    /// The media-identity registry the asset store reads.
    #[must_use]
    pub fn asset_layouts(&self) -> AssetLayoutRegistry {
        asset_layouts(&self.document.assets)
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
    use std::{fs, num::NonZeroUsize, path::PathBuf};

    use kithara::{
        hls::SizeProbeMethod,
        host::HostConfig,
        net::{Compression, NetOptions},
        worker::ComputePool,
    };
    use tempfile::TempDir;

    use super::{BAKED_PATH, Config, LoadError, StorageBackend};
    use crate::pools::AppPools;

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
    /// `worker.max_compute_tasks` and `worker_pool` reach the application as
    /// separate values. `WorkerConfig`'s own fields are `pub(crate)`, so the
    /// application can only see what the accessors hand it — that the patch
    /// then writes the ceiling is pinned inside `kithara-worker` by
    /// `a_patch_writes_only_the_field_it_names`.
    #[kithara::test(native, flash(false))]
    fn the_worker_keys_survive_the_load_pipeline() {
        let dir = tempdir();
        let path = write(
            &dir,
            "worker-and-pool",
            "worker:\n  max_compute_tasks: 4\nworker_pool:\n  mode: disabled\n",
        );

        let config = Config::load_with(Some(&path), None, &env).expect("the overlay loads");

        assert_eq!(
            config.worker().max_compute_tasks.map(NonZeroUsize::get),
            Some(4)
        );
        assert!(
            matches!(config.worker_pool(), Some(ComputePool::Disabled {})),
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

        assert_eq!(settings.cache_capacity.map(NonZeroUsize::get), Some(32));
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
            Some(StorageBackend::default()),
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

        assert_eq!(config.assets_store().backend, Some(StorageBackend::Memory));
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
            .maybe_sample_rate_hint(document.app().sample_rate)
            .build();

        assert_eq!(host.sample_rate().get(), 48_000);
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
