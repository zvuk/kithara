use iced::{Size, window::Settings};
use kithara_platform::{
    sync::{Arc, Mutex},
    tokio,
};
use kithara_ui::render::fonts;

use super::{
    app::{Decks, Kithara},
    studio_ui::StudioUi,
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
    pub(super) const STUDIO_MIN_HEIGHT: f32 = STUDIO_HEIGHT;
}
use consts::*;

/// Settings for the studio window. The bar draws the window chrome itself, so
/// the system decorations stay off; close goes through `close_requests()`,
/// whose handler exits the app.
pub(crate) fn window_settings() -> Settings {
    Settings {
        size: Size::new(STUDIO_WIDTH, STUDIO_HEIGHT),
        min_size: Some(Size::new(STUDIO_MIN_WIDTH, STUDIO_MIN_HEIGHT)),
        decorations: false,
        exit_on_close_request: false,
        ..Settings::default()
    }
}

struct Boot {
    session: DeckSet,
    decks: Decks,
    catalog: Catalog,
    config: AppConfig,
    studio: StudioUi,
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

    fn run_loop(&mut self, session: DeckSet) -> Result<(), FrontendError> {
        let palette = self.palette;
        let config = self.config.clone();
        let studio = StudioUi::new()?;

        let rt = tokio::runtime::Runtime::new()
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;
        let _guard = rt.enter();

        // The CLI tracks start on the first deck; every deck gets its own
        // controller, listener and analysis worker.
        if let Some(first) = session.decks().first() {
            first
                .queue
                .set_tracks(crate::sources::build_sources(&config));
        }
        let controllers: Vec<(DeckId, Arc<StateController>)> = session
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

        let boot = Mutex::new(Some(Boot {
            session,
            decks: Decks::new(controllers).ok_or("no decks to render")?,
            catalog: Catalog::new(config.tracks.clone()),
            config: config.clone(),
            studio,
        }));

        let daemon = iced::daemon(
            move || {
                let boot = boot
                    .lock()
                    .take()
                    .expect("iced boots the application exactly once");
                Kithara::new(
                    boot.session,
                    boot.decks,
                    boot.catalog,
                    boot.config,
                    boot.studio,
                    palette,
                )
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
