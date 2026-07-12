#[cfg(not(any(feature = "tui", feature = "gui")))]
compile_error!("`kithara` binary requires at least one of `tui` or `gui` feature");

use std::io::{self, IsTerminal};

use clap::Parser;
use kithara::{
    assets::{AssetStoreBuilder, EvictConfig, FlushHub, FlushPolicy, StoreOptions},
    audio::generate_log_spaced_bands,
    bufpool::Region,
    net::{HttpClient, NetOptions},
    play::{PlayerConfig, PlayerImpl, StretchControls},
    stream::dl::{Downloader, DownloaderConfig},
};
#[cfg(not(feature = "tui"))]
use kithara_app::gui;
#[cfg(feature = "gui")]
use kithara_app::gui::GuiFrontend;
#[cfg(feature = "tui")]
use kithara_app::tui::TuiFrontend;
use kithara_app::{baked, config::AppConfig, frontend::Frontend, tracing_init::init_tracing};
use kithara_platform::{CancelToken, sync::Arc};
use kithara_queue::{Queue, QueueConfig};

/// Kithara — audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player with TUI and GUI modes")]
struct Args {
    /// UI mode: auto, tui, or gui (default: auto-detect from terminal).
    #[arg(long, short, default_value = "auto")]
    mode: Mode,

    /// Audio files or URLs to play.
    tracks: Vec<String>,

    /// Accept invalid TLS certificates (self-signed, expired). For test servers only.
    /// Enabled by default during testing phase.
    #[arg(long, default_value_t = true)]
    insecure: bool,
}

/// Application UI mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    /// Auto-detect: TUI if terminal attached, GUI otherwise.
    Auto,
    /// Terminal UI (ratatui).
    Tui,
    /// Graphical UI (iced).
    Gui,
}

type AppError = Box<dyn std::error::Error + Send + Sync>;
type AppResult<T = ()> = Result<T, AppError>;

/// Resolve `Mode::Auto` into a concrete mode.
fn resolve_mode(mode: Mode) -> Mode {
    match mode {
        Mode::Auto => {
            if io::stdin().is_terminal() {
                Mode::Tui
            } else {
                Mode::Gui
            }
        }
        concrete => concrete,
    }
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

fn init_tracing_for_mode(mode: Mode) -> AppResult<()> {
    let log_directives: &[&str] = match mode {
        Mode::Tui => &["off"],
        _ => &["info"],
    };
    init_tracing(log_directives, mode == Mode::Tui)?;
    Ok(())
}

fn main() -> AppResult {
    suppress_macos_system_logs();

    let args = Args::parse();
    let mode = resolve_mode(args.mode);

    init_tracing_for_mode(mode)?;

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
    let store_options = StoreOptions::builder()
        .flush_hub(Arc::clone(&flush_hub))
        .build();
    let asset_store = Arc::new(
        AssetStoreBuilder::default()
            .cancel(shutdown.child())
            .backend(store_options.backend.clone())
            .evict_config(EvictConfig::from(&store_options))
            .pool(byte_pool.clone())
            .maybe_cache_capacity(store_options.cache_capacity)
            .maybe_flush_hub(store_options.flush_hub.clone())
            .build(),
    );
    let config = AppConfig::builder()
        .downloader(downloader)
        .flush_hub(flush_hub)
        .shutdown(shutdown.clone())
        .byte_pool(byte_pool)
        .pcm_pool(region.pcm_pool())
        .asset_store(asset_store)
        .maybe_tracks((!args.tracks.is_empty()).then_some(args.tracks))
        .should_accept_invalid_certs(args.insecure)
        .build();

    let timestretch = StretchControls::new(1.0);
    let player_config = PlayerConfig::builder()
        .cancel(shutdown.child())
        .crossfade_duration(config.crossfade_seconds)
        .eq_layout(generate_log_spaced_bands(config.eq_band_count))
        .pcm_pool(region.pcm_pool())
        .timestretch(Arc::clone(&timestretch))
        .build();
    let player = Arc::new(PlayerImpl::new(player_config));
    let queue_config = QueueConfig::default()
        .with_player(player)
        .with_cancel(shutdown.child());

    let queue = Arc::new(Queue::new(queue_config));

    match mode {
        #[cfg(feature = "tui")]
        Mode::Tui | Mode::Auto => {
            let mut frontend = TuiFrontend::new(&config)?;
            frontend.start(Arc::clone(&queue))?;
            frontend.run_loop(Arc::clone(&queue), Arc::clone(&timestretch))?;
            frontend.shutdown()?;
        }
        #[cfg(feature = "gui")]
        Mode::Gui => {
            let mut frontend = GuiFrontend::new(&config)?;
            frontend.start(Arc::clone(&queue))?;
            frontend.run_loop(Arc::clone(&queue), Arc::clone(&timestretch))?;
            frontend.shutdown()?;
        }
        #[cfg(not(feature = "gui"))]
        Mode::Gui => {
            return Err("GUI mode not available: compile with --features gui".into());
        }
    }

    Ok(())
}
