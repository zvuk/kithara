use std::sync::Arc;

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use kithara::{
    abr::AbrMode,
    events::{AppEvent, Event},
};
use kithara_platform::time::Duration;
use kithara_queue::{Queue, QueueEvent, TrackId, Transition};
use tokio::{sync::broadcast::error::TryRecvError, task};

use super::{
    dashboard::{Dashboard, Tab},
    session::UiSession,
};
use crate::{
    crossfade::{CrossfadeClock, ProgressLog},
    events::{format_seconds, is_progress_event},
    state::{StateController, UiState},
    theme::tui,
};

struct Consts;
impl Consts {
    const CONTROL_POLL_MS: u64 = 50;
    const CROSSFADE_MAX_SECONDS: f32 = 8.0;
    const CROSSFADE_STEP_SECONDS: f32 = 0.5;
    const EQ_GAIN_STEP_DB: f32 = 1.0;
    const MAX_DIGIT_TRACKS: usize = 9;
    const PERCENT_SCALE: f32 = 100.0;
    const SEEK_STEP_SECONDS_F64: f64 = 5.0;
    const VOLUME_STEP: f32 = 0.05;
}

type RunnerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Run the TUI event loop.
///
/// # Errors
/// Returns an error if the TUI event loop fails.
pub(super) async fn run_tui(queue: Arc<Queue>, config: &crate::config::AppConfig) -> RunnerResult {
    let palette: tui::TuiPalette = config.palette.into();

    queue.set_tracks(crate::sources::build_sources(config));

    let bus = queue.bus().clone();
    let controller = Arc::new(StateController::new(
        Arc::clone(&queue),
        config.clone(),
        config.shutdown.child_token(),
    ));

    let mut ui_handle = task::spawn_blocking(move || run_ui_loop(&controller, &palette));

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

fn run_ui_loop(controller: &Arc<StateController>, palette: &tui::TuiPalette) -> RunnerResult {
    let dashboard = Dashboard::new(*palette);

    let mut event_rx = controller.queue().subscribe();

    let mut state = controller.snapshot();
    let mut ui = UiSession::new(dashboard, &state)?;
    ui.log_line(
        &format!(
            "controls: space play/pause, 1-{} select track, Tab cycle tabs, ←/→ seek/adjust, ↑/↓ vol/tweak {:+.0}%",
            Consts::MAX_DIGIT_TRACKS,
            Consts::VOLUME_STEP * Consts::PERCENT_SCALE
        ),
        &state,
    )?;
    ui.log_line(
        "auto-advances to next track with crossfade near end of each track",
        &state,
    )?;
    ui.draw(&state)?;

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
                        controller.mutate(|st| st.crossfade_progress = Some(0.0));
                    }
                    if handle_event(&event, &mut ui, &mut progress_log, &state)? {
                        break 'ui;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(n)) => {
                    ui.log_line(&format!("events lagged: {n}"), &state)?;
                }
                Err(TryRecvError::Closed) => break 'ui,
            }
        }

        match controller.queue().tick() {
            Ok(()) => tick_error_logged = false,
            Err(err) => {
                if !tick_error_logged {
                    ui.log_line(&format!("tick failed: {err}"), &state)?;
                    controller.mutate(|st| st.status_note = Some(format!("tick failed: {err}")));
                    tick_error_logged = true;
                }
            }
        }

        controller.refresh_continuous();

        if let Some(clock) = crossfade_clock.as_ref() {
            let progress = clock.progress();
            controller.mutate(|st| st.crossfade_progress = Some(progress));
            if progress >= 1.0 {
                crossfade_clock = None;
            }
        } else {
            controller.mutate(|st| st.crossfade_progress = None);
        }

        let queue = controller.queue();
        let crossfade_secs = f64::from(queue.crossfade_duration());
        if let (Some(pos), Some(dur)) = (queue.position_seconds(), queue.duration_seconds())
            && dur > crossfade_secs
            && pos >= dur - crossfade_secs
        {
            let current = queue.current_index().unwrap_or(0);
            let not_yet_advanced = auto_advanced_index != Some(current);
            if not_yet_advanced && current + 1 < queue.len() {
                auto_advanced_index = Some(current);
                let _ = queue.advance_to_next(Transition::Crossfade);
            }
        }

        if event::poll(Duration::from_millis(Consts::CONTROL_POLL_MS))? {
            state = controller.snapshot();
            match event::read()? {
                TermEvent::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    match handle_key(key.code, key.modifiers, controller, &mut ui.dashboard) {
                        ControlOutcome::Continue(Some(line)) => ui.log_line(&line, &state)?,
                        ControlOutcome::Continue(None) => {}
                        ControlOutcome::SwitchTrack(index) => {
                            if let Some(id) = controller.queue().tracks().get(index).map(|t| t.id) {
                                switch_to_id(
                                    controller,
                                    id,
                                    index,
                                    &mut ui,
                                    &mut auto_advanced_index,
                                    &state,
                                )?;
                            }
                        }
                        ControlOutcome::DeleteCurrent => {
                            if let Some(id) = controller.queue().current().map(|e| e.id) {
                                match controller.queue().remove(id) {
                                    Ok(()) => {
                                        auto_advanced_index = None;
                                        ui.log_line(
                                            &format!("removed track {}", id.as_u64()),
                                            &state,
                                        )?;
                                    }
                                    Err(e) => {
                                        ui.log_line(&format!("remove failed: {e}"), &state)?;
                                    }
                                }
                            }
                        }
                        ControlOutcome::Quit => break,
                    }
                }
                TermEvent::Resize(_, _) => ui.on_resize(&state)?,
                _ => {}
            }
        }

        state = controller.snapshot();
        ui.draw(&state)?;
    }

    Ok(())
}

