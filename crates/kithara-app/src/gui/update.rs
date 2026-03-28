use iced::Task;
use tracing::error;

use super::{app::Kithara, message::Message};

/// Handle all messages (Elm `update` function).
#[expect(
    clippy::cognitive_complexity,
    reason = "single match over Message enum"
)]
pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    match message {
        Message::TogglePlayPause => {
            if state.playing {
                state.player.pause();
            } else {
                state.player.play();
            }
            state.playing = !state.playing;
            Task::none()
        }

        Message::Next => {
            if let Some(next_idx) = state.playlist.get_next_track() {
                state.current_track_index = Some(next_idx);
                state.track_name = state.playlist.track_name(next_idx);
                state.variant_label.clear();
                state.abr_mode_is_auto = true;
                state.selected_variant = None;
                state.load_track(next_idx)
            } else {
                Task::none()
            }
        }

        Message::Prev => {
            if let Some(prev_idx) = state.playlist.get_prev_track() {
                state.current_track_index = Some(prev_idx);
                state.track_name = state.playlist.track_name(prev_idx);
                state.variant_label.clear();
                state.load_track(prev_idx)
            } else {
                Task::none()
            }
        }

        Message::SeekChanged(pos) => {
            state.is_seeking = true;
            state.seek_position = pos;
            Task::none()
        }

        Message::SeekReleased => {
            state.is_seeking = false;
            if let Err(e) = state.player.seek_seconds(f64::from(state.seek_position)) {
                error!("seek failed: {e:?}");
            }
            Task::none()
        }

        Message::VolumeChanged(vol) => {
            state.volume = vol;
            state.player.set_volume(vol);
            Task::none()
        }

        Message::EqBandChanged(band, db) => {
            if band < state.eq_bands.len() {
                state.eq_bands[band] = db;
                match state.player.set_eq_gain(band, db) {
                    Ok(()) => tracing::info!("EQ band={band} gain={db:.1} dB — OK"),
                    Err(e) => error!("set EQ gain band={band} db={db:.1} failed: {e:?}"),
                }
            }
            Task::none()
        }

        Message::EqBandReset(band) => {
            if band < state.eq_bands.len() {
                state.eq_bands[band] = 0.0;
                if let Err(e) = state.player.set_eq_gain(band, 0.0) {
                    error!("reset EQ band={band} failed: {e:?}");
                }
            }
            Task::none()
        }

        Message::PlayRateChanged(rate) => {
            state.selected_rate = rate;
            state.player.set_default_rate(rate);
            if state.playing {
                state.player.play();
            }
            Task::none()
        }

        Message::CrossfadeChanged(secs) => {
            state.crossfade = secs;
            state.player.set_crossfade_duration(secs);
            Task::none()
        }

        Message::SelectTrack(idx) => {
            state.playlist.on_track_selected(idx);
            state.current_track_index = Some(idx);
            state.track_name = state.playlist.track_name(idx);
            state.variant_label.clear();
            state.abr_mode_is_auto = true;
            state.selected_variant = None;
            state.load_track(idx)
        }

        Message::TabSelected(tab) => {
            state.active_tab = tab;
            Task::none()
        }

        Message::TrackLoaded(index, result) => {
            // Status already updated by TrackLoadParams::load_and_apply().
            if let Err(ref e) = result {
                error!("track load failed: {e}");
            }
            // Auto-select first loaded track if none is playing.
            if result.is_ok() && state.current_track_index == Some(index) && !state.playing {
                state.playing = true;
            }
            Task::none()
        }

        Message::SetAbrMode(variant) => {
            use kithara::abr::AbrMode;
            state.abr_mode_is_auto = variant.is_none();
            state.selected_variant = variant;
            state
                .player
                .set_abr_mode(variant.map_or(AbrMode::Auto(None), AbrMode::Manual));
            Task::none()
        }

        Message::Tick => {
            let _ = state.player.tick();
            state.blink_counter = state.blink_counter.wrapping_add(1);

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
            state.playing = state.player.is_playing();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "f64 duration -> f32 is fine for UI"
            )]
            {
                state.duration = state.player.duration_seconds().unwrap_or(0.0) as f32;
            }

            if !state.is_seeking {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "f64 position -> f32 is fine for UI"
                )]
                {
                    state.position = state.player.position_seconds().unwrap_or(0.0) as f32;
                }
            }

            Task::none()
        }

        Message::ToggleShuffle => {
            state.shuffle_enabled = state.playlist.toggle_shuffle();
            Task::none()
        }

        Message::ToggleRepeat => {
            state.repeat_enabled = !state.repeat_enabled;
            Task::none()
        }
    }
}
