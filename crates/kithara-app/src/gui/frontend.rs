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
    let (min_w, initial_w) = axis_settings(
        size.w,
        sizing.chrome_w,
        sizing.min_floor_w,
        sizing.initial_scale,
    );
    let (min_h, initial_h) = axis_settings(
        size.h,
        sizing.chrome_h,
        sizing.min_floor_h,
        sizing.initial_scale,
    );
    let min_size = Size::new(min_w, min_h);
    let initial_size = Size::new(initial_w, initial_h);
    Settings {
        size: initial_size,
        min_size: Some(min_size),
        exit_on_close_request: false,
        ..Settings::default()
    }
}

fn axis_settings(dim: Dim, chrome: f32, floor: f32, initial_scale: f32) -> (f32, f32) {
    let tree_min = dim.min();
    let min = if tree_min == 0.0 {
        floor
    } else {
        tree_min + chrome
    };
    let initial = if dim.max().is_none() {
        min * initial_scale
    } else {
        min
    };
    (min, initial)
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
        .font(fonts::INTER_REGULAR_BYTES)
        .font(fonts::INTER_SEMIBOLD_BYTES)
        .font(fonts::SPACE_GROTESK_REGULAR_BYTES)
        .font(fonts::SPACE_GROTESK_MEDIUM_BYTES)
        .font(fonts::SPACE_GROTESK_SEMIBOLD_BYTES)
        .font(fonts::SPACE_GROTESK_BOLD_BYTES)
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
    use kithara_ui::{builtin, compile::compile, size::Dim, source::UiConfig};

    use super::{axis_settings, window_settings};
    use crate::{config::WindowSizing, gui::modular::endpoints};

    #[kithara::test]
    fn player_open_axes_scale_from_their_intrinsic_minimum() {
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

        assert_eq!(min.width, ui.size.w.min() + sizing.chrome_w);
        assert_eq!(min.height, ui.size.h.min() + sizing.chrome_h);
        assert!(ui.size.w.max().is_none());
        assert!(ui.size.h.max().is_none());
        assert_eq!(settings.size.width, min.width * sizing.initial_scale);
        assert_eq!(settings.size.height, min.height * sizing.initial_scale);
    }

    #[kithara::test]
    fn micro_bounded_height_opens_at_its_intrinsic_minimum() {
        let ui = compile(
            builtin::MICRO_PRESET,
            &builtin::resolver(),
            &endpoints::catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("micro preset must compile: {error}"));
        let sizing = WindowSizing::default();

        let settings = window_settings(Some(&ui), &sizing);
        let min = settings
            .min_size
            .unwrap_or_else(|| panic!("main window must have a minimum size"));

        assert!(ui.size.h.min() > 0.0);
        assert!(ui.size.h.max().is_some());
        assert!(ui.size.w.max().is_none());
        assert_eq!(min.height, ui.size.h.min() + sizing.chrome_h);
        assert!(min.height < sizing.min_floor_h);
        assert_eq!(settings.size.width, min.width * sizing.initial_scale);
        assert_eq!(settings.size.height, min.height);
    }

    #[kithara::test]
    fn fill_only_axis_uses_the_configured_floor_and_scale() {
        let sizing = WindowSizing::default();

        let (min, initial) = axis_settings(
            Dim::Fill,
            sizing.chrome_h,
            sizing.min_floor_h,
            sizing.initial_scale,
        );

        assert_eq!(min, sizing.min_floor_h);
        assert_eq!(initial, min * sizing.initial_scale);
    }
}
