use iced::Task;
use kithara::abr::AbrMode;
use kithara_queue::{TrackId, Transition};
use tracing::error;

use super::{app::Kithara, message::Message};

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

/// Handle all messages (Elm `update` function).
#[expect(
    clippy::cognitive_complexity,
    reason = "single match over Message enum"
)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "iced `update` signature requires `Message` by value"
)]
pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    match message {
        Message::TogglePlayPause => {
            if state.playing {
                state.queue.pause();
            } else {
                state.queue.play();
            }
            state.playing = !state.playing;
            Task::none()
        }

        Message::Next => {
            // Next button → crossfade, matching auto-advance feel.
            if state.queue.advance_to_next(Transition::Crossfade).is_some() {
                let current = state.queue.current_index();
                if let Some(idx) = current {
                    state.current_track_index = Some(idx);
                    state.track_name = track_name_at(state, idx);
                }
                state.variant_label.clear();
                state.abr_mode_is_auto = true;
                state.selected_variant = None;
            }
            Task::none()
        }

        Message::Prev => {
            // Prev button → crossfade, symmetric with Next.
            if state
                .queue
                .return_to_previous(Transition::Crossfade)
                .is_some()
            {
                let current = state.queue.current_index();
                if let Some(idx) = current {
                    state.current_track_index = Some(idx);
                    state.track_name = track_name_at(state, idx);
                }
                state.variant_label.clear();
            }
            Task::none()
        }

        Message::SeekChanged(pos) => {
            state.is_seeking = true;
            state.seek_position = pos;
            Task::none()
        }

        Message::SeekReleased => {
            state.is_seeking = false;
            if let Err(e) = state.queue.seek(f64::from(state.seek_position)) {
                error!("seek failed: {e:?}");
            }
            Task::none()
        }

        Message::VolumeChanged(vol) => {
            state.volume = vol;
            state.queue.set_volume(vol);
            Task::none()
        }

        Message::EqBandChanged(band, db) => {
            if band < state.eq_bands.len() {
                state.eq_bands[band] = db;
                match state.queue.set_eq_gain(band, db) {
                    Ok(()) => tracing::info!("EQ band={band} gain={db:.1} dB — OK"),
                    Err(e) => error!("set EQ gain band={band} db={db:.1} failed: {e:?}"),
                }
            }
            Task::none()
        }

        Message::EqBandReset(band) => {
            if band < state.eq_bands.len() {
                state.eq_bands[band] = 0.0;
                if let Err(e) = state.queue.set_eq_gain(band, 0.0) {
                    error!("reset EQ band={band} failed: {e:?}");
                }
            }
            Task::none()
        }

        Message::PlayRateChanged(rate) => {
            state.selected_rate = rate;
            state.queue.set_default_rate(rate);
            if state.playing {
                state.queue.play();
            }
            Task::none()
        }

        Message::CrossfadeChanged(secs) => {
            state.crossfade = secs;
            state.queue.set_crossfade_duration(secs);
            Task::none()
        }

        Message::SelectTrack(idx) => {
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
            Task::none()
        }

        Message::DeleteTrack => {
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
            Task::none()
        }

        Message::TabSelected(tab) => {
            state.active_tab = tab;
            Task::none()
        }

        Message::SetAbrMode(variant) => {
            state.abr_mode_is_auto = variant.is_none();
            state.selected_variant = variant;
            state
                .queue
                .player()
                .set_abr_mode(variant.map_or(AbrMode::Auto(None), AbrMode::Manual));
            Task::none()
        }

        Message::Tick => {
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
            #[expect(
                clippy::cast_possible_truncation,
                reason = "f64 duration -> f32 is fine for UI"
            )]
            {
                state.duration = state.queue.duration_seconds().unwrap_or(0.0) as f32;
            }

            if !state.is_seeking {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "f64 position -> f32 is fine for UI"
                )]
                {
                    state.position = state.queue.position_seconds().unwrap_or(0.0) as f32;
                }
            }

            Task::none()
        }

        Message::ToggleShuffle => {
            let new = !state.queue.is_shuffle_enabled();
            state.queue.set_shuffle(new);
            state.shuffle_enabled = new;
            Task::none()
        }

        Message::ToggleRepeat => {
            state.repeat_enabled = !state.repeat_enabled;
            Task::none()
        }
    }
}
