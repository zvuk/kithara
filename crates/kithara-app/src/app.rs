use std::{error::Error, sync::Arc};

use kithara::{
    play::{Engine, PlayerConfig},
    prelude::{PlayerImpl, Resource, ResourceConfig},
};
use tracing::warn;

use crate::{events, tui_runner};

pub(crate) const CROSSFADE_SECONDS: f32 = 5.0;
pub(crate) const FILE_URL_DEFAULT: &str = "https://stream.silvercomet.top/track.mp3";
pub(crate) const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";

pub type AppError = Box<dyn Error + Send + Sync>;
pub type AppResult<T = ()> = Result<T, AppError>;

/// Application UI mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Auto-detect: TUI if terminal attached, GUI otherwise.
    Auto,
    /// Terminal UI (ratatui).
    Tui,
    /// Graphical UI (Slint).
    Gui,
}

/// Extract a human-readable track name from a URL.
#[must_use]
pub fn track_name(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

/// Resolve `Mode::Auto` into a concrete mode.
#[must_use]
pub fn resolve_mode(mode: Mode) -> Mode {
    match mode {
        Mode::Auto => {
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                Mode::Tui
            } else {
                Mode::Gui
            }
        }
        concrete => concrete,
    }
}

/// Run the application in the selected mode.
///
/// # Errors
/// Returns an error if resource loading or the UI event loop fails.
pub async fn run(mode: Mode, urls: Vec<String>) -> AppResult {
    let mode = resolve_mode(mode);

    let urls = if urls.is_empty() {
        vec![FILE_URL_DEFAULT.to_string(), HLS_URL_DEFAULT.to_string()]
    } else {
        urls
    };

    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS),
    ));

    match mode {
        Mode::Tui => run_tui_mode(&player, urls).await,
        Mode::Gui => run_gui_mode(&player, urls),
        Mode::Auto => unreachable!("resolved above"),
    }
}

/// GUI mode: kithara-ui owns track loading via its playlist + callbacks.
fn run_gui_mode(player: &Arc<PlayerImpl>, urls: Vec<String>) -> AppResult {
    events::start_event_logging(player);
    kithara_ui::run(player, urls, true)?;
    Ok(())
}

/// TUI mode: load resources upfront, then run the terminal UI event loop.
async fn run_tui_mode(player: &Arc<PlayerImpl>, urls: Vec<String>) -> AppResult {
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<events::UiMsg>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let mut forwarders = vec![
        events::forward_player_events(player.subscribe(), ui_tx.clone()),
        events::forward_engine_events(player.engine().subscribe(), ui_tx.clone()),
    ];

    let mut track_names: Vec<String> = Vec::new();
    for (i, url) in urls.iter().enumerate() {
        let name = track_name(url);
        let config = match ResourceConfig::new(url) {
            Ok(c) => c,
            Err(err) => {
                warn!(?err, url, "invalid URL, skipping");
                continue;
            }
        };
        match Resource::new(config).await {
            Ok(resource) => {
                let source_events = resource.subscribe();
                player.insert(resource, None);
                let label = format!("src{}", i + 1);
                forwarders.push(events::forward_source_events(
                    source_events,
                    label,
                    ui_tx.clone(),
                ));
                track_names.push(name);
                let _ = ui_tx.send(events::UiMsg::Note(format!("loaded #{}: {url}", i + 1)));
            }
            Err(err) => {
                warn!(?err, url, "failed to load resource, skipping");
                let _ = ui_tx.send(events::UiMsg::Note(format!("skip #{}: {err}", i + 1)));
            }
        }
    }

    if track_names.is_empty() {
        return Err("no tracks loaded".into());
    }

    player.play();

    tui_runner::run_tui(player, urls, track_names, ui_tx, ui_rx, stop_tx, stop_rx).await?;

    player.pause();

    for forwarder in forwarders {
        forwarder.abort();
        let _ = forwarder.await;
    }

    Ok(())
}
