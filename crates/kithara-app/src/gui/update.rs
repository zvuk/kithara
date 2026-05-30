use iced::{Task, window};
use kithara::abr::AbrMode;
use kithara_queue::{RepeatMode, TrackId, Transition};
use tracing::{debug, error, info};

use super::{
    app::Kithara,
    dj,
    message::{Message, Tab},
    url_bar,
};

fn track_id_at(state: &Kithara, index: usize) -> Option<TrackId> {
    state.ui_state.tracks.get(index).map(|e| e.id)
}

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        Message::Dj(msg) => dj::handle(state, &msg),
        Message::Url(msg) => url_bar::handle(state, msg),
        Message::WindowCloseRequested(id) => handle_window_close_requested(state, id),
        other => {
            handle_player(state, &other);
            Task::none()
        }
    };

    state.ui_state = state.controller.snapshot();
    task
}

/// All player/UI messages that mutate state without scheduling a follow-up
/// task. Window/DJ messages are handled separately in [`update`].
fn handle_player(state: &mut Kithara, message: &Message) {
    match *message {
        Message::TogglePlayPause => handle_toggle_play_pause(state),
        Message::Next => handle_next(state),
        Message::Prev => handle_prev(state),
        Message::SeekChanged(pos) => handle_seek_changed(state, pos),
        Message::SeekReleased => handle_seek_released(state),
        Message::VolumeChanged(vol) => handle_volume_changed(state, vol),
        Message::EqBandChanged(band, db) => handle_eq_band_changed(state, band, db),
        Message::PlayRateChanged(rate) => handle_play_rate_changed(state, rate),
        Message::CrossfadeChanged(secs) => handle_crossfade_changed(state, secs),
        Message::ToggleMute => handle_toggle_mute(state),
        Message::ToggleShuffle => handle_toggle_shuffle(state),
        Message::ToggleRepeat => handle_toggle_repeat(state),
        Message::EqResetAll => handle_eq_reset_all(state),
        Message::SelectTrack(idx) => handle_select_track(state, idx),
        Message::DeleteTrack => handle_delete_track(state),
        Message::TabSelected(tab) => handle_tab_selected(state, tab),
        Message::SetAbrMode(variant) => handle_set_abr_mode(state, variant),
        Message::Tick => handle_tick(state),
        Message::Dj(_) | Message::Url(_) | Message::WindowCloseRequested(_) => {}
    }
}

fn handle_window_close_requested(state: &Kithara, id: window::Id) -> Task<Message> {
    if state.window_id == Some(id) {
        iced::exit()
    } else {
        window::close(id)
    }
}

fn handle_toggle_play_pause(state: &Kithara) {
    if state.ui_state.playing {
        state.controller.queue().pause();
    } else {
        state.controller.queue().play();
    }
}

fn handle_next(state: &Kithara) {
    let _ = state
        .controller
        .queue()
        .advance_to_next(Transition::Crossfade);
}

fn handle_prev(state: &Kithara) {
    let _ = state
        .controller
        .queue()
        .return_to_previous(Transition::Crossfade);
}

fn handle_seek_changed(state: &Kithara, pos: f64) {
    state.controller.mutate(|st| {
        st.is_seeking = true;
        st.seek_position = pos;
    });
}

fn handle_seek_released(state: &Kithara) {
    let target = state.controller.mutate(|st| {
        st.is_seeking = false;
        st.seek_position
    });
    if let Err(e) = state.controller.queue().seek(target) {
        error!("seek failed: {e:?}");
    }
}

fn handle_volume_changed(state: &mut Kithara, vol: f32) {
    state.controller.queue().set_volume(vol);
    state.controller.mutate(|st| st.volume = vol);
    if vol > 0.0 {
        state.previous_volume = vol;
    }
}

fn handle_eq_band_changed(state: &Kithara, band: usize, db: f32) {
    if band >= state.ui_state.eq_bands.len() {
        return;
    }
    // `eq_bands` is the user's desired EQ and the source of truth: record it
    // regardless of whether a playback slot exists yet. The listener re-applies
    // it to the engine once a track becomes active.
    state.controller.mutate(|st| {
        if let Some(slot) = st.eq_bands.get_mut(band) {
            *slot = db;
        }
    });
    if let Err(e) = state.controller.queue().set_eq_gain(band, db) {
        // Expected before playback starts (no active slot yet); the gain is
        // retained in `eq_bands` and pushed down when playback begins.
        debug!("set EQ gain band={band} db={db:.1} deferred: {e:?}");
    }
}

