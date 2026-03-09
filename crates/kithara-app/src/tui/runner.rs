use std::{
    sync::mpsc::{self, TryRecvError},
    time::Duration,
};

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use kithara::{
    play::{Engine, EngineEvent, PlayerEvent},
    prelude::{PlayerImpl, Resource, ResourceConfig},
};
use tokio::sync::broadcast::error::RecvError;

use super::{dashboard::Dashboard, session::UiSession};
use crate::{
    controls::{AppController, PlayerControls},
    crossfade::{CrossfadeClock, ProgressLog},
    events::{UiMsg, format_seconds, is_progress_event, source_note},
    theme::tui::TuiPalette,
};

const CONTROL_POLL_MS: u64 = 100;
const SEEK_STEP_SECONDS_F64: f64 = 5.0;
const VOLUME_STEP: f32 = 0.05;

type RunnerResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

enum ControlOutcome {
    Continue(Option<String>),
    SwitchTrack(usize),
    Quit,
}

/// Run the TUI event loop.
///
/// # Errors
/// Returns an error if the TUI event loop fails.
pub(super) async fn run_tui(
    controller: &mut AppController,
    urls: Vec<String>,
    track_names: Vec<String>,
    palette: TuiPalette,
) -> RunnerResult {
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let player = controller.player().clone();

    let mut forwarders = vec![
        forward_player_events(player.subscribe(), ui_tx.clone()),
        forward_engine_events(player.engine().subscribe(), ui_tx.clone()),
    ];

    // Load tracks.
    for (i, url) in urls.iter().enumerate() {
        let config = match ResourceConfig::new(url) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(?err, url, "invalid URL, skipping");
                continue;
            }
        };
        match Resource::new(config).await {
            Ok(resource) => {
                let source_events = resource.subscribe();
                player.insert(resource, None);
                let label = format!("src{}", i + 1);
                forwarders.push(forward_source_events(source_events, label, ui_tx.clone()));
                let _ = ui_tx.send(UiMsg::Note(format!("loaded #{}: {url}", i + 1)));
            }
            Err(err) => {
                tracing::warn!(?err, url, "failed to load resource, skipping");
                let _ = ui_tx.send(UiMsg::Note(format!("skip #{}: {err}", i + 1)));
            }
        }
    }

    if track_names.is_empty() {
        return Err("no tracks loaded".into());
    }

    controller.play();

    // Run blocking UI loop in a separate thread.
    let player_for_ui = player.clone();
    let ui_tx_for_loop = ui_tx.clone();
    let urls_for_loop = urls.clone();
    let mut ui_handle = tokio::task::spawn_blocking(move || {
        run_ui_loop(
            &player_for_ui,
            &urls_for_loop,
            track_names,
            &ui_tx_for_loop,
            &ui_rx,
            &stop_rx,
            palette,
        )
    });

    let ui_finished = tokio::select! {
        result = &mut ui_handle => {
            result??;
            true
        }
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() {
                let _ = ui_tx.send(UiMsg::Note("ctrl+c received".to_string()));
            }
            false
        }
    };

    let _ = stop_tx.send(());
    if !ui_finished {
        ui_handle.await??;
    }

    controller.pause();

    for forwarder in forwarders {
        forwarder.abort();
        let _ = forwarder.await;
    }

    Ok(())
}

