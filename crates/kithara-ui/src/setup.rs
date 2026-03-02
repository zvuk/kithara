use std::sync::Arc;

use kithara::prelude::PlayerImpl;
use slint::{ModelRc, VecModel};

use crate::{AppWindow, EqBandModel, PlaylistTrackModel, playlist::Playlist};

/// Sets up the initial state of the application window from the player.
pub(crate) fn setup_app(app: &AppWindow, player: &Arc<PlayerImpl>, playlist: &Arc<Playlist>) {
    // Build playlist model for UI
    let tracks: Vec<PlaylistTrackModel> = (0..playlist.len())
        .map(|i| {
            let name = playlist.track_name(i);
            let path = playlist.track_path(i).unwrap_or("").to_string();
            PlaylistTrackModel {
                path: path.into(),
                artist: slint::SharedString::default(),
                title: name.into(),
                state: crate::PlaylistState::Idle,
                duration: 0.0,
            }
        })
        .collect();
    app.set_playlist(ModelRc::new(VecModel::from(tracks)));

    // Initialize UI from player state
    app.set_volume(player.volume());
    app.set_crossfade(player.crossfade_duration());

    #[expect(clippy::cast_possible_truncation)]
    if let Some(pos) = player.position_seconds() {
        app.set_position(pos as f32);
    }
    #[expect(clippy::cast_possible_truncation)]
    if let Some(dur) = player.duration_seconds() {
        app.set_duration(dur as f32);
    }

    // EQ bands
    let band_count = player.eq_band_count();
    let eq_labels = ["Low", "Mid", "High"];
    let eq_freqs = [200.0_f32, 1000.0, 5000.0];
    let ui_bands: Vec<EqBandModel> = (0..band_count)
        .map(|i| {
            let gain = player.eq_gain(i).unwrap_or(0.0);
            let freq = eq_freqs.get(i).copied().unwrap_or(0.0);
            let _ = eq_labels.get(i).unwrap_or(&"?");
            EqBandModel { gain, freq }
        })
        .collect();
    app.set_eq_bands(ModelRc::new(VecModel::from(ui_bands)));
}
