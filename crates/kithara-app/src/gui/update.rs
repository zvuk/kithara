use iced::Task;
use kithara_queue::{TrackId, Transition};
use tracing::error;

use super::{
    app::Kithara,
    message::{Message, Tab},
};

fn track_id_at(state: &Kithara, index: usize) -> Option<TrackId> {
    state.tracks_snapshot.get(index).map(|e| e.id)
}

fn track_name_at(state: &Kithara, index: usize) -> String {
    state
        .tracks_snapshot
        .get(index)
        .map(|e| e.name.clone())
        .unwrap_or_default()
}

fn handle_next(state: &mut Kithara) {
    // Next button → crossfade, matching auto-advance feel.
    if state.queue.advance_to_next(Transition::Crossfade).is_some() {
        if let Some(idx) = state.queue.current_index() {
            state.current_track_index = Some(idx);
            state.track_name = track_name_at(state, idx);
        }
        state.variant_label.clear();
        state.abr_mode_is_auto = true;
        state.selected_variant = None;
    }
}

fn handle_prev(state: &mut Kithara) {
    // Prev button → crossfade, symmetric with Next.
    if state
        .queue
        .return_to_previous(Transition::Crossfade)
        .is_some()
    {
        if let Some(idx) = state.queue.current_index() {
            state.current_track_index = Some(idx);
            state.track_name = track_name_at(state, idx);
        }
        state.variant_label.clear();
    }
}

fn handle_select_track(state: &mut Kithara, idx: usize) {
    // First click highlights, second click on the same row plays.
    // Matches file-browser UX: click = focus, double-click = open.
    if state.selected_track_index != Some(idx) {
        state.selected_track_index = Some(idx);
    } else if let Some(id) = track_id_at(state, idx) {
        if let Err(e) = state.queue.select(id, Transition::None) {
            error!(index = idx, error = %e, "select failed");
        }
        state.current_track_index = Some(idx);
        state.track_name = track_name_at(state, idx);
        state.variant_label.clear();
        state.abr_mode_is_auto = true;
        state.selected_variant = None;
    }
}

fn handle_delete_track(state: &mut Kithara) {
    // Prefer the highlighted row; fall back to the playing one.
    let target_idx = state.selected_track_index.or(state.current_track_index);
    if let Some(idx) = target_idx
        && let Some(id) = track_id_at(state, idx)
    {
        match state.queue.remove(id) {
            Ok(()) => {
                state.selected_track_index = None;
                // Queue::remove auto-advances if we removed the
                // current track, or pauses on empty queue.
            }
            Err(e) => error!(index = idx, error = %e, "remove failed"),
        }
    }
}

fn handle_set_abr_mode(state: &mut Kithara, variant: Option<usize>) {
    state.abr_mode_is_auto = variant.is_none();
    state.selected_variant = variant;
    if let Some(handle) = state.queue.current_abr_handle() {
        let mode = variant.map_or(kithara::abr::AbrMode::Auto(None), |idx| {
            kithara::abr::AbrMode::Manual(idx)
        });
        if let Err(err) = handle.set_mode(mode) {
            error!(?err, ?variant, "SetAbrMode rejected by ABR state");
        }
    }
}

fn handle_tick(state: &mut Kithara) {
    use num_traits::cast::AsPrimitive;

    let _ = state.queue.tick();
    state.blink_counter = state.blink_counter.wrapping_add(1);

    // Refresh track snapshot for view rendering.
    state.tracks_snapshot = state.queue.tracks();

    // Sync variant label and ABR variants from background listener.
    if let Ok(label) = state.shared_variant_label.lock()
        && *label != state.variant_label
    {
        state.variant_label.clone_from(&label);
    }
    if let Ok(sv) = state.shared_abr_variants.lock()
        && *sv != state.abr_variants
    {
        state.abr_variants.clone_from(&sv);
    }

    // Sync playback state.
    state.playing = state.queue.is_playing();
    if let Some(idx) = state.queue.current_index() {
        state.current_track_index = Some(idx);
        state.track_name = track_name_at(state, idx);
    }
    state.duration = state.queue.duration_seconds().unwrap_or(0.0).as_();
    if !state.is_seeking {
        state.position = state.queue.position_seconds().unwrap_or(0.0).as_();
    }
}

