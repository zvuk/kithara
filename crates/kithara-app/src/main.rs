#[cfg(not(feature = "gui"))]
compile_error!("`kithara` binary requires the `gui` feature");

use std::sync::OnceLock;

use clap::Parser;
use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    bufpool::Region,
    net::{HttpClient, NetOptions},
    play::SessionHandle,
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    baked,
    config::AppConfig,
    deck::{Deck, DeckId, DeckSet},
    gui::{GuiFrontend, Host},
    tracing_init::init_tracing,
};
use kithara_platform::CancelToken;

/// Kithara — audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player")]
struct Args {
    /// Audio files or URLs to play.
    tracks: Vec<String>,

    /// Accept invalid TLS certificates (self-signed, expired). For test servers only.
    /// Enabled by default during testing phase.
    #[arg(long, default_value_t = true)]
    insecure: bool,

    /// Which host draws the studio. A build without the `masonry` feature has
    /// only the immediate one.
    #[arg(long, value_enum, default_value_t)]
    host: Host,
}

type AppError = Box<dyn std::error::Error + Send + Sync>;
type AppResult<T = ()> = Result<T, AppError>;

static APP_SESSION: OnceLock<SessionHandle> = OnceLock::new();

fn app_session_handle() -> SessionHandle {
    APP_SESSION.get_or_init(SessionHandle::spawn_native).clone()
}

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
    init_tracing(&["info"])?;

    // App master root held for the whole process: it goes into `AppConfig` and
    // every subsystem derives from `shutdown.child()`, so a frontend
    // `config.shutdown.cancel()` propagates down the shutdown subtree to all of
    let shutdown = CancelToken::root();
    let region = Region::default();
    let byte_pool = region.byte_pool();
    let net = NetOptions::builder()
        .is_insecure(args.insecure || baked::BAKED_SHOULD_ACCEPT_INVALID_CERTS)
        .compression(baked::BAKED_COMPRESSION)
        .byte_pool(byte_pool.clone())
        .build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, shutdown.child())).build(),
    );
    let flush_hub = FlushHub::new(shutdown.child(), FlushPolicy::default());
    let store = AssetStore::builder()
        .cancel(shutdown.child())
        .backend(StorageBackend::default())
        .pool(byte_pool.clone())
        .flush_hub(flush_hub)
        .layouts(baked::build_baked_asset_layouts())
        .build();
    let config = AppConfig::builder()
        .downloader(downloader)
        .shutdown(shutdown.clone())
        .byte_pool(byte_pool)
        .pcm_pool(region.pcm_pool())
        .store(store)
        .maybe_tracks((!args.tracks.is_empty()).then_some(args.tracks))
        .should_accept_invalid_certs(args.insecure)
        .build();

    let session = app_session_handle();
    let mut deck_set = DeckSet::new(vec![
        Deck::build(DeckId(0), &config, &session),
        Deck::build(DeckId(1), &config, &session),
    ]);
    deck_set.commit(deck_set.mix().clone())?;
    let mut frontend = GuiFrontend::new(&config, args.host)?;
    frontend.attach_broadcast(session, shutdown.clone());
    frontend.start(&deck_set)?;
    frontend.run_loop(deck_set)?;
    frontend.shutdown()?;

    Ok(())
}
