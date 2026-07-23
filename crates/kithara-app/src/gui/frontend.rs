use iced::{Size, window::Settings};
use kithara_platform::{
    sync::{Arc, Mutex},
    tokio,
};

use kithara_ui::render::fonts;

use super::{
    app::{Decks, Kithara},
    update, view,
};
use crate::{
    catalog::Catalog,
    config::AppConfig,
    deck::{DeckId, DeckSet},
    frontend::{Frontend, FrontendError},
    state::StateController,
    theme::gui,
};

mod consts {
    /// DJ Studio window size in logical pixels. The minimum keeps both deck
    /// panes wide enough for their fixed transport and timestretch controls.
    pub(super) const STUDIO_WIDTH: f32 = 1280.0;
    pub(super) const STUDIO_HEIGHT: f32 = 760.0;
    pub(super) const STUDIO_MIN_WIDTH: f32 = 1080.0;
    pub(super) const STUDIO_MIN_HEIGHT: f32 = 640.0;
}
use consts::*;

/// Settings for the studio window. Close goes through `close_requests()`,
/// whose handler exits the app.
pub(crate) fn window_settings() -> Settings {
    Settings {
        size: Size::new(STUDIO_WIDTH, STUDIO_HEIGHT),
        min_size: Some(Size::new(STUDIO_MIN_WIDTH, STUDIO_MIN_HEIGHT)),
        exit_on_close_request: false,
        ..Settings::default()
    }
}

/// GUI frontend using iced.
pub struct GuiFrontend {
    config: AppConfig,
    palette: gui::GuiPalette,
}

impl Frontend for GuiFrontend {
    fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        Ok(Self {
            palette: config.palette.into(),
            config: config.clone(),
        })
    }

    fn run_loop(&mut self, decks: DeckSet) -> Result<(), FrontendError> {
        let palette = self.palette;
        let config = self.config.clone();

        let rt = tokio::runtime::Runtime::new()
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;
        let _guard = rt.enter();

        // The CLI tracks start on the first deck; every deck gets its own
        // controller, listener and analysis worker.
        if let Some(first) = decks.decks().first() {
            first
                .queue
                .set_tracks(crate::sources::build_sources(&config));
        }
        let controllers: Vec<(DeckId, Arc<StateController>)> = decks
            .decks()
            .iter()
            .map(|deck| {
                let controller = Arc::new(StateController::new(
                    Arc::clone(&deck.queue),
                    Arc::clone(&deck.timestretch),
                    config.clone(),
                    config.shutdown.child(),
                ));
                (deck.id, controller)
            })
            .collect();

        let boot = Mutex::new(Some((
            decks,
            Decks::new(controllers).ok_or("no decks to render")?,
            Catalog::new(config.tracks.clone()),
            config.clone(),
        )));

        let daemon = iced::daemon(
            move || {
                let (session, decks, catalog, config) = boot
                    .lock()
                    .take()
                    .expect("iced boots the application exactly once");
                Kithara::new(session, decks, catalog, config, palette)
            },
            update::update,
            view::view,
        )
        .title(Kithara::title)
        .theme(Kithara::theme)
        .subscription(Kithara::subscription)
        .default_font(fonts::SANS);
        let result = fonts::FONT_BYTES
            .iter()
            .fold(daemon, |daemon, bytes| daemon.font(*bytes))
            .run();

        config.shutdown.cancel();
        result?;

        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        Ok(())
    }

    fn start(&mut self, _decks: &DeckSet) -> Result<(), FrontendError> {
        Ok(())
    }
}
