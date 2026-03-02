// Slint macro-generated code triggers these workspace lints.
#![allow(
    unused_qualifications,
    unreachable_pub,
    dead_code,
    clippy::allow_attributes,
    clippy::impl_trait_in_params,
    clippy::unwrap_used
)]

mod callbacks;
mod listeners;
mod playlist;
mod setup;

use std::sync::Arc;

use kithara::prelude::PlayerImpl;
use slint::ComponentHandle;

slint::include_modules!();

/// Run the Slint GUI application.
///
/// Creates the main application window, sets up state from the player,
/// registers callbacks, and runs the Slint event loop.
///
/// This function blocks until the window is closed.
///
/// # Errors
/// Returns an error if the Slint platform fails to initialize or run.
pub fn run(
    player: &Arc<PlayerImpl>,
    track_paths: Vec<String>,
    autoplay: bool,
) -> Result<(), slint::PlatformError> {
    tracing::info!(tracks = track_paths.len(), autoplay, "starting Kithara GUI");

    let app = AppWindow::new()?;

    let playlist = Arc::new(playlist::Playlist::new(track_paths));

    setup::setup_app(&app, player, &playlist);
    callbacks::register_callbacks(&app, player, &playlist, autoplay);
    listeners::start_polling(&app, player);

    app.run()
}
