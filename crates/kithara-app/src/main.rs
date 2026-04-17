// Binary needs at least one frontend — `lib-only` alone is not enough.
#[cfg(not(any(feature = "tui", feature = "gui")))]
compile_error!("`kithara` binary requires at least one of `tui` or `gui` feature");

use std::{
    io::{self, IsTerminal},
    sync::Arc,
};

use clap::Parser;
use kithara::{
    audio::generate_log_spaced_bands,
    net::NetOptions,
    play::{PlayerConfig, PlayerImpl},
    stream::dl::{Downloader, DownloaderConfig},
};
#[cfg(not(feature = "tui"))]
use kithara_app::gui;
#[cfg(feature = "gui")]
use kithara_app::gui::GuiFrontend;
#[cfg(feature = "tui")]
use kithara_app::tui::{TuiFrontend, init_tracing as init_tui_tracing};
use kithara_app::{config::AppConfig, frontend::Frontend};
use kithara_queue::{Queue, QueueConfig};

/// Kithara — audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player with TUI and GUI modes")]
struct Args {
    /// UI mode: auto, tui, or gui (default: auto-detect from terminal).
    #[arg(long, short, default_value = "auto")]
    mode: Mode,

    /// Accept invalid TLS certificates (self-signed, expired). For test servers only.
    /// Enabled by default during testing phase.
    #[arg(long, default_value_t = true)]
    insecure: bool,

    /// Audio files or URLs to play.
    tracks: Vec<String>,
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

fn main() -> AppResult {
    // Suppress noisy macOS system logs (OpenGL dlsym, WindowTab, etc.)
    #[cfg(target_os = "macos")]
    // SAFETY: called at program start before any threads are spawned.
    unsafe {
        std::env::set_var("OS_ACTIVITY_MODE", "disable");
    }

    let args = Args::parse();
    let mode = resolve_mode(args.mode);

    // One Downloader for the whole app — shared HTTP pool and
    // ambient-runtime aware, so every track created through
    // `sources::build_source` reuses it. TLS posture goes through
    // `NetOptions` on the Downloader; `ResourceConfig` has no `net`.
    let net = NetOptions {
        insecure: args.insecure,
        ..NetOptions::default()
    };
    let downloader = Downloader::new(DownloaderConfig::default().with_net(net));
    let config = AppConfig::new(downloader)
        .with_tracks(args.tracks)
        .with_danger_accept_invalid_certs(args.insecure);

    // Initialize tracing based on mode.
    #[cfg(feature = "tui")]
    {
        let log_directives: &[&str] = match mode {
            Mode::Tui => &["off"],
            _ => &["info"],
        };
        init_tui_tracing(log_directives, mode == Mode::Tui)?;
    }
    #[cfg(not(feature = "tui"))]
    {
        // GUI-only: simple tracing init without CRLF writer.
        gui::init_tracing()?;
    }

    let player_config = PlayerConfig::default()
        .with_crossfade_duration(config.crossfade_seconds)
        .with_eq_layout(generate_log_spaced_bands(config.eq_band_count));
    let player = Arc::new(PlayerImpl::new(player_config));
    let queue_config = QueueConfig::default()
        .with_player(player)
        .with_autoplay(true);

    // Queue is constructed here, but `queue.set_tracks` is deferred to each
    // frontend's runtime context — `Loader::spawn_load` requires a running
    // tokio runtime.
    let queue = Arc::new(Queue::new(queue_config));

    match mode {
        #[cfg(feature = "tui")]
        Mode::Tui | Mode::Auto => {
            let mut frontend = TuiFrontend::new(&config)?;
            frontend.start(Arc::clone(&queue))?;
            frontend.run_loop(Arc::clone(&queue))?;
            frontend.shutdown()?;
        }
        #[cfg(feature = "gui")]
        Mode::Gui => {
            let mut frontend = GuiFrontend::new(&config)?;
            frontend.start(Arc::clone(&queue))?;
            frontend.run_loop(Arc::clone(&queue))?;
            frontend.shutdown()?;
        }
        #[cfg(not(feature = "gui"))]
        Mode::Gui => {
            return Err("GUI mode not available: compile with --features gui".into());
        }
        #[cfg_attr(
            feature = "tui",
            expect(unreachable_patterns, reason = "depends on enabled features")
        )]
        _ => {
            return Err("No frontend available for the selected mode".into());
        }
    }

    Ok(())
}