fn handle_play_rate_changed(state: &Kithara, rate: f32) {
    state.controller.queue().set_default_rate(rate);
    if state.ui_state.playing {
        state.controller.queue().play();
    }
    state.controller.mutate(|st| st.selected_rate = rate);
}

fn handle_crossfade_changed(state: &Kithara, secs: f32) {
    state.controller.queue().set_crossfade_duration(secs);
    state.controller.mutate(|st| st.crossfade = secs);
}

fn handle_toggle_mute(state: &mut Kithara) {
    if state.ui_state.volume > 0.0 {
        state.previous_volume = state.ui_state.volume;
        handle_volume_changed(state, 0.0);
    } else {
        let target = if state.previous_volume > 0.0 {
            state.previous_volume
        } else {
            0.5
        };
        handle_volume_changed(state, target);
    }
}

fn handle_toggle_shuffle(state: &Kithara) {
    let new = !state.ui_state.shuffle_enabled;
    state.controller.queue().set_shuffle(new);
    state.controller.mutate(|st| st.shuffle_enabled = new);
}

fn handle_toggle_repeat(state: &Kithara) {
    // `RepeatMode` is non_exhaustive; the wildcard covers `One` and any
    // future variant, both cycling back to `Off`.
    let next = match state.ui_state.repeat_mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        _ => RepeatMode::Off,
    };
    state.controller.queue().set_repeat(next);
    state.controller.mutate(|st| st.repeat_mode = next);
}

fn handle_eq_reset_all(state: &Kithara) {
    let band_count = state.ui_state.eq_bands.len();
    for band in 0..band_count {
        if let Err(e) = state.controller.queue().set_eq_gain(band, 0.0) {
            error!("reset EQ band={band} failed: {e:?}");
        }
    }
    state.controller.mutate(|st| {
        for slot in &mut st.eq_bands {
            *slot = 0.0;
        }
    });
}

fn handle_select_track(state: &mut Kithara, idx: usize) {
    if state.selected_track_index != Some(idx) {
        state.selected_track_index = Some(idx);
        return;
    }
    if let Some(id) = track_id_at(state, idx)
        && let Err(e) = state.controller.queue().select(id, Transition::None)
    {
        error!(index = idx, error = %e, "select failed");
    }
}

fn handle_delete_track(state: &mut Kithara) {
    let target = state
        .selected_track_index
        .or(state.ui_state.current_track_index);
    if let Some(idx) = target
        && let Some(id) = track_id_at(state, idx)
    {
        match state.controller.queue().remove(id) {
            Ok(()) => state.selected_track_index = None,
            Err(e) => error!(index = idx, error = %e, "remove failed"),
        }
    }
}

fn handle_tab_selected(state: &mut Kithara, tab: Tab) {
    state.active_tab = tab;
}

fn handle_set_abr_mode(state: &Kithara, variant: Option<usize>) {
    let handle = state.controller.queue().current_abr_handle();
    info!(
        ?variant,
        handle_present = handle.is_some(),
        "GUI: SetAbrMode received"
    );
    if let Some(handle) = handle {
        let mode = variant.map_or(AbrMode::Auto(None), AbrMode::manual);
        match handle.set_mode(mode) {
            Ok(()) => info!(?variant, ?mode, "GUI: set_mode accepted"),
            Err(err) => error!(?err, ?variant, "SetAbrMode rejected by ABR state"),
        }
    } else {
        error!(?variant, "GUI: no current AbrHandle — set_mode skipped");
    }
    state.controller.mutate(|st| {
        st.abr_mode_is_auto = variant.is_none();
        st.selected_variant = variant;
    });
}

fn handle_tick(state: &mut Kithara) {
    let _ = state.controller.queue().tick();
    state.controller.refresh_continuous();
    state.blink_counter = state.blink_counter.wrapping_add(1);
}
