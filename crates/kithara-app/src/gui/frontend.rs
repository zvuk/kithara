use std::{error::Error, num::NonZeroUsize};

use iced::{Size, window::Settings};
use kithara::{
    platform::{
        sync::{Arc, Mutex},
        time::Duration,
    },
    ui::render::fonts,
    worker::{DispatcherConfig, TaskConfig},
};
#[cfg(feature = "masonry")]
use num_traits::cast::AsPrimitive;

use super::{
    app::{Decks, Kithara},
    ui::{AppUi, package::Package, window::WINDOW_SIZE},
    update, view,
};
use crate::{
    catalog::Catalog,
    config::AppConfig,
    deck::{DeckId, DeckSet},
    state::StateController,
    wave_cache::{AnalysisPersistence, persistence::AnalysisPersistenceConfig},
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
    .style(Kithara::style)
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

/// The box the app window opens at, in whole logical points. Both hosts open
/// the same window.
#[cfg(feature = "masonry")]
pub(crate) fn window_size() -> (u32, u32) {
    (whole(WINDOW_SIZE.width), whole(WINDOW_SIZE.height))
}

/// The smallest box the compiled documents are laid out for, in whole logical
/// points.
#[cfg(feature = "masonry")]
pub(crate) fn window_min(min: Size) -> (u32, u32) {
    (whole(min.width), whole(min.height))
}

#[cfg(feature = "masonry")]
fn whole(value: f32) -> u32 {
    value.as_()
}

/// Settings for the app window. The bar draws the window chrome itself, so
/// the system decorations stay off; close goes through `close_requests()`,
/// whose handler exits the app.
pub(crate) fn window_settings(min: Size) -> Settings {
    Settings {
        size: WINDOW_SIZE,
        min_size: Some(min),
        decorations: false,
        exit_on_close_request: false,
        transparent: true,
        ..Settings::default()
    }
}

struct Boot {
    config: AppConfig,
    ui: AppUi,
    broadcast: crate::broadcast::Broadcaster,
    catalog: Catalog,
    session: DeckSet,
    decks: Decks,
}

/// GUI frontend for the studio.
pub struct GuiFrontend {
    config: AppConfig,
    host: Host,
    broadcast: Option<crate::broadcast::Broadcaster>,
}

impl GuiFrontend {
    /// Creates the GUI frontend from application configuration.
    ///
    /// # Errors
    /// Returns an error if GUI initialization fails.
    pub fn new(config: &AppConfig, host: Host) -> Result<Self, FrontendError> {
        Ok(Self {
            host,
            broadcast: None,
            config: config.clone(),
        })
    }

    /// Gives the bar's REC cell a session to put on air.
    pub fn attach_broadcast(&mut self, shutdown: kithara::platform::CancelToken) {
        self.broadcast = Some(crate::broadcast::Broadcaster::new(
            shutdown,
            self.config.broadcast_tap_lead,
        ));
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
        let ui = AppUi::new(Package::load(config.ui_package.as_deref())?)?;
        let base_worker = config
            .base_worker
            .clone()
            .ok_or("GUI analysis persistence requires the app base worker")?;
        let persistence = AnalysisPersistence::new(AnalysisPersistenceConfig::new(
            base_worker,
            config.worker.pools().clone(),
            NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            Duration::from_secs(u64::from(config.analysis_chunk_seconds.get())),
            DispatcherConfig::builder()
                .name("kithara-analysis-persistence")
                .build(),
            TaskConfig::new(),
        ))?;

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
                    deck.queue.control().clone(),
                    Arc::clone(&deck.timestretch),
                    config.clone(),
                    deck.cancel_child(),
                    persistence.clone(),
                ));
                (deck.id, controller)
            })
            .collect();

        let boot = Boot {
            session,
            ui,
            broadcast: self
                .broadcast
                .take()
                .ok_or("broadcast service was not configured")?,
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
