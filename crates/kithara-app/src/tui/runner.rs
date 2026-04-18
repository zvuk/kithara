use std::{sync::Arc, time::Duration};

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use kithara::events::{AppEvent, EngineEvent, Event, EventReceiver, PlayerEvent};
use kithara_queue::{Queue, QueueEvent, TrackId, Transition};
use tokio::{sync::broadcast::error::TryRecvError, task};

use super::{dashboard::Dashboard, session::UiSession};
use crate::{
    crossfade::{CrossfadeClock, ProgressLog},
    events::{format_seconds, is_progress_event, source_note},
    theme::tui,
};

struct Consts;
impl Consts {
    const CONTROL_POLL_MS: u64 = 50;
    const SEEK_STEP_SECONDS_F64: f64 = 5.0;
    const VOLUME_STEP: f32 = 0.05;
    const PERCENT_SCALE: f32 = 100.0;
    const MAX_DIGIT_TRACKS: usize = 9;
}

type RunnerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Run the TUI event loop.
///
/// # Errors
/// Returns an error if the TUI event loop fails.
pub(super) async fn run_tui(queue: Arc<Queue>, config: &crate::config::AppConfig) -> RunnerResult {
    let palette: tui::TuiPalette = config.palette.into();

    // Feed tracks from inside the runtime — `Loader::spawn_load` uses
    // `tokio::spawn`, which requires a running reactor.
    queue.set_tracks(crate::sources::build_sources(config));

    let event_rx = queue.subscribe();
    let bus = queue.player().bus().clone();

    let queue_for_loop = Arc::clone(&queue);
    let mut ui_handle =
        task::spawn_blocking(move || run_ui_loop(&queue_for_loop, event_rx, palette));

    let ui_finished = tokio::select! {
        result = &mut ui_handle => {
            result??;
            true
        }
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() {
                bus.publish(AppEvent::Stop);
            }
            false
        }
    };

    if !ui_finished {
        ui_handle.await??;
    }

    queue.pause();

    Ok(())
}