fn run_ui_loop(
    player: &PlayerImpl,
    urls: &[String],
    track_names: Vec<String>,
    ui_tx: &mpsc::Sender<UiMsg>,
    ui_rx: &mpsc::Receiver<UiMsg>,
    stop_rx: &mpsc::Receiver<()>,
    palette: TuiPalette,
) -> RunnerResult {
    let track_count = track_names.len();
    let dashboard = Dashboard::new(track_names, palette);
    let mut ui = UiSession::new(dashboard)?;
    ui.log_line(&format!(
        "controls: 1-{} select track, Left/Right seek {SEEK_STEP_SECONDS_F64:.0}s, Up/Down vol {:+.0}%",
        track_count.min(9),
        VOLUME_STEP * 100.0
    ))?;
    ui.log_line("auto-advances to next track with crossfade near end of each track")?;
    ui.draw()?;

    let mut progress_log = ProgressLog::new();
    let mut crossfade_clock: Option<CrossfadeClock> = None;
    let mut auto_advanced_index: Option<usize> = None;
    let mut tick_error_logged = false;

    'ui: loop {
        match stop_rx.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }

        loop {
            match ui_rx.try_recv() {
                Ok(msg) => match msg {
                    UiMsg::Engine(event) => {
                        if let EngineEvent::MasterVolumeChanged { volume } = event {
                            ui.dashboard.set_volume(volume);
                        }
                        ui.log_line(&format!("engine {event:?}"))?;
                    }
                    UiMsg::Note(note) => {
                        ui.dashboard.set_note(note.clone());
                        ui.log_line(&note)?;
                    }
                    UiMsg::Player(event) => {
                        match event {
                            PlayerEvent::CurrentItemChanged => {
                                ui.dashboard.set_note("track changed");
                            }
                            PlayerEvent::RateChanged { rate } => {
                                ui.dashboard.set_playing(rate > 0.0);
                                ui.dashboard.set_note(format!("rate {rate:.2}"));
                            }
                            PlayerEvent::StatusChanged { status } => {
                                ui.dashboard.set_note(format!("status {status:?}"));
                            }
                            PlayerEvent::VolumeChanged { volume } => {
                                ui.dashboard.set_volume(volume);
                                ui.dashboard
                                    .set_note(format!("volume {:.0}%", volume * 100.0));
                            }
                            _ => {}
                        }
                        ui.log_line(&format!("player {event:?}"))?;
                    }
                    UiMsg::Source { event, source } => {
                        if let Some(note) = source_note(&source, &event) {
                            ui.dashboard.set_note(note);
                        }
                        if is_progress_event(&event) {
                            if progress_log.should_emit() {
                                ui.log_line(&format!("{source} {event:?}"))?;
                            }
                        } else {
                            ui.log_line(&format!("{source} {event:?}"))?;
                        }
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'ui,
            }
        }

        match player.tick() {
            Ok(()) => {
                tick_error_logged = false;
            }
            Err(err) => {
                if !tick_error_logged {
                    ui.log_line(&format!("tick failed: {err}"))?;
                    ui.dashboard.set_note(format!("tick failed: {err}"));
                    tick_error_logged = true;
                }
            }
        }

        refresh_dashboard(&mut ui.dashboard, player);

        // Crossfade progress
        if let Some(clock) = crossfade_clock.as_ref() {
            let progress = clock.progress();
            ui.dashboard.set_crossfade_progress(Some(progress));
            if progress >= 1.0 {
                crossfade_clock = None;
            }
        } else {
            ui.dashboard.set_crossfade_progress(None);
        }

        // Auto-advance: crossfade to next track when remaining time <= crossfade duration
        let crossfade_secs = f64::from(player.crossfade_duration());
        if let (Some(pos), Some(dur)) = (player.position_seconds(), player.duration_seconds())
            && dur > crossfade_secs
            && pos >= dur - crossfade_secs
        {
            let current = player.current_index();
            let not_yet_advanced = auto_advanced_index != Some(current);
            if not_yet_advanced && current + 1 < player.item_count() {
                auto_advanced_index = Some(current);
                let next = current + 1;
                switch_track(
                    player,
                    next,
                    urls,
                    ui_tx,
                    &mut ui,
                    &mut crossfade_clock,
                    &mut auto_advanced_index,
                )?;
                // Restore guard (switch_track resets it for manual switches)
                auto_advanced_index = Some(current);
            }
        }

        if event::poll(Duration::from_millis(CONTROL_POLL_MS))? {
            match event::read()? {
                TermEvent::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    match handle_key(key.code, key.modifiers, player, &mut ui.dashboard) {
                        ControlOutcome::Continue(Some(line)) => ui.log_line(&line)?,
                        ControlOutcome::Continue(None) => {}
                        ControlOutcome::SwitchTrack(index) => {
                            if index < player.item_count() {
                                switch_track(
                                    player,
                                    index,
                                    urls,
                                    ui_tx,
                                    &mut ui,
                                    &mut crossfade_clock,
                                    &mut auto_advanced_index,
                                )?;
                            }
                        }
                        ControlOutcome::Quit => break,
                    }
                }
                TermEvent::Resize(_, _) => {
                    ui.on_resize()?;
                }
                _ => {}
            }
        }

        ui.draw()?;
    }

    Ok(())
}

