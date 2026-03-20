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
                state.load_track(next_idx)
            } else {
                Task::none()
            }
        }

        Message::Prev => {
            if let Some(prev_idx) = state.playlist.get_prev_track() {
                state.current_track_index = Some(prev_idx);
                state.track_name = state.playlist.track_name(prev_idx);
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

        Message::CrossfadeChanged(secs) => {
            state.crossfade = secs;
            state.player.set_crossfade_duration(secs);
            Task::none()
        }

        Message::SelectTrack(idx) => {
            state.playlist.on_track_selected(idx);
            state.current_track_index = Some(idx);
            state.track_name = state.playlist.track_name(idx);
            state.load_track(idx)
        }

        Message::TabSelected(tab) => {
            state.active_tab = tab;
            Task::none()
        }

        Message::TrackLoaded(result) => {
            if let Err(ref e) = result {
                error!("track load failed: {e}");
            }
            Task::none()
        }

        Message::Tick => {
            let _ = state.player.tick();

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