fn run_ui_loop(
    queue: &Queue,
    mut event_rx: EventReceiver,
    palette: tui::TuiPalette,
) -> RunnerResult {
    let mut dashboard = Dashboard::new(palette);
    dashboard.refresh_tracks(queue);
    let track_count = dashboard.track_count();

    let mut ui = UiSession::new(dashboard)?;
    ui.log_line(&format!(
        "controls: space play/pause, 1-{} select track, Left/Right seek {SEEK_STEP_SECONDS_F64:.0}s, Up/Down vol {:+.0}%",
        track_count.min(Consts::MAX_DIGIT_TRACKS),
        Consts::VOLUME_STEP * Consts::PERCENT_SCALE,
        SEEK_STEP_SECONDS_F64 = Consts::SEEK_STEP_SECONDS_F64
    ))?;
    ui.log_line("auto-advances to next track with crossfade near end of each track")?;
    ui.draw()?;

    let mut progress_log = ProgressLog::new();
    let mut crossfade_clock: Option<CrossfadeClock> = None;
    let mut auto_advanced_index: Option<usize> = None;
    let mut tick_error_logged = false;

    'ui: loop {
        loop {
            match event_rx.try_recv() {
                Ok(event) => {
                    if let Event::Queue(QueueEvent::CrossfadeStarted { duration_seconds }) = &event
                    {
                        crossfade_clock = Some(CrossfadeClock::new(*duration_seconds));
                        ui.dashboard.set_crossfade_progress(Some(0.0));
                    }
                    if handle_event(&event, &mut ui, &mut progress_log)? {
                        break 'ui;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(n)) => {
                    ui.log_line(&format!("events lagged: {n}"))?;
                }
                Err(TryRecvError::Closed) => break 'ui,
            }
        }

        match queue.tick() {
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

        ui.dashboard.refresh_tracks(queue);
        refresh_dashboard(&mut ui.dashboard, queue);

        if let Some(clock) = crossfade_clock.as_ref() {
            let progress = clock.progress();
            ui.dashboard.set_crossfade_progress(Some(progress));
            if progress >= 1.0 {
                crossfade_clock = None;
            }
        } else {
            ui.dashboard.set_crossfade_progress(None);
        }

        // Auto-advance: crossfade to next track when remaining time <= crossfade duration.
        let crossfade_secs = f64::from(queue.crossfade_duration());
        if let (Some(pos), Some(dur)) = (queue.position_seconds(), queue.duration_seconds())
            && dur > crossfade_secs
            && pos >= dur - crossfade_secs
        {
            let current = queue.current_index().unwrap_or(0);
            let not_yet_advanced = auto_advanced_index != Some(current);
            if not_yet_advanced && current + 1 < queue.len() {
                auto_advanced_index = Some(current);
                // Clock is armed by QueueEvent::CrossfadeStarted on the bus.
                let _ = queue.advance_to_next(Transition::Crossfade);
            }
        }

        if event::poll(Duration::from_millis(Consts::CONTROL_POLL_MS))? {
            match event::read()? {
                TermEvent::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    match handle_key(key.code, key.modifiers, queue, &mut ui.dashboard) {
                        ControlOutcome::Continue(Some(line)) => ui.log_line(&line)?,
                        ControlOutcome::Continue(None) => {}
                        ControlOutcome::SwitchTrack(index) => {
                            if let Some(id) = track_id_at(queue, index) {
                                switch_to_id(queue, id, index, &mut ui, &mut auto_advanced_index)?;
                            }
                        }
                        ControlOutcome::DeleteCurrent => {
                            if let Some(id) = queue.current().map(|e| e.id) {
                                match queue.remove(id) {
                                    Ok(()) => {
                                        auto_advanced_index = None;
                                        ui.log_line(&format!("removed track {}", id.as_u64()))?;
                                    }
                                    Err(e) => ui.log_line(&format!("remove failed: {e}"))?,
                                }
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

fn track_id_at(queue: &Queue, index: usize) -> Option<TrackId> {
    queue.tracks().get(index).map(|e| e.id)
}

/// Returns `true` if the UI loop should exit.
fn handle_event(
    event: &Event,
    ui: &mut UiSession,
    progress_log: &mut ProgressLog,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    match event {
        Event::App(AppEvent::Stop) => return Ok(true),
        Event::App(AppEvent::Note(note)) => {
            ui.dashboard.set_note(note.clone());
            ui.log_line(note)?;
        }
        Event::Player(pe) => {
            match pe {
                PlayerEvent::CurrentItemChanged => {
                    ui.dashboard.set_note("track changed");
                }
                PlayerEvent::RateChanged { rate } => {
                    ui.dashboard.set_playing(*rate > 0.0);
                    ui.dashboard.set_note(format!("rate {rate:.2}"));
                }
                PlayerEvent::StatusChanged { status } => {
                    ui.dashboard.set_note(format!("status {status:?}"));
                }
                PlayerEvent::VolumeChanged { volume } => {
                    ui.dashboard.set_volume(*volume);
                    ui.dashboard
                        .set_note(format!("volume {:.0}%", volume * Consts::PERCENT_SCALE));
                }
                _ => {}
            }
            ui.log_line(&format!("player {pe:?}"))?;
        }
        Event::Engine(ee) => {
            if let EngineEvent::MasterVolumeChanged { volume } = ee {
                ui.dashboard.set_volume(*volume);
            }
            ui.log_line(&format!("engine {ee:?}"))?;
        }
        Event::Hls(_) | Event::File(_) | Event::Audio(_) => {
            if let Some(note) = source_note("src", event) {
                ui.dashboard.set_note(note);
            }
            if !is_progress_event(event) || progress_log.should_emit() {
                ui.log_line(&format!("{event:?}"))?;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_key(
    key: KeyCode,
    modifiers: KeyModifiers,
    queue: &Queue,
    dashboard: &mut Dashboard,
) -> ControlOutcome {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(key, KeyCode::Char('c')) {
        return ControlOutcome::Quit;
    }
    match key {
        KeyCode::Left => ControlOutcome::Continue(Some(apply_seek(
            queue,
            -Consts::SEEK_STEP_SECONDS_F64,
            dashboard,
        ))),
        KeyCode::Right => ControlOutcome::Continue(Some(apply_seek(
            queue,
            Consts::SEEK_STEP_SECONDS_F64,
            dashboard,
        ))),
        KeyCode::Up => {
            apply_volume(queue, Consts::VOLUME_STEP, dashboard);
            ControlOutcome::Continue(None)
        }
        KeyCode::Down => {
            apply_volume(queue, -Consts::VOLUME_STEP, dashboard);
            ControlOutcome::Continue(None)
        }
        KeyCode::Char(' ') => {
            if queue.is_playing() {
                queue.pause();
            } else {
                queue.play();
            }
            ControlOutcome::Continue(None)
        }
        KeyCode::Char('q') => ControlOutcome::Quit,
        KeyCode::Char(c @ '1'..='9') => {
            let index = (c as usize) - ('1' as usize);
            ControlOutcome::SwitchTrack(index)
        }
        KeyCode::Delete | KeyCode::Backspace => ControlOutcome::DeleteCurrent,
        _ => ControlOutcome::Continue(None),
    }
}

fn apply_seek(queue: &Queue, delta_seconds: f64, dashboard: &mut Dashboard) -> String {
    let current = queue.position_seconds().unwrap_or(0.0).max(0.0);
    let target = (current + delta_seconds).max(0.0);
    match queue.seek(target) {
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

fn apply_volume(queue: &Queue, delta: f32, dashboard: &mut Dashboard) {
    let volume = (queue.volume() + delta).clamp(0.0, 1.0);
    queue.set_volume(volume);
    dashboard.set_volume(volume);
    dashboard.set_note(format!("volume {:.0}%", volume * Consts::PERCENT_SCALE));
}

fn refresh_dashboard(dashboard: &mut Dashboard, queue: &Queue) {
    dashboard.set_playing(queue.is_playing());
    let current = queue.current_index().unwrap_or(0);
    dashboard.set_queue(current, queue.len());
    dashboard.set_volume(queue.volume());

    let position = queue.position_seconds();
    if let Some(position) = position.filter(|seconds| seconds.is_finite() && *seconds >= 0.0) {
        dashboard.set_position(Duration::from_secs_f64(position));
    } else {
        dashboard.set_position(Duration::ZERO);
    }

    let total = queue.duration_seconds().and_then(|seconds| {
        if !seconds.is_finite() || seconds <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64(seconds))
    });
    dashboard.set_total(total);
}

fn switch_to_id(
    queue: &Queue,
    id: TrackId,
    index: usize,
    ui: &mut UiSession,
    auto_advanced_index: &mut Option<usize>,
) -> RunnerResult {
    // Manual selection from the playlist — immediate cut (no crossfade)
    // matches the AVQueuePlayer user-initiated-selection idiom.
    match queue.select(id, Transition::None) {
        Ok(()) => {
            *auto_advanced_index = None;
            let note = format!("switch to #{} (immediate)", index + 1);
            ui.dashboard.set_note(&note);
            ui.log_line(&note)?;
            // Clock is armed by QueueEvent::CrossfadeStarted on the bus.
        }
        Err(e) => {
            let note = format!("switch failed: {e}");
            ui.dashboard.set_note(&note);
            ui.log_line(&note)?;
        }
    }
    Ok(())
}

enum ControlOutcome {
    Continue(Option<String>),
    SwitchTrack(usize),
    DeleteCurrent,
    Quit,
}
