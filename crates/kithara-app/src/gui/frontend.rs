use std::error::Error;

use iced::{Size, window::Settings};
use kithara_platform::{
    sync::{Arc, Mutex},
    tokio,
};
use kithara_ui::render::fonts;

use super::{
    app::{Decks, Kithara},
    ui::{AppUi, window::WINDOW_SIZE},
    update, view,
};
use crate::{
    catalog::Catalog,
    config::AppConfig,
    deck::{DeckId, DeckSet},
    state::StateController,
    theme::gui,
};

/// Error returned by the GUI frontend.
pub type FrontendError = Box<dyn Error + Send + Sync>;

pub(in crate::gui) mod consts {
    /// The minimum is the box the micro bar declares: a smaller window
    /// overflows the cells that stand in it at every size.
    pub(in crate::gui) const WINDOW_MIN_WIDTH: f32 = 221.0;
    pub(in crate::gui) const WINDOW_MIN_HEIGHT: f32 = 42.0;
}
use consts::*;

/// Settings for the app window. The bar draws the window chrome itself, so
/// the system decorations stay off; close goes through `close_requests()`,
/// whose handler exits the app.
pub(crate) fn window_settings() -> Settings {
    Settings {
        size: WINDOW_SIZE,
        min_size: Some(Size::new(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)),
        decorations: false,
        exit_on_close_request: false,
        ..Settings::default()
    }
}

struct Boot {
    broadcast: crate::broadcast::Broadcaster,
    config: AppConfig,
    catalog: Catalog,
    session: DeckSet,
    decks: Decks,
    ui: AppUi,
}

/// GUI frontend using iced.
pub struct GuiFrontend {
    broadcast: Option<crate::broadcast::Broadcaster>,
    config: AppConfig,
    palette: gui::GuiPalette,
}

impl GuiFrontend {
    /// Creates the GUI frontend from application configuration.
    ///
    /// # Errors
    /// Returns an error if GUI initialization fails.
    pub fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        Ok(Self {
            broadcast: None,
            palette: config.palette.into(),
            config: config.clone(),
        })
    }

    /// Gives the bar's REC cell a session to put on air.
    pub fn attach_broadcast(
        &mut self,
        session: kithara::play::SessionHandle,
        shutdown: kithara_platform::CancelToken,
    ) {
        self.broadcast = Some(crate::broadcast::Broadcaster::new(session, shutdown));
    }

    /// Runs the GUI event loop until the application exits.
    ///
    /// # Errors
    /// Returns an error if the event loop fails.
    ///
    /// # Panics
    /// Panics if iced boots the application more than once; the boot
    /// state is handed over exactly once by construction.
    pub fn run_loop(&mut self, session: DeckSet) -> Result<(), FrontendError> {
        let palette = self.palette;
        let config = self.config.clone();
        let ui = AppUi::new()?;

        let rt = tokio::runtime::Runtime::new().map_err(FrontendError::from)?;
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
            broadcast: self
                .broadcast
                .take()
                .ok_or("broadcast service was not configured")?,
            session,
            ui,
            decks: Decks::new(controllers).ok_or("no decks to render")?,
            catalog: Catalog::new(config.tracks.clone()),
            config: config.clone(),
        }));

        let daemon = iced::daemon(
            move || {
                let boot = boot
                    .lock()
                    .take()
                    .expect("invariant: iced boots the application exactly once");
                Kithara::new(
                    boot.session,
                    boot.decks,
                    boot.catalog,
                    boot.config,
                    boot.ui,
                    palette,
                    boot.broadcast,
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

    /// Completes GUI shutdown.
    ///
    /// # Errors
    /// Returns an error if shutdown fails.
    pub fn shutdown(&mut self) -> Result<(), FrontendError> {
        Ok(())
    }

    /// Prepares the GUI frontend for the deck session.
    ///
    /// # Errors
    /// Returns an error if startup fails.
    pub fn start(&mut self, _decks: &DeckSet) -> Result<(), FrontendError> {
        Ok(())
    }
}
