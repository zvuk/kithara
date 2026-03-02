use std::{sync::Arc, time::Duration};

use kithara::prelude::PlayerImpl;
use slint::ComponentHandle;

use crate::AppWindow;

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Start a periodic timer that polls the player state and updates the UI.
///
/// kithara uses pull-based broadcast receivers for events. Instead of push
/// listeners, we poll the player state every 100ms from the Slint event loop
/// and synchronize UI properties accordingly.
#[expect(clippy::cast_possible_truncation)]
pub(crate) fn start_polling(app: &AppWindow, player: &Arc<PlayerImpl>) {
    let player = Arc::clone(player);
    let app_weak = app.as_weak();

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, POLL_INTERVAL, move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };

        // Don't update position while user is dragging the seek slider
        if !app.get_is_user_seeking() {
            if let Some(pos) = player.position_seconds()
                && pos.is_finite()
                && pos >= 0.0
            {
                app.set_position(pos as f32);
            }
            if let Some(dur) = player.duration_seconds()
                && dur.is_finite()
                && dur > 0.0
            {
                app.set_duration(dur as f32);
            }
        }

        app.set_volume(player.volume());
        app.set_playing(player.is_playing());
    });

    // Keep the timer alive by storing it — Slint will manage its lifecycle
    // via the event loop. We use a leaked Box to keep it alive for the app's
    // lifetime (the app owns the event loop, so this is fine).
    std::mem::forget(timer);
}
