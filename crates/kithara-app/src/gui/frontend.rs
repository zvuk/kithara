use std::sync::Arc;

use iced::{Size, window};
use kithara_queue::Queue;

use super::{app::Kithara, update, view};
use crate::{
    config::AppConfig,
    frontend::{Frontend, FrontendError},
    theme::gui,
};

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
        /// Default window width in logical pixels.
        const WINDOW_WIDTH: f32 = 448.0;

        /// Default window height in logical pixels.
        const WINDOW_HEIGHT: f32 = 734.0;

        /// Minimum window width — keeps the EQ row at full ten-band
        /// density (10·28 + 9·2 = 298 px inside the panel) plus the
        /// 18 px outer padding × 2 and the 12 px panel padding × 2.
        const MIN_WIDTH: f32 = 420.0;

        /// Minimum window height. The fixed sections above the tabs
        /// (header + URL + now-playing + seek + transport + rate row +
        /// volume + tabs strip + outer paddings) eat ≈540 px on their
        /// own; the remainder must still leave the EQ row and a couple
        /// of playlist entries readable, so we anchor the floor at
        /// 720 px.
        const MIN_HEIGHT: f32 = 720.0;

        let palette = self.palette;
        let config = self.config.clone();

        let rt = tokio::runtime::Runtime::new()
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;
        let _guard = rt.enter();

        queue.set_tracks(crate::sources::build_sources(&config));
        let controller = Arc::new(crate::state::StateController::new(Arc::clone(&queue)));

        iced::application(
            move || Kithara::new(Arc::clone(&controller), palette),
            update::update,
            view::view,
        )
        .title("Kithara")
        .theme(Kithara::theme)
        .subscription(Kithara::subscription)
        .window(window::Settings {
            size: Size::new(WINDOW_WIDTH, WINDOW_HEIGHT),
            min_size: Some(Size::new(MIN_WIDTH, MIN_HEIGHT)),
            ..window::Settings::default()
        })
        .run()?;

        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        Ok(())
    }

    fn start(&mut self, _queue: Arc<Queue>) -> Result<(), FrontendError> {
        Ok(())
    }
}