/// Returns `true` if the UI loop should exit.
fn handle_event(
    event: &Event,
    ui: &mut UiSession,
    progress_log: &mut ProgressLog,
    state: &UiState,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    match event {
        Event::App(AppEvent::Stop) => return Ok(true),
        Event::App(AppEvent::Note(note)) => {
            ui.log_line(note, state)?;
        }
        Event::Player(pe) => {
            ui.log_line(&format!("player {pe:?}"), state)?;
        }
        Event::Engine(ee) => {
            ui.log_line(&format!("engine {ee:?}"), state)?;
        }
        Event::Hls(_) | Event::File(_) | Event::Audio(_) => {
            if !is_progress_event(event) || progress_log.should_emit() {
                ui.log_line(&format!("{event:?}"), state)?;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_key(
    key: KeyCode,
    modifiers: KeyModifiers,
    controller: &StateController,
    dashboard: &mut Dashboard,
) -> ControlOutcome {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(key, KeyCode::Char('c')) {
        return ControlOutcome::Quit;
    }

    if key == KeyCode::Tab {
        dashboard.active_tab = match dashboard.active_tab {
            Tab::Playlist => Tab::Equalizer,
            Tab::Equalizer => Tab::Settings,
            Tab::Settings => Tab::Playlist,
        };
        return ControlOutcome::Continue(None);
    }

    match dashboard.active_tab {
        Tab::Playlist => {}
        Tab::Equalizer => return handle_eq_key(key, controller, dashboard),
        Tab::Settings => return handle_settings_key(key, controller, dashboard),
    }

    match key {
        KeyCode::Left => {
            ControlOutcome::Continue(Some(apply_seek(controller, -Consts::SEEK_STEP_SECONDS_F64)))
        }
        KeyCode::Right => {
            ControlOutcome::Continue(Some(apply_seek(controller, Consts::SEEK_STEP_SECONDS_F64)))
        }
        KeyCode::Up => {
            apply_volume(controller, Consts::VOLUME_STEP);
            ControlOutcome::Continue(None)
        }
        KeyCode::Down => {
            apply_volume(controller, -Consts::VOLUME_STEP);
            ControlOutcome::Continue(None)
        }
        KeyCode::Char(' ') => {
            if controller.queue().is_playing() {
                controller.queue().pause();
            } else {
                controller.queue().play();
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

fn handle_eq_key(
    key: KeyCode,
    controller: &StateController,
    dashboard: &mut Dashboard,
) -> ControlOutcome {
    match key {
        KeyCode::Left => {
            dashboard.selected_eq_band = dashboard.selected_eq_band.saturating_sub(1);
            ControlOutcome::Continue(None)
        }
        KeyCode::Right => {
            let max = controller.queue().eq_band_count().saturating_sub(1);
            dashboard.selected_eq_band = dashboard.selected_eq_band.saturating_add(1).min(max);
            ControlOutcome::Continue(None)
        }
        KeyCode::Up => {
            adjust_eq(controller, dashboard, Consts::EQ_GAIN_STEP_DB);
            ControlOutcome::Continue(None)
        }
        KeyCode::Down => {
            adjust_eq(controller, dashboard, -Consts::EQ_GAIN_STEP_DB);
            ControlOutcome::Continue(None)
        }
        _ => ControlOutcome::Continue(None),
    }
}

fn adjust_eq(controller: &StateController, dashboard: &Dashboard, delta: f32) {
    let band = dashboard.selected_eq_band;
    let snapshot = controller.snapshot();
    let Some(&current) = snapshot.eq_bands.get(band) else {
        return;
    };
    let target = (current + delta).clamp(Dashboard::EQ_GAIN_MIN, Dashboard::EQ_GAIN_MAX);
    if let Err(err) = controller.queue().set_eq_gain(band, target) {
        controller.mutate(|st| st.status_note = Some(format!("eq band={band} failed: {err}")));
        return;
    }
    controller.mutate(|st| {
        if let Some(slot) = st.eq_bands.get_mut(band) {
            *slot = target;
        }
    });
}

fn handle_settings_key(
    key: KeyCode,
    controller: &StateController,
    dashboard: &mut Dashboard,
) -> ControlOutcome {
    match key {
        KeyCode::Up => {
            dashboard.selected_setting_row = dashboard.selected_setting_row.saturating_sub(1);
            ControlOutcome::Continue(None)
        }
        KeyCode::Down => {
            dashboard.selected_setting_row = dashboard
                .selected_setting_row
                .saturating_add(1)
                .min(Dashboard::SETTINGS_ROW_COUNT.saturating_sub(1));
            ControlOutcome::Continue(None)
        }
        KeyCode::Left => {
            adjust_setting(controller, dashboard, -1);
            ControlOutcome::Continue(None)
        }
        KeyCode::Right => {
            adjust_setting(controller, dashboard, 1);
            ControlOutcome::Continue(None)
        }
        _ => ControlOutcome::Continue(None),
    }
}

fn adjust_setting(controller: &StateController, dashboard: &Dashboard, dir: i32) {
    match dashboard.selected_setting_row {
        0 => cycle_quality(controller, dir),
        1 => adjust_crossfade(controller, dir),
        _ => {}
    }
}

fn cycle_quality(controller: &StateController, dir: i32) {
    let snapshot = controller.snapshot();
    if snapshot.abr_variants.is_empty() {
        return;
    }
    let current_pos: isize = if snapshot.abr_mode_is_auto {
        -1
    } else {
        snapshot
            .selected_variant
            .and_then(|idx| {
                snapshot
                    .abr_variants
                    .iter()
                    .position(|(i, _)| *i == idx)
                    .and_then(|p| isize::try_from(p).ok())
            })
            .unwrap_or(-1)
    };
    let last_pos = isize::try_from(snapshot.abr_variants.len() - 1).unwrap_or(isize::MAX);
    let next_pos = (current_pos + isize::try_from(dir).unwrap_or(0)).clamp(-1, last_pos);
    let chosen = if next_pos < 0 {
        None
    } else {
        usize::try_from(next_pos)
            .ok()
            .and_then(|p| snapshot.abr_variants.get(p).map(|(i, _)| *i))
    };

    if let Some(handle) = controller.queue().current_abr_handle() {
        let mode = chosen.map_or(AbrMode::Auto(None), AbrMode::manual);
        if let Err(err) = handle.set_mode(mode) {
            controller.mutate(|st| st.status_note = Some(format!("abr failed: {err}")));
            return;
        }
    }
    controller.mutate(|st| {
        st.abr_mode_is_auto = chosen.is_none();
        st.selected_variant = chosen;
    });
}

fn adjust_crossfade(controller: &StateController, dir: i32) {
    let current = controller.queue().crossfade_duration();
    let step = Consts::CROSSFADE_STEP_SECONDS * f32::from(i8::try_from(dir).unwrap_or(0));
    let target = (current + step).clamp(0.0, Consts::CROSSFADE_MAX_SECONDS);
    controller.queue().set_crossfade_duration(target);
    controller.mutate(|st| st.crossfade = target);
}

fn apply_seek(controller: &StateController, delta_seconds: f64) -> String {
    let current = controller
        .queue()
        .position_seconds()
        .unwrap_or(0.0)
        .max(0.0);
    let target = (current + delta_seconds).max(0.0);
    match controller.queue().seek(target) {
        Ok(_outcome) => {
            controller.mutate(|st| {
                st.status_note = Some(format!("seek {}", format_seconds(target)));
            });
            format!("seek target={}", format_seconds(target))
        }
        Err(err) => {
            let line = format!("seek failed target={} err={err}", format_seconds(target));
            controller.mutate(|st| st.status_note = Some(line.clone()));
            line
        }
    }
}

fn apply_volume(controller: &StateController, delta: f32) {
    let volume = (controller.queue().volume() + delta).clamp(0.0, 1.0);
    controller.queue().set_volume(volume);
    controller.mutate(|st| {
        st.volume = volume;
        st.status_note = Some(format!("volume {:.0}%", volume * Consts::PERCENT_SCALE));
    });
}

fn switch_to_id(
    controller: &StateController,
    id: TrackId,
    index: usize,
    ui: &mut UiSession,
    auto_advanced_index: &mut Option<usize>,
    state: &UiState,
) -> RunnerResult {
    match controller.queue().select(id, Transition::None) {
        Ok(()) => {
            *auto_advanced_index = None;
            let note = format!("switch to #{} (immediate)", index + 1);
            controller.mutate(|st| st.status_note = Some(note.clone()));
            ui.log_line(&note, state)?;
        }
        Err(e) => {
            let note = format!("switch failed: {e}");
            controller.mutate(|st| st.status_note = Some(note.clone()));
            ui.log_line(&note, state)?;
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
