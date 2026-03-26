use std::{
    sync::{
        Arc,
        mpsc::{self, TryRecvError},
    },
    time::Duration,
};

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use kithara::{
    play::{Engine, EngineEvent, PlayerEvent},
    prelude::PlayerImpl,
};
use tokio::{
    sync::broadcast::{Receiver, error::RecvError},
    task::{self, JoinHandle},
};

use super::{dashboard::Dashboard, session::UiSession};
use crate::{
    controls::{AppController, PlayerControls, TrackLoadParams},
    crossfade::{CrossfadeClock, ProgressLog},
    events::{UiMsg, format_seconds, is_progress_event, source_note},
    playlist::Playlist,
    theme::tui,
};

const CONTROL_POLL_MS: u64 = 100;
const SEEK_STEP_SECONDS_F64: f64 = 5.0;
const VOLUME_STEP: f32 = 0.05;
const MAX_DIGIT_TRACKS: usize = 9;
const PERCENT_SCALE: f32 = 100.0;

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
    config: &crate::config::AppConfig,
) -> RunnerResult {
    let palette: tui::TuiPalette = config.palette.into();
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let player = controller.player().clone();
    let load_params = controller.load_params();
    let playlist = Arc::clone(load_params.playlist());

    let forwarders = vec![
        forward_player_events(player.subscribe(), ui_tx.clone()),
        forward_engine_events(player.engine().subscribe(), ui_tx.clone()),
    ];

    // Reserve player slots for all tracks so replace_item works by index.
    player.reserve_slots(playlist.len());

    // Spawn async loading for each track.
    for i in 0..playlist.len() {
        let params = load_params.clone();
        let tx = ui_tx.clone();
        tokio::spawn(async move {
            let ok = params.load_and_apply(i).await;
            let msg = if ok {
                UiMsg::Note(format!("loaded #{}", i + 1))
            } else {
                UiMsg::Note(format!("failed #{}", i + 1))
            };
            let _ = tx.send(msg);
        });
    }

    // Run blocking UI loop in a separate thread.
    let ui_player = player.clone();
    let ui_playlist = Arc::clone(&playlist);
    let ui_load_params = load_params.clone();
    let mut ui_handle = task::spawn_blocking(move || {
        run_ui_loop(
            &ui_player,
            &ui_playlist,
            &ui_load_params,
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
    playlist: &Arc<Playlist>,
    load_params: &TrackLoadParams,
    ui_rx: &mpsc::Receiver<UiMsg>,
    stop_rx: &mpsc::Receiver<()>,
    palette: tui::TuiPalette,
) -> RunnerResult {
    let track_count = playlist.len();
    let dashboard = Dashboard::new(Arc::clone(playlist), palette);
    let mut ui = UiSession::new(dashboard)?;
    ui.log_line(&format!(
        "controls: space play/pause, 1-{} select track, Left/Right seek {SEEK_STEP_SECONDS_F64:.0}s, Up/Down vol {:+.0}%",
        track_count.min(MAX_DIGIT_TRACKS),
        VOLUME_STEP * PERCENT_SCALE
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
                                    .set_note(format!("volume {:.0}%", volume * PERCENT_SCALE));
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
                    load_params,
                    next,
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
                                    load_params,
                                    index,
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
        KeyCode::Char(' ') => {
            if player.is_playing() {
                player.pause();
            } else {
                player.play();
            }
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
    dashboard.set_note(format!("volume {:.0}%", volume * PERCENT_SCALE));
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
    load_params: &TrackLoadParams,
    index: usize,
    ui: &mut UiSession,
    crossfade_clock: &mut Option<CrossfadeClock>,
    auto_advanced_index: &mut Option<usize>,
) -> RunnerResult {
    let handle = tokio::runtime::Handle::current();

    // Use TrackLoadParams — THE ONLY place for DRM config + resource loading.
    let resource = handle.block_on(load_params.load_resource(index))?;
    player.replace_item(index, resource);

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

fn forward_player_events(mut rx: Receiver<PlayerEvent>, tx: mpsc::Sender<UiMsg>) -> JoinHandle<()> {
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

fn forward_engine_events(mut rx: Receiver<EngineEvent>, tx: mpsc::Sender<UiMsg>) -> JoinHandle<()> {
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
