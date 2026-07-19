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
    config::{AppConfig, WindowSizing},
    frontend::{Frontend, FrontendError},
    theme::gui,
};

const ERROR_FALLBACK: SizeSpec = SizeSpec::new(Dim::Fixed(360.0), Dim::Fixed(240.0));

/// Derives main-window settings from the compiled body and app window policy.
pub(crate) fn window_settings(compiled: Option<&CompiledUi>, sizing: &WindowSizing) -> Settings {
    let size = compiled.map_or(ERROR_FALLBACK, |ui| ui.size);
    let min_size = Size::new(
        (size.w.min() + sizing.chrome_w).max(sizing.min_floor_w),
        (size.h.min() + sizing.chrome_h).max(sizing.min_floor_h),
    );
    let initial_size = Size::new(
        min_size.width * sizing.initial_scale,
        min_size.height * sizing.initial_scale,
    );
    Settings {
        size: initial_size,
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

    fn run_loop(
        &mut self,
        queue: Arc<Queue>,
        timestretch: Arc<StretchControls>,
    ) -> Result<(), FrontendError> {
        let palette = self.palette;
        let config = self.config.clone();
        let window_sizing = config.window_sizing;

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
            move || Kithara::new(Arc::clone(&controller), palette, window_sizing),
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{builtin, compile::compile, source::UiConfig};

    use super::window_settings;
    use crate::{config::WindowSizing, gui::modular::endpoints};

    #[kithara::test]
    fn player_window_settings_include_chrome_floor_and_initial_scale() {
        let ui = compile(
            builtin::PLAYER_PRESET,
            &builtin::resolver(),
            &endpoints::catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("player preset must compile: {error}"));
        let sizing = WindowSizing::default();

        let settings = window_settings(Some(&ui), &sizing);
        let min = settings
            .min_size
            .unwrap_or_else(|| panic!("main window must have a minimum size"));

        assert!(min.width >= ui.size.w.min() + sizing.chrome_w);
        assert!(min.height >= ui.size.h.min() + sizing.chrome_h);
        assert!(min.width >= sizing.min_floor_w);
        assert!(min.height >= sizing.min_floor_h);
        assert!(settings.size.width > min.width);
        assert!(settings.size.height > min.height);
    }
}
