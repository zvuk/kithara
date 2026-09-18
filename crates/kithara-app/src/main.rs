#[cfg(not(feature = "gui"))]
compile_error!("`kithara` binary requires the `gui` feature");

use std::num::NonZeroUsize;

use clap::Parser;
use kithara::{
    assets::{FlushHub, FlushPolicy},
    download::{Downloader, DownloaderConfig},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, thread, tokio},
    play::PlayWorkerConfig,
    worker::{OwnedPoolConfig, Worker, WorkerConfig},
};
use kithara_app::{
    config::{AppBroadcastConfig, AppConfig, AppDrm},
    deck::{Deck, DeckId, DeckSet},
    document::Config,
    gui::{self, GuiFrontend},
    memory,
    pools::{self, AppHost, AppStore, AppWorker},
    tracing_init::init_tracing,
};

/// Kithara — audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player")]
struct Args {
    /// Which host draws the studio. A build without the `masonry` feature has
    /// only the immediate one.
    #[arg(long, value_enum, default_value_t)]
    host: gui::Host,

    /// Configuration document to read. Defaults to `kithara.yaml` beside the
    /// executable when one is there.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Folder holding the UI package to draw from. An override on top of the
    /// document's `app.ui_package`: a path here wins regardless of what the
    /// document says. With neither, the package a release lays out beside the
    /// executable draws.
    #[arg(long)]
    ui_package: Option<std::path::PathBuf>,

    /// Audio files or URLs to play.
    tracks: Vec<String>,

    /// Print the effective configuration and exit.
    #[arg(long)]
    dump_config: bool,

    /// Accept invalid TLS certificates (self-signed, expired). For test servers only.
    /// An override on top of the document's `net.is_insecure`: `true` here
    /// forces it on regardless of the document.
    #[arg(long)]
    insecure: bool,
}

/// Count what this build allocates and abort on the first allocation that
/// carries the process past [`memory::DEFAULT_LIMIT_BYTES`], so an unbounded
/// allocator leaves the stack that crossed the ceiling. A release build keeps
/// the platform allocator and pays nothing.
#[cfg(debug_assertions)]
#[global_allocator]
static HEAP: memory::Ceiling = memory::Ceiling;

/// Where a release lays its UI documents out: beside the executable.
fn shipped_ui_package() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("assets/ui"))
}

/// Where an installation leaves its configuration: beside the executable.
fn config_beside_binary() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("kithara.yaml"))
}

type AppError = Box<dyn std::error::Error + Send + Sync>;
type AppResult<T = ()> = Result<T, AppError>;

/// Suppress noisy macOS system logs (`OpenGL` `dlsym`, `WindowTab`, etc.)
/// at program start before any threads are spawned. No-op on other targets.
#[cfg(target_os = "macos")]
fn suppress_macos_system_logs() {
    // SAFETY: called at program start before any threads are spawned.
    unsafe {
        std::env::set_var("OS_ACTIVITY_MODE", "disable");
    }
}

#[cfg(not(target_os = "macos"))]
fn suppress_macos_system_logs() {}

