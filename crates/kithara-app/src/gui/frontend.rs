use std::error::Error;

use iced::{Size, window::Settings};
use kithara_platform::{
    sync::{Arc, Mutex},
    tokio,
};
use kithara_ui::render::fonts;
#[cfg(feature = "masonry")]
use num_traits::cast::AsPrimitive;

use super::{
    app::{Decks, Kithara},
    ui::AppUi,
    update, view,
};
use crate::{
    catalog::Catalog,
    config::AppConfig,
    deck::{DeckId, DeckSet},
    state::StateController,
};

/// Error returned by the GUI frontend.
pub type FrontendError = Box<dyn Error + Send + Sync>;

/// Which host draws the studio.
///
/// The retained host is a build the `masonry` feature turns on; without it
/// there is only one host to pick. Both read the same documents and the same
/// state, so the choice is the shell and nothing else.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum Host {
    /// iced: the tree is rebuilt from the state on every message.
    #[cfg_attr(not(feature = "masonry"), default)]
    Immediate,
    /// masonry and Vello: the tree is kept and told what changed.
    #[cfg(feature = "masonry")]
    #[default]
    Retained,
}

fn immediate(boot: Boot) -> Result<(), FrontendError> {
    let boot = Mutex::new(Some(boot));
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
    fonts::FONT_BYTES
        .iter()
        .fold(daemon, |daemon, bytes| daemon.font(*bytes))
        .run()?;
    Ok(())
}

#[cfg(feature = "masonry")]
fn retained(boot: Boot) -> Result<(), FrontendError> {
    super::retained::run(super::retained::Studio::new(
        boot.session,
        boot.decks,
        boot.catalog,
        boot.config,
        boot.ui,
        boot.broadcast,
    ))?;
    Ok(())
}

mod consts {
    /// The minimum keeps both deck panes wide enough for their fixed
    /// transport and timestretch controls.
    pub(super) const WINDOW_MIN_WIDTH: f32 = 1080.0;
    pub(super) const WINDOW_MIN_HEIGHT: f32 = super::WINDOW_SIZE.height;
}
use consts::*;

use super::ui::window::WINDOW_SIZE;

/// The app window and the smallest box the document is laid out for, in whole
/// logical points. Both hosts open the same window.
#[cfg(feature = "masonry")]
pub(crate) fn window_size() -> ((u32, u32), (u32, u32)) {
    let whole = |value: f32| -> u32 { value.as_() };
    (
        (whole(WINDOW_SIZE.width), whole(WINDOW_SIZE.height)),
        (whole(WINDOW_MIN_WIDTH), whole(WINDOW_MIN_HEIGHT)),
    )
}

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

/// GUI frontend for the studio.
pub struct GuiFrontend {
    broadcast: Option<crate::broadcast::Broadcaster>,
    config: AppConfig,
    host: Host,
}

impl GuiFrontend {
    /// Creates the GUI frontend from application configuration.
    ///
    /// # Errors
    /// Returns an error if GUI initialization fails.
    pub fn new(config: &AppConfig, host: Host) -> Result<Self, FrontendError> {
        Ok(Self {
            broadcast: None,
            host,
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

        let boot = Boot {
            broadcast: self
                .broadcast
                .take()
                .ok_or("broadcast service was not configured")?,
            session,
            ui,
            decks: Decks::new(controllers).ok_or("no decks to render")?,
            catalog: Catalog::new(config.tracks.clone()),
            config: config.clone(),
        };
        let result = match self.host {
            Host::Immediate => immediate(boot),
            #[cfg(feature = "masonry")]
            Host::Retained => retained(boot),
        };

        config.shutdown.cancel();
        result
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
