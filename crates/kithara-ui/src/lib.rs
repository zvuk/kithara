mod icons;
mod message;
mod playlist;
mod state;
mod theme;
mod update;
mod view;

use std::sync::Arc;

use iced::Size;
use kithara::prelude::PlayerImpl;

/// Run the iced GUI application.
///
/// Creates the main application window with a dark + gold theme,
/// sets up state from the player, and runs the iced event loop.
///
/// This function blocks until the window is closed.
/// iced owns the tokio runtime — do **not** call from within `rt.block_on()`.
///
/// # Errors
/// Returns an error if iced fails to initialize or run.
pub fn run(player: &Arc<PlayerImpl>, track_paths: Vec<String>, autoplay: bool) -> iced::Result {
    tracing::info!(tracks = track_paths.len(), autoplay, "starting Kithara GUI");

    let player = player.clone();
    iced::application(
        move || state::Kithara::new(player.clone(), track_paths.clone(), autoplay),
        update::update,
        view::view,
    )
    .title("Kithara")
    .theme(state::Kithara::theme)
    .subscription(state::Kithara::subscription)
    .window_size(Size::new(448.0, 734.0))
    .run()
}
