use std::{error::Error, path::Path};

use arc_swap::ArcSwap;
use iced::{Size, window::Settings};
use kithara::{
    platform::{
        sync::{Arc, Mutex},
        tokio::sync::mpsc::UnboundedSender,
    },
    ui::{render::fonts, source::UiConfig},
};
use kithara_app_library::Registration;

use super::{
    app::Kithara,
    library::{Library, SourceAdditions, StartupSource},
    ui::{AppUi, package::Package, window::consts::WINDOW_SIZE},
    update, view,
};
use crate::{
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
        platform_specific: iced::window::settings::PlatformSpecific {
            target: Some("kithara".to_owned()),
        },
        ..Settings::default()
    }
}

pub(crate) struct Boot {
    pub(super) ui: AppUi,
    pub(super) snapshots: Arc<ArcSwap<EngineSnapshot>>,
    pub(super) library: Library,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) picker: super::library::FolderPicker,
    pub(super) palette: Palette,
    #[cfg(feature = "masonry")]
    pub(super) settings: UiConfig,
    pub(super) commands: UnboundedSender<Envelope>,
    pub(super) config_path: String,
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
        runtime: kithara::platform::tokio::runtime::Handle,
        #[builder(default)] plugins: Vec<Registration>,
        #[builder(default)] chrome_hidden: bool,
        config_path: Option<&Path>,
    ) -> Result<Self, FrontendError> {
        #[cfg(not(target_arch = "wasm32"))]
        let (explorer, picker) =
            super::library::Explorer::registered(std::env::home_dir(), runtime.clone());
        let registered = vec![
            StartupSource::registered(tracks),
            #[cfg(not(target_arch = "wasm32"))]
            explorer,
        ];
        let registered: Vec<_> = registered.into_iter().chain(plugins).collect();
        let package = Package::load(package, SourceAdditions::new(&registered), &settings.limits)?;
        let library = Library::new(registered, package.text())?;
        let mut ui = AppUi::new(package, settings, runtime)?;
        ui.cache.window.set_chrome_hidden(chrome_hidden);
        Ok(Self {
            ui,
            snapshots,
            library,
            #[cfg(not(target_arch = "wasm32"))]
            picker,
            palette,
            commands,
            #[cfg(feature = "masonry")]
            settings: settings.clone(),
            config_path: config_path
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
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
    use crate::gui::{reads::ReadRoot, test_fixture};

    fn booted(chrome_hidden: bool, config_path: Option<&Path>) -> Kithara {
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let (commands, _) = mpsc::unbounded_channel();
        let runtime = test_fixture::runtime();
        let boot = Boot::builder()
            .settings(&UiConfig::default())
            .tracks(Vec::new())
            .palette(Palette::default())
            .snapshots(snapshots)
            .commands(commands)
            .runtime(runtime.handle().clone())
            .chrome_hidden(chrome_hidden)
            .maybe_config_path(config_path)
            .build()
            .unwrap();
        Kithara::mounted(boot, Id::unique())
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Short))]
    fn the_add_folder_row_shows_wherever_a_folder_picker_exists() {
        let (package, library) =
            test_fixture::mount(None, vec![StartupSource::registered(Vec::new())])
                .expect("the startup source mounts");
        let runtime = test_fixture::runtime();
        let state = test_fixture::mounted(package, library, runtime.handle());
        let root = ReadRoot::new(&state);

        assert_eq!(
            Walk::new(&root).get("library.add_folder.hidden"),
            Some(ReadValue::Bool(false)),
        );
    }

    #[kithara::test(timeout(Short))]
    fn the_app_reads_the_configuration_path_it_booted_with() {
        let state = booted(false, Some(Path::new("/opt/kithara/kithara.yaml")));
        let root = ReadRoot::new(&state);

        assert_eq!(
            Walk::new(&root).get("ui.app.config_path"),
            Some(ReadValue::Text("/opt/kithara/kithara.yaml")),
        );
    }

    #[kithara::test(timeout(Short))]
    fn the_root_decides_whether_the_window_chrome_is_hidden() {
        for chrome_hidden in [true, false] {
            let state = booted(chrome_hidden, None);
            let root = ReadRoot::new(&state);
            let reads = Walk::new(&root);

            assert_eq!(
                reads.get("ui.window.chrome_hidden@window=1"),
                Some(ReadValue::Bool(chrome_hidden)),
            );
        }
    }
}