fn main() -> AppResult {
    suppress_macos_system_logs();

    let args = Args::parse();
    // Reported through `Display` and not returned: `main`'s error is printed
    // with `Debug`, which drops the readable list of unset `$KITHARA_...`
    // names. Tracing is not up yet either, so this goes to stderr directly.
    let document = match Config::load(args.config.as_deref(), config_beside_binary().as_deref()) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    if args.dump_config {
        println!("{}", document.dump());
        return Ok(());
    }

    let app = document.app();
    let directives = app
        .log_directives
        .clone()
        .unwrap_or_else(|| vec!["info".to_string()]);
    init_tracing(&directives.iter().map(String::as_str).collect::<Vec<&str>>())?;
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();

    let shutdown = CancelToken::root();
    let pools = pools::build(&document.pools())?;
    let compute_threads = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
    let mut worker_config = WorkerConfig::new()
        .with_cancel(shutdown.child())
        .with_runtime(runtime.handle().clone())
        .with_max_compute_tasks(compute_threads)
        .with_owned_pool(OwnedPoolConfig::new(compute_threads, "kithara-compute"));
    worker_config.apply(document.worker());
    let base_worker = Worker::new(worker_config);
    let mut play_worker_config = PlayWorkerConfig::builder(pools.clone())
        .cancel(shutdown.child())
        .worker(base_worker.clone())
        .build();
    play_worker_config.apply(document.play_worker());
    let worker = AppWorker::new(play_worker_config);
    #[cfg(feature = "broadcast")]
    let broadcast = {
        let mut broadcast = AppBroadcastConfig::builder(base_worker.clone(), pools.clone())
            .cancel(shutdown.child())
            .build();
        broadcast.apply(document.broadcast());
        broadcast
    };
    #[cfg(not(feature = "broadcast"))]
    let broadcast = AppBroadcastConfig::default();
    let mut net = NetOptions::builder().build();
    net.apply(document.net());
    if args.insecure {
        net.is_insecure = true;
    }
    let should_accept_invalid_certs = net.is_insecure;
    let mut downloader_config =
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), shutdown.child())).build();
    downloader_config.apply(document.downloader());
    let downloader = Downloader::new(downloader_config);
    let mut flush_policy = FlushPolicy::default();
    flush_policy.apply(document.flush());
    let flush_hub = FlushHub::new(shutdown.child(), flush_policy);
    let mut store_config = AppStore::builder(pools)
        .cancel(shutdown.child())
        .flush_hub(flush_hub)
        .layouts(document.asset_layouts())
        .into_config();
    store_config.apply(document.assets_store());
    let store = AppStore::open(store_config);
    // `eprintln!`, not `tracing::error!`: tracing is up by now, but
    // `init_tracing` points the subscriber at `KITHARA_LOG_FILE`, so a startup
    // refusal logged through it lands in `app.log` and the terminal that ran
    // the binary shows nothing before the exit code.
    let drm_policy = match document.drm_policy() {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let beat_analysis = match document.beat() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("beat: {error}");
            std::process::exit(1);
        }
    };
    let mut config = AppConfig::builder()
        .drm(AppDrm::new(drm_policy))
        .beat_analysis(beat_analysis)
        .downloader(downloader)
        .shutdown(shutdown.clone())
        .worker(worker)
        .base_worker(base_worker.clone())
        .broadcast(broadcast)
        .store(store)
        .queue(document.queue())
        .dispatcher(document.dispatcher())
        .player(document.player())
        .audio(document.audio())
        .hls(document.hls())
        .file(document.file())
        .ui(document.ui()?)
        // The same value tracing is running on, so a document that names no
        // directives still leaves the built configuration agreeing with the
        // process; one that names them has `apply` put back exactly this.
        .log_directives(directives)
        .tracks(if args.tracks.is_empty() {
            document.tracks().to_vec()
        } else {
            args.tracks
        })
        .should_accept_invalid_certs(should_accept_invalid_certs)
        .maybe_ui_package(shipped_ui_package())
        .build();
    config.apply(app);
    memory::set_limit(config.memory_limit_bytes);
    if let Some(package) = args.ui_package {
        config.ui_package = Some(package);
    }

    let mut host = AppHost::new(
        HostConfig::builder()
            .maybe_sample_rate_hint(config.sample_rate)
            .maybe_output_block_frames(config.output_block_frames)
            .build(),
    )?;
    let decks = vec![
        Deck::build(DeckId(0), &config, &mut host)?,
        Deck::build(DeckId(1), &config, &mut host)?,
    ];
    let mut deck_set = DeckSet::new(host, decks);
    deck_set.commit(deck_set.mix().clone())?;
    let mut frontend = GuiFrontend::new(&config, args.host)?;
    frontend.attach_broadcast();
    frontend.start(&deck_set)?;
    frontend.run_loop(deck_set)?;
    frontend.shutdown()?;

    Ok(())
}
