use std::{error::Error, path::Path};

use arc_swap::ArcSwap;
#[cfg(target_arch = "wasm32")]
use iced::window::settings::PlatformSpecific;
use iced::{Size, window::Settings};
use kithara::{
    platform::{
        sync::{Arc, Mutex},
        tokio::sync::mpsc::UnboundedSender,
    },
    ui::{render::fonts, source::UiConfig},
};

use super::{
    app::Kithara,
    ui::{AppUi, package::Package, window::consts::WINDOW_SIZE},
    update, view,
};
use crate::{
    catalog::Catalog,
    engine::{EngineSnapshot, Envelope},
    theme::Palette,
};

/// Error returned by the GUI frontend.
pub type FrontendError = Box<dyn Error + Send + Sync>;

pub(crate) fn immediate(boot: Boot) -> Result<(), FrontendError> {
    let boot = Mutex::new(Some(boot));
    let daemon = iced::daemon(
        move || {
            let boot = boot
                .lock()
                .take()
                .expect("invariant: iced boots the application exactly once");
            Kithara::new(boot)
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
pub(crate) fn retained(boot: Boot) -> Result<(), FrontendError> {
    super::retained::run(super::retained::Studio::new(boot))?;
    Ok(())
}

pub(crate) fn window_settings(min: Size) -> Settings {
    Settings {
        size: WINDOW_SIZE,
        min_size: Some(min),
        decorations: false,
        exit_on_close_request: false,
        transparent: true,
        #[cfg(target_arch = "wasm32")]
        platform_specific: PlatformSpecific {
            target: Some("kithara".to_owned()),
        },
        ..Settings::default()
    }
}

pub(crate) struct Boot {
    pub(super) ui: AppUi,
    pub(super) snapshots: Arc<ArcSwap<EngineSnapshot>>,
    pub(super) catalog: Catalog,
    pub(super) palette: Palette,
    #[cfg(feature = "masonry")]
    pub(super) settings: UiConfig,
    pub(super) commands: UnboundedSender<Envelope>,
}

#[bon::bon]
impl Boot {
    #[builder]
    pub(crate) fn new(
        package: Option<&Path>,
        settings: &UiConfig,
        tracks: Vec<String>,
        palette: Palette,
        snapshots: Arc<ArcSwap<EngineSnapshot>>,
        commands: UnboundedSender<Envelope>,
        #[builder(default)] chrome_hidden: bool,
    ) -> Result<Self, FrontendError> {
        let mut ui = AppUi::new(Package::load(package)?, settings)?;
        ui.cache.window.set_chrome_hidden(chrome_hidden);
        Ok(Self {
            snapshots,
            palette,
            commands,
            ui,
            catalog: Catalog::new(tracks),
            #[cfg(feature = "masonry")]
            settings: settings.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use ::kithara::{
        platform::tokio::sync::mpsc,
        ui::render::{ReadValue, Reads, Walk},
    };
    use iced::window::Id;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::gui::reads::ReadRoot;

    fn booted(chrome_hidden: bool) -> Kithara {
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let (commands, _) = mpsc::unbounded_channel();
        let boot = Boot::builder()
            .settings(&UiConfig::default())
            .tracks(Vec::new())
            .palette(Palette::default())
            .snapshots(snapshots)
            .commands(commands)
            .chrome_hidden(chrome_hidden)
            .build()
            .unwrap();
        Kithara::mounted(boot, Id::unique())
    }

    #[kithara::test]
    fn the_root_decides_whether_the_window_chrome_is_hidden() {
        for chrome_hidden in [true, false] {
            let state = booted(chrome_hidden);
            let root = ReadRoot::new(&state);
            let reads = Walk::new(&root);

            assert_eq!(
                reads.get("ui.window.chrome_hidden@window=1"),
                Some(ReadValue::Bool(chrome_hidden)),
            );
        }
    }
}
