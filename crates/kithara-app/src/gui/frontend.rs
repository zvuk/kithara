use std::sync::Arc;

use iced::{Size, window};
use kithara_queue::Queue;

use super::{app::Kithara, fonts, update, view};
use crate::{
    config::AppConfig,
    frontend::{Frontend, FrontendError},
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
    pub(super) const STUDIO_HEIGHT: f32 = 600.0;
    pub(super) const STUDIO_MIN_WIDTH: f32 = 820.0;
    pub(super) const STUDIO_MIN_HEIGHT: f32 = 520.0;
}
use consts::*;

/// Window settings per mode. A mode swap opens a fresh window rather than
/// resizing the live one. Close is handled via `close_requests()`, so the
/// programmatic swap-close does not exit the app.
pub(crate) fn window_settings(dj: bool) -> window::Settings {
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
    window::Settings {
        size,
        min_size: Some(min_size),
        exit_on_close_request: false,
        ..window::Settings::default()
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

    fn run_loop(&mut self, queue: Arc<Queue>) -> Result<(), FrontendError> {
        let palette = self.palette;
        let config = self.config.clone();

        let rt = tokio::runtime::Runtime::new()
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;
        let _guard = rt.enter();

        queue.set_tracks(crate::sources::build_sources(&config));
        let controller = Arc::new(crate::state::StateController::new(
            Arc::clone(&queue),
            config.clone(),
            config.shutdown.child(),
        ));

        let result = iced::daemon(
            move || Kithara::new(Arc::clone(&controller), palette),
            update::update,
            view::view,
        )
        .title(Kithara::title)
        .theme(Kithara::theme)
        .subscription(Kithara::subscription)
        .default_font(fonts::SANS)
        .font(fonts::INTER_BYTES)
        .font(fonts::SPACE_GROTESK_BYTES)
        .font(fonts::JETBRAINS_MONO_REGULAR_BYTES)
        .font(fonts::JETBRAINS_MONO_MEDIUM_BYTES)
        .font(fonts::JETBRAINS_MONO_SEMIBOLD_BYTES)
        .run();

        config.shutdown.cancel();
        result?;

        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        Ok(())
    }

    fn start(&mut self, _queue: Arc<Queue>) -> Result<(), FrontendError> {
        Ok(())
    }
}
