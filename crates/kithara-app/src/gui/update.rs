use iced::Task;
use kithara::abr::AbrMode;
use kithara_queue::{TrackId, Transition};
use tracing::error;

use super::{
    app::Kithara,
    message::{Message, Tab},
};

fn track_id_at(state: &Kithara, index: usize) -> Option<TrackId> {
    state.ui_state.tracks.get(index).map(|e| e.id)
}

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    match message {
        Message::TogglePlayPause => handle_toggle_play_pause(state),
        Message::Next => handle_next(state),
        Message::Prev => handle_prev(state),
        Message::SeekChanged(pos) => handle_seek_changed(state, pos),
        Message::SeekReleased => handle_seek_released(state),
        Message::VolumeChanged(vol) => handle_volume_changed(state, vol),
        Message::EqBandChanged(band, db) => handle_eq_band_changed(state, band, db),
        Message::PlayRateChanged(rate) => handle_play_rate_changed(state, rate),
        Message::CrossfadeChanged(secs) => handle_crossfade_changed(state, secs),
        Message::UrlChanged(text) => handle_url_changed(state, text),
        Message::AddUrl => handle_add_url(state),
        Message::ToggleMute => handle_toggle_mute(state),
        Message::EqResetAll => handle_eq_reset_all(state),
        Message::SelectTrack(idx) => handle_select_track(state, idx),
        Message::DeleteTrack => handle_delete_track(state),
        Message::TabSelected(tab) => handle_tab_selected(state, tab),
        Message::SetAbrMode(variant) => handle_set_abr_mode(state, variant),
        Message::Tick => handle_tick(state),
    }

    state.ui_state = state.controller.snapshot();
    Task::none()
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
    match state.controller.queue().set_eq_gain(band, db) {
        Ok(()) => {
            state.controller.mutate(|st| {
                if let Some(slot) = st.eq_bands.get_mut(band) {
                    *slot = db;
                }
            });
        }
        Err(e) => error!("set EQ gain band={band} db={db:.1} failed: {e:?}"),
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

fn handle_url_changed(state: &mut Kithara, text: String) {
    state.url_text = text;
}

fn handle_add_url(state: &mut Kithara) {
    let url = state.url_text.trim();
    if url.is_empty() {
        return;
    }
    state.controller.queue().append(url);
    state.url_text.clear();
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
    if let Some(handle) = state.controller.queue().current_abr_handle() {
        let mode = variant.map_or(AbrMode::Auto(None), AbrMode::Manual);
        if let Err(err) = handle.set_mode(mode) {
            error!(?err, ?variant, "SetAbrMode rejected by ABR state");
        }
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
