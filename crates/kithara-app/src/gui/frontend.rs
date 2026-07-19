use iced::{Size, window::Settings};
use kithara::play::StretchControls;
use kithara_platform::{sync::Arc, tokio};
use kithara_queue::Queue;
use kithara_ui::{
    compile::CompiledUi,
    size::{Dim, SizeSpec},
};

use super::{app::Kithara, fonts, update, view};
use crate::{
    config::AppConfig,
    frontend::{Frontend, FrontendError},
    theme::gui,
};

/// Degraded-mode window size used only when the preset failed to compile, so
/// the error window still has usable dimensions. Builtin presets are tested to
/// compile, so this is never hit in normal operation.
const ERROR_FALLBACK: SizeSpec = SizeSpec::new(Dim::Fixed(360.0), Dim::Fixed(240.0));

/// Window settings derived from the compiled UI's intrinsic size: the window's
/// minimum and initial size are the composed minimum of the root. A preset swap
/// opens a fresh window rather than resizing the live one; close is handled via
/// `close_requests()`, so the programmatic swap-close does not exit the app.
pub(crate) fn window_settings(compiled: Option<&CompiledUi>) -> Settings {
    let size = compiled.map_or(ERROR_FALLBACK, |ui| ui.size);
    let dims = Size::new(size.w.min(), size.h.min());
    Settings {
        size: dims,
        min_size: Some(dims),
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

    fn run_loop(
        &mut self,
        queue: Arc<Queue>,
        timestretch: Arc<StretchControls>,
    ) -> Result<(), FrontendError> {
        let palette = self.palette;
        let config = self.config.clone();

        let rt = tokio::runtime::Runtime::new()
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;
        let _guard = rt.enter();

        queue.set_tracks(crate::sources::build_sources(&config));
        let controller = Arc::new(crate::state::StateController::new(
            Arc::clone(&queue),
            timestretch,
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
