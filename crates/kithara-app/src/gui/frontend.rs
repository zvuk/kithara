use std::error::Error;

use iced::{Size, window::Settings};
use kithara_platform::{
    sync::{Arc, Mutex},
    tokio,
};
use kithara_ui::render::fonts;
use num_traits::cast::AsPrimitive;

use super::{
    app::{Decks, Kithara},
    studio_ui::StudioUi,
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

fn immediate(boot: Boot, palette: gui::GuiPalette) -> Result<(), FrontendError> {
    let boot = Mutex::new(Some(boot));
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
    fonts::FONT_BYTES
        .iter()
        .fold(daemon, |daemon, bytes| daemon.font(*bytes))
        .run()?;
    Ok(())
}

#[cfg(feature = "masonry")]
fn retained(boot: Boot, palette: gui::GuiPalette) -> Result<(), FrontendError> {
    super::retained::run(super::retained::Studio::new(
        boot.session,
        boot.decks,
        boot.catalog,
        boot.config,
        boot.studio,
        palette,
    ))?;
    Ok(())
}

mod consts {
    /// DJ Studio window size in whole logical points. The minimum keeps both
    /// deck panes wide enough for their fixed transport and timestretch
    /// controls; the studio never gets shorter than it opens.
    pub(super) const STUDIO_WIDTH: u32 = 1280;
    pub(super) const STUDIO_HEIGHT: u32 = 760;
    pub(super) const STUDIO_MIN_WIDTH: u32 = 1080;
}
use consts::*;

/// The studio window and the smallest box the document is laid out for, in
/// logical points. Both hosts open the same window.
pub(crate) const fn studio_size() -> ((u32, u32), (u32, u32)) {
    (
        (STUDIO_WIDTH, STUDIO_HEIGHT),
        (STUDIO_MIN_WIDTH, STUDIO_HEIGHT),
    )
}

/// Settings for the studio window. The bar draws the window chrome itself, so
/// the system decorations stay off; close goes through `close_requests()`,
/// whose handler exits the app.
pub(crate) fn window_settings() -> Settings {
    let (size, min_size) = studio_size();
    let logical = |(width, height): (u32, u32)| Size::new(width.as_(), height.as_());
    Settings {
        size: logical(size),
        min_size: Some(logical(min_size)),
        decorations: false,
        exit_on_close_request: false,
        ..Settings::default()
    }
}

struct Boot {
    config: AppConfig,
    catalog: Catalog,
    session: DeckSet,
    decks: Decks,
    studio: StudioUi,
}

/// GUI frontend for the studio.
pub struct GuiFrontend {
    config: AppConfig,
    host: Host,
    palette: gui::GuiPalette,
}

impl GuiFrontend {
    /// Creates the GUI frontend from application configuration.
    ///
    /// # Errors
    /// Returns an error if GUI initialization fails.
    pub fn new(config: &AppConfig, host: Host) -> Result<Self, FrontendError> {
        Ok(Self {
            host,
            palette: config.palette.into(),
            config: config.clone(),
        })
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
        let studio = StudioUi::new()?;

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
            session,
            studio,
            decks: Decks::new(controllers).ok_or("no decks to render")?,
            catalog: Catalog::new(config.tracks.clone()),
            config: config.clone(),
        };
        let result = match self.host {
            Host::Immediate => immediate(boot, palette),
            #[cfg(feature = "masonry")]
            Host::Retained => retained(boot, palette),
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
