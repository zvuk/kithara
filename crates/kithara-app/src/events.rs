use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use kithara::{
    play::{Engine, EngineEvent, PlayerEvent},
    prelude::{AudioEvent, Event, FileEvent, HlsEvent, PlayerImpl},
};
use tokio::sync::{broadcast, broadcast::error::RecvError};
use tracing::info;

pub enum UiMsg {
    Engine(EngineEvent),
    Note(String),
    Player(PlayerEvent),
    Source { event: Event, source: String },
}

#[must_use]
pub fn is_progress_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Audio(AudioEvent::PlaybackProgress { .. })
            | Event::File(FileEvent::DownloadProgress { .. })
            | Event::File(FileEvent::ByteProgress { .. })
            | Event::File(FileEvent::PlaybackProgress { .. })
            | Event::Hls(HlsEvent::DownloadProgress { .. })
            | Event::Hls(HlsEvent::ByteProgress { .. })
            | Event::Hls(HlsEvent::PlaybackProgress { .. })
    )
}

#[must_use]
pub fn source_note(source: &str, event: &Event) -> Option<String> {
    match event {
        Event::Audio(AudioEvent::FormatDetected { spec }) => Some(format!(
            "{source} fmt {}ch {}Hz",
            spec.channels, spec.sample_rate
        )),
        Event::Audio(AudioEvent::SeekComplete { position, .. }) => Some(format!(
            "{source} seek {}",
            format_seconds(position.as_secs_f64())
        )),
        Event::Hls(HlsEvent::VariantApplied {
            to_variant, reason, ..
        }) => Some(format!("{source} abr v{to_variant} {reason:?}")),
        Event::File(FileEvent::DownloadComplete { total_bytes }) => {
            Some(format!("{source} dl done {total_bytes} bytes"))
        }
        _ => None,
    }
}

#[must_use]
pub fn format_seconds(seconds: f64) -> String {
    let whole = if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds).as_secs()
    } else {
        0
    };
    let minutes = whole / 60;
    let secs = whole % 60;
    format!("{minutes:02}:{secs:02}")
}

#[must_use]
pub fn forward_player_events(
    mut rx: broadcast::Receiver<PlayerEvent>,
    tx: mpsc::Sender<UiMsg>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if tx.send(UiMsg::Player(event)).is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    if tx
                        .send(UiMsg::Note(format!("player events lagged n={n}")))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

#[must_use]
pub fn forward_engine_events(
    mut rx: broadcast::Receiver<EngineEvent>,
    tx: mpsc::Sender<UiMsg>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if tx.send(UiMsg::Engine(event)).is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    if tx
                        .send(UiMsg::Note(format!("engine events lagged n={n}")))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

#[must_use]
pub fn forward_source_events(
    mut rx: broadcast::Receiver<Event>,
    source: String,
    tx: mpsc::Sender<UiMsg>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if tx
                        .send(UiMsg::Source {
                            event,
                            source: source.clone(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    if tx
                        .send(UiMsg::Note(format!("{source} events lagged n={n}")))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// Spawn a background task that logs all player and engine events via tracing.
///
/// Used in GUI mode where there is no TUI event loop to display events.
pub fn start_event_logging(player: &Arc<PlayerImpl>) {
    let mut player_rx = player.subscribe();
    let mut engine_rx = player.engine().subscribe();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = player_rx.recv() => match result {
                    Ok(event) => info!("[player] {event:?}"),
                    Err(RecvError::Lagged(n)) => info!("[player] events lagged: {n}"),
                    Err(RecvError::Closed) => break,
                },
                result = engine_rx.recv() => match result {
                    Ok(event) => info!("[engine] {event:?}"),
                    Err(RecvError::Lagged(n)) => info!("[engine] events lagged: {n}"),
                    Err(RecvError::Closed) => break,
                },
            }
        }
    });
}
