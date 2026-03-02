use std::sync::Arc;

use kithara::prelude::{PlayerImpl, Resource, ResourceConfig};
use slint::ComponentHandle;
use tracing::error;

use crate::{AppWindow, playlist::Playlist};

/// Register all UI callbacks that map Slint events to kithara-play commands.
pub(crate) fn register_callbacks(
    app: &AppWindow,
    player: &Arc<PlayerImpl>,
    playlist: &Arc<Playlist>,
    autoplay: bool,
) {
    // Load first track
    if !playlist.is_empty() {
        load_track(player, playlist, 0, autoplay);
        playlist.on_track_selected(0);
        app.set_current_track(0);
    }

    register_playback(app, player);
    register_navigation(app, player, playlist);
    register_seek(app, player);
    register_volume(app, player);
    register_eq(app, player);
    register_crossfade(app, player);
}

fn register_playback(app: &AppWindow, player: &Arc<PlayerImpl>) {
    let player_play = Arc::clone(player);
    let app_weak = app.as_weak();
    app.on_play(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if app.get_playing() {
            player_play.pause();
            app.set_playing(false);
        } else {
            player_play.play();
            app.set_playing(true);
        }
    });
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn register_navigation(app: &AppWindow, player: &Arc<PlayerImpl>, playlist: &Arc<Playlist>) {
    // PREV
    let player_prev = Arc::clone(player);
    let playlist_prev = Arc::clone(playlist);
    let app_prev = app.as_weak();
    app.on_prev(move || {
        let Some(app) = app_prev.upgrade() else {
            return;
        };
        let is_playing = app.get_playing();
        if let Some(prev_idx) = playlist_prev.get_prev_track() {
            load_track(&player_prev, &playlist_prev, prev_idx, is_playing);
            app.set_current_track(prev_idx as i32);
        }
    });

    // NEXT
    let player_next = Arc::clone(player);
    let playlist_next = Arc::clone(playlist);
    let app_next = app.as_weak();
    app.on_next(move || {
        let Some(app) = app_next.upgrade() else {
            return;
        };
        let is_playing = app.get_playing();
        if let Some(next_idx) = playlist_next.get_next_track() {
            load_track(&player_next, &playlist_next, next_idx, is_playing);
            app.set_current_track(next_idx as i32);
        }
    });

    // SELECT TRACK
    let player_select = Arc::clone(player);
    let playlist_select = Arc::clone(playlist);
    let app_select = app.as_weak();
    app.on_select_track(move |idx| {
        let Some(app) = app_select.upgrade() else {
            return;
        };
        let track_idx = idx as usize;
        let is_playing = app.get_playing();
        playlist_select.on_track_selected(track_idx);
        load_track(&player_select, &playlist_select, track_idx, is_playing);
        app.set_current_track(idx);
    });

    // SHUFFLE
    let playlist_shuffle = Arc::clone(playlist);
    let app_shuffle = app.as_weak();
    app.on_toggle_shuffle(move || {
        let Some(app) = app_shuffle.upgrade() else {
            return;
        };
        let enabled = playlist_shuffle.toggle_shuffle();
        app.set_shuffle_active(enabled);
    });

    // REPEAT (stub)
    app.on_toggle_repeat(move || {
        // TODO: implement repeat logic
    });
}

fn register_seek(app: &AppWindow, player: &Arc<PlayerImpl>) {
    let player_seek = Arc::clone(player);
    let app_seek = app.as_weak();
    let seek_timer = slint::Timer::default();

    app.on_seek(move |pos| {
        let Some(app) = app_seek.upgrade() else {
            return;
        };
        app.set_is_user_seeking(true);

        if let Err(e) = player_seek.seek_seconds(f64::from(pos)) {
            error!("seek failed: {e:?}");
        }

        let app_timer = app.as_weak();
        seek_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(300),
            move || {
                if let Some(app) = app_timer.upgrade() {
                    app.set_is_user_seeking(false);
                }
            },
        );
    });
}

fn register_volume(app: &AppWindow, player: &Arc<PlayerImpl>) {
    let player_vol = Arc::clone(player);
    app.on_update_volume(move |volume| {
        player_vol.set_volume(volume);
    });
}

#[expect(clippy::cast_sign_loss)]
fn register_eq(app: &AppWindow, player: &Arc<PlayerImpl>) {
    let player_eq = Arc::clone(player);
    app.on_eq_band_changed(move |index, value| {
        if let Err(e) = player_eq.set_eq_gain(index as usize, value) {
            error!("set EQ gain band={index} failed: {e:?}");
        }
    });
}

fn register_crossfade(app: &AppWindow, player: &Arc<PlayerImpl>) {
    let player_xf = Arc::clone(player);
    app.on_update_crossfade(move |value| {
        player_xf.set_crossfade_duration(value);
    });
}

/// Load a track by index into the player.
fn load_track(player: &Arc<PlayerImpl>, playlist: &Playlist, index: usize, autoplay: bool) {
    let Some(path) = playlist.track_path(index) else {
        return;
    };

    let config = match ResourceConfig::new(path) {
        Ok(c) => c,
        Err(e) => {
            error!("invalid resource URL {path}: {e:?}");
            return;
        }
    };

    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        error!("no tokio runtime available for resource loading");
        return;
    };

    let player = Arc::clone(player);
    let path = path.to_string();
    handle.spawn(async move {
        match Resource::new(config).await {
            Ok(resource) => {
                if index < player.item_count() {
                    player.replace_item(index, resource);
                } else {
                    player.insert(resource, None);
                }
                if let Err(e) = player.select_item(index, autoplay) {
                    error!("select_item({index}) failed: {e:?}");
                }
            }
            Err(e) => {
                error!("failed to load resource {path}: {e:?}");
            }
        }
    });
}