fn handle_key(
    key: KeyCode,
    modifiers: KeyModifiers,
    player: &PlayerImpl,
    dashboard: &mut Dashboard,
) -> ControlOutcome {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(key, KeyCode::Char('c')) {
        return ControlOutcome::Quit;
    }
    match key {
        KeyCode::Left => {
            ControlOutcome::Continue(Some(apply_seek(player, -SEEK_STEP_SECONDS_F64, dashboard)))
        }
        KeyCode::Right => {
            ControlOutcome::Continue(Some(apply_seek(player, SEEK_STEP_SECONDS_F64, dashboard)))
        }
        KeyCode::Up => {
            apply_volume(player, VOLUME_STEP, dashboard);
            ControlOutcome::Continue(None)
        }
        KeyCode::Down => {
            apply_volume(player, -VOLUME_STEP, dashboard);
            ControlOutcome::Continue(None)
        }
        KeyCode::Char('q') => ControlOutcome::Quit,
        KeyCode::Char(c @ '1'..='9') => {
            let index = (c as usize) - ('1' as usize);
            ControlOutcome::SwitchTrack(index)
        }
        _ => ControlOutcome::Continue(None),
    }
}

fn apply_seek(player: &PlayerImpl, delta_seconds: f64, dashboard: &mut Dashboard) -> String {
    let current = player.position_seconds().unwrap_or(0.0).max(0.0);
    let target = (current + delta_seconds).max(0.0);
    match player.seek_seconds(target) {
        Ok(()) => {
            dashboard.set_note(format!("seek {}", format_seconds(target)));
            dashboard.set_position(Duration::from_secs_f64(target));
            format!("seek target={}", format_seconds(target))
        }
        Err(err) => {
            let line = format!("seek failed target={} err={err}", format_seconds(target));
            dashboard.set_note(line.clone());
            line
        }
    }
}

fn apply_volume(player: &PlayerImpl, delta: f32, dashboard: &mut Dashboard) {
    let volume = (player.volume() + delta).clamp(0.0, 1.0);
    player.set_volume(volume);
    dashboard.set_volume(volume);
    dashboard.set_note(format!("volume {:.0}%", volume * 100.0));
}

fn refresh_dashboard(dashboard: &mut Dashboard, player: &PlayerImpl) {
    dashboard.set_playing(player.is_playing());
    dashboard.set_queue(player.current_index(), player.item_count());
    dashboard.set_volume(player.volume());

    let position = player.position_seconds();
    if let Some(position) = position.filter(|seconds| seconds.is_finite() && *seconds >= 0.0) {
        dashboard.set_position(Duration::from_secs_f64(position));
    } else {
        dashboard.set_position(Duration::ZERO);
    }

    let total = player.duration_seconds().and_then(|seconds| {
        if !seconds.is_finite() || seconds <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64(seconds))
    });
    dashboard.set_total(total);
}

fn switch_track(
    player: &PlayerImpl,
    index: usize,
    urls: &[String],
    ui_tx: &mpsc::Sender<UiMsg>,
    ui: &mut UiSession,
    crossfade_clock: &mut Option<CrossfadeClock>,
    auto_advanced_index: &mut Option<usize>,
) -> RunnerResult {
    let handle = tokio::runtime::Handle::current();
    let url = &urls[index];
    let resource = handle.block_on(async {
        let config = ResourceConfig::new(url)?;
        Resource::new(config).await
    })?;
    let events = resource.subscribe();
    player.replace_item(index, resource);

    let label = format!("src{}", index + 1);
    let tx = ui_tx.clone();
    handle.spawn(async move {
        let mut rx = events;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if tx
                        .send(UiMsg::Source {
                            event,
                            source: label.clone(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    match player.select_item(index, true) {
        Ok(()) => {
            *crossfade_clock = Some(CrossfadeClock::new(player.crossfade_duration()));
            ui.dashboard.set_crossfade_progress(Some(0.0));
            *auto_advanced_index = None;
            let note = format!(
                "crossfade to #{} ({:.1}s)",
                index + 1,
                player.crossfade_duration()
            );
            ui.dashboard.set_note(&note);
            ui.log_line(&note)?;
        }
        Err(e) => {
            let note = format!("switch failed: {e}");
            ui.dashboard.set_note(&note);
            ui.log_line(&note)?;
        }
    }
    Ok(())
}

fn forward_player_events(
    mut rx: tokio::sync::broadcast::Receiver<PlayerEvent>,
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

fn forward_engine_events(
    mut rx: tokio::sync::broadcast::Receiver<EngineEvent>,
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

fn forward_source_events(
    mut rx: tokio::sync::broadcast::Receiver<kithara::prelude::Event>,
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