/// Handle all messages (Elm `update` function). Each Message variant
/// dispatches to a small handler so the central match stays a flat switch
/// with no per-arm logic — keeps cognitive complexity bounded as new
/// messages are added.
///
/// `message: Message` is by value because that is iced's `Application::update`
/// signature contract; the silencer is feature-gated so it only narrows the
/// GUI build where the iced trait actually applies.
#[cfg_attr(
    feature = "gui",
    expect(
        clippy::needless_pass_by_value,
        reason = "iced Application::update requires owned Message"
    )
)]
pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    match message {
        Message::TogglePlayPause => handle_toggle_play_pause(state),
        Message::Next => {
            handle_next(state);
            Task::none()
        }
        Message::Prev => {
            handle_prev(state);
            Task::none()
        }
        Message::SeekChanged(pos) => handle_seek_changed(state, pos),
        Message::SeekReleased => handle_seek_released(state),
        Message::VolumeChanged(vol) => handle_volume_changed(state, vol),
        Message::EqBandChanged(band, db) => handle_eq_band_changed(state, band, db),
        Message::EqBandReset(band) => handle_eq_band_reset(state, band),
        Message::PlayRateChanged(rate) => handle_play_rate_changed(state, rate),
        Message::CrossfadeChanged(secs) => handle_crossfade_changed(state, secs),
        Message::SelectTrack(idx) => {
            handle_select_track(state, idx);
            Task::none()
        }
        Message::DeleteTrack => {
            handle_delete_track(state);
            Task::none()
        }
        Message::TabSelected(tab) => handle_tab_selected(state, tab),
        Message::SetAbrMode(variant) => {
            handle_set_abr_mode(state, variant);
            Task::none()
        }
        Message::Tick => {
            handle_tick(state);
            Task::none()
        }
        Message::ToggleShuffle => handle_toggle_shuffle(state),
        Message::ToggleRepeat => handle_toggle_repeat(state),
    }
}

fn handle_toggle_play_pause(state: &mut Kithara) -> Task<Message> {
    if state.playing {
        state.queue.pause();
    } else {
        state.queue.play();
    }
    state.playing = !state.playing;
    Task::none()
}

fn handle_seek_changed(state: &mut Kithara, pos: f32) -> Task<Message> {
    state.is_seeking = true;
    state.seek_position = pos;
    Task::none()
}

fn handle_seek_released(state: &mut Kithara) -> Task<Message> {
    state.is_seeking = false;
    if let Err(e) = state.queue.seek(f64::from(state.seek_position)) {
        error!("seek failed: {e:?}");
    }
    Task::none()
}

fn handle_volume_changed(state: &mut Kithara, vol: f32) -> Task<Message> {
    state.volume = vol;
    state.queue.set_volume(vol);
    Task::none()
}

fn handle_eq_band_changed(state: &mut Kithara, band: usize, db: f32) -> Task<Message> {
    if band < state.eq_bands.len() {
        state.eq_bands[band] = db;
        match state.queue.set_eq_gain(band, db) {
            Ok(()) => tracing::info!("EQ band={band} gain={db:.1} dB — OK"),
            Err(e) => error!("set EQ gain band={band} db={db:.1} failed: {e:?}"),
        }
    }
    Task::none()
}

fn handle_eq_band_reset(state: &mut Kithara, band: usize) -> Task<Message> {
    if band < state.eq_bands.len() {
        state.eq_bands[band] = 0.0;
        if let Err(e) = state.queue.set_eq_gain(band, 0.0) {
            error!("reset EQ band={band} failed: {e:?}");
        }
    }
    Task::none()
}

fn handle_play_rate_changed(state: &mut Kithara, rate: f32) -> Task<Message> {
    state.selected_rate = rate;
    state.queue.set_default_rate(rate);
    if state.playing {
        state.queue.play();
    }
    Task::none()
}

fn handle_crossfade_changed(state: &mut Kithara, secs: f32) -> Task<Message> {
    state.crossfade = secs;
    state.queue.set_crossfade_duration(secs);
    Task::none()
}

fn handle_tab_selected(state: &mut Kithara, tab: Tab) -> Task<Message> {
    state.active_tab = tab;
    Task::none()
}

fn handle_toggle_shuffle(state: &mut Kithara) -> Task<Message> {
    let new = !state.queue.is_shuffle_enabled();
    state.queue.set_shuffle(new);
    state.shuffle_enabled = new;
    Task::none()
}

fn handle_toggle_repeat(state: &mut Kithara) -> Task<Message> {
    state.repeat_enabled = !state.repeat_enabled;
    Task::none()
}
