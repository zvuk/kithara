use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use kithara::prelude::PlayerImpl;
use kithara_tui::Dashboard;

use crate::events::format_seconds;

const SEEK_STEP_SECONDS_F64: f64 = 5.0;
const VOLUME_STEP: f32 = 0.05;

pub enum ControlOutcome {
    Continue(Option<String>),
    SwitchTrack(usize),
    Quit,
}

pub fn handle_key(
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
