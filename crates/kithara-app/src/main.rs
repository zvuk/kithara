use std::{
    io::{self, IsTerminal},
    sync::Arc,
};

use clap::Parser;
use kithara::{audio::generate_log_spaced_bands, play::PlayerConfig, prelude::PlayerImpl};
#[cfg(not(feature = "tui"))]
use kithara_app::gui;
#[cfg(feature = "gui")]
use kithara_app::gui::GuiFrontend;
#[cfg(feature = "tui")]
use kithara_app::tui::{TuiFrontend, init_tracing as init_tui_tracing};
use kithara_app::{
    config::AppConfig, controls::AppController, frontend::Frontend, playlist::Playlist,
};

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

    let mut config = AppConfig::with_defaults(args.tracks);
    config.danger_accept_invalid_certs = args.insecure;

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

    // Create player and controller.
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::default()
            .with_crossfade_duration(config.crossfade_seconds)
            .with_eq_layout(generate_log_spaced_bands(config.eq_band_count)),
    ));
    let playlist = Arc::new(Playlist::new(config.tracks.clone(), &config.drm_domains));
    let mut controller = AppController::new(Arc::clone(&player), playlist, config.eq_band_count);

    match mode {
        #[cfg(feature = "tui")]
        Mode::Tui | Mode::Auto => {
            let mut frontend = TuiFrontend::new(&config)?;
            frontend.start(&mut controller)?;
            frontend.run_loop(&mut controller)?;
            frontend.shutdown()?;
        }
        #[cfg(feature = "gui")]
        Mode::Gui => {
            let mut frontend = GuiFrontend::new(&config)?;
            frontend.start(&mut controller)?;
            frontend.run_loop(&mut controller)?;
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
