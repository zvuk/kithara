use iced::{Size, window::Settings};
use kithara_platform::{
    sync::{Arc, Mutex},
    tokio,
};

use super::{
    app::{Decks, Kithara},
    fonts, update, view,
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
    /// Compact-player window size in logical pixels.
    pub(super) const COMPACT_WIDTH: f32 = 448.0;
    pub(super) const COMPACT_HEIGHT: f32 = 784.0;
    pub(super) const COMPACT_MIN_WIDTH: f32 = 420.0;
    pub(super) const COMPACT_MIN_HEIGHT: f32 = 760.0;

    /// DJ Studio window size in logical pixels.
    pub(super) const STUDIO_WIDTH: f32 = 980.0;
    pub(super) const STUDIO_HEIGHT: f32 = 700.0;
    pub(super) const STUDIO_MIN_WIDTH: f32 = 820.0;
    pub(super) const STUDIO_MIN_HEIGHT: f32 = 620.0;
}
use consts::*;

/// Window settings per mode. A mode swap opens a fresh window rather than
/// resizing the live one. Close is handled via `close_requests()`, so the
/// programmatic swap-close does not exit the app.
pub(crate) fn window_settings(dj: bool) -> Settings {
    let (size, min_size) = if dj {
        (
            Size::new(STUDIO_WIDTH, STUDIO_HEIGHT),
            Size::new(STUDIO_MIN_WIDTH, STUDIO_MIN_HEIGHT),
        )
    } else {
        (
            Size::new(COMPACT_WIDTH, COMPACT_HEIGHT),
            Size::new(COMPACT_MIN_WIDTH, COMPACT_MIN_HEIGHT),
        )
    };
    Settings {
        size,
        min_size: Some(min_size),
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

        let result = iced::daemon(
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
        .default_font(fonts::SANS)
        .font(fonts::INTER_REGULAR_BYTES)
        .font(fonts::INTER_SEMIBOLD_BYTES)
        .font(fonts::SPACE_GROTESK_MEDIUM_BYTES)
        .font(fonts::SPACE_GROTESK_BOLD_BYTES)
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
