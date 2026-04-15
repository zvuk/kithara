use std::io::{self, IsTerminal};

use clap::Parser;
use kithara::{audio::generate_log_spaced_bands, play::PlayerConfig, prelude::ResourceConfig};
#[cfg(not(feature = "tui"))]
use kithara_app::gui;
#[cfg(feature = "gui")]
use kithara_app::gui::GuiFrontend;
#[cfg(feature = "tui")]
use kithara_app::tui::{TuiFrontend, init_tracing as init_tui_tracing};
use kithara_app::{config::AppConfig, frontend::Frontend};
use kithara_queue::{Queue, QueueConfig, TrackSource};
use url::Url;

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

/// Whether the host portion of `url` matches one of the configured DRM
/// domains.
fn needs_drm(url: &str, drm_domains: &[String]) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| drm_domains.iter().any(|d| h.ends_with(d.as_str())))
        })
        .unwrap_or(false)
}

/// Build a [`TrackSource`] for a given URL, applying zvuk DRM keys when
/// the URL's host matches `config.drm_domains`.
fn build_source(url: &str, config: &AppConfig) -> TrackSource {
    if needs_drm(url, &config.drm_domains) {
        match ResourceConfig::new(url) {
            Ok(mut cfg) => {
                cfg = cfg.with_keys(kithara_app::drm::make_key_options());
                cfg.net.insecure = config.danger_accept_invalid_certs;
                TrackSource::Config(Box::new(cfg))
            }
            Err(e) => {
                tracing::error!(%url, %e, "failed to build DRM config, falling back to Uri");
                TrackSource::Uri(url.to_string())
            }
        }
    } else {
        TrackSource::Uri(url.to_string())
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

    let player_config = PlayerConfig::default()
        .with_crossfade_duration(config.crossfade_seconds)
        .with_eq_layout(generate_log_spaced_bands(config.eq_band_count));
    let mut queue_config = QueueConfig::new(player_config);
    queue_config.net.insecure = config.danger_accept_invalid_certs;
    queue_config.autoplay = true;

    let queue = std::sync::Arc::new(Queue::new(queue_config));
    let sources: Vec<TrackSource> = config
        .tracks
        .iter()
        .map(|url| build_source(url, &config))
        .collect();
    queue.set_tracks(sources);

    match mode {
        #[cfg(feature = "tui")]
        Mode::Tui | Mode::Auto => {
            let mut frontend = TuiFrontend::new(&config)?;
            frontend.start(std::sync::Arc::clone(&queue))?;
            frontend.run_loop(std::sync::Arc::clone(&queue))?;
            frontend.shutdown()?;
        }
        #[cfg(feature = "gui")]
        Mode::Gui => {
            let mut frontend = GuiFrontend::new(&config)?;
            frontend.start(std::sync::Arc::clone(&queue))?;
            frontend.run_loop(std::sync::Arc::clone(&queue))?;
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
