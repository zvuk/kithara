use std::rc::Rc;

use iced::{Size, window::Id};
use kithara::ui::{
    app,
    app::{App, Config, RunError},
    render::{Reads, Skin, UiEvent, Walk},
    source::UiConfig,
};
use num_traits::cast::AsPrimitive;

use super::{
    app::Kithara,
    frontend::Boot,
    message::Message,
    reads::ReadRoot,
    ui::{self, window::consts::WINDOW_SIZE},
    update,
};

/// The studio driven by the retained host.
///
/// Everything below the surface is the studio the immediate host shows: the
/// same state, the same reads, the same translation of what the document
/// published. Only the shell differs.
pub(crate) struct Studio {
    state: Kithara,
    settings: UiConfig,
}

impl Studio {
    pub(crate) fn new(boot: Boot) -> Self {
        Self {
            settings: boot.settings.clone(),
            state: Kithara::mounted(boot, Id::unique()),
        }
    }
}

impl App for Studio {
    fn document(&self) -> &str {
        self.state.ui.package.document(self.state.ui.cache.layout())
    }

    /// The studio answers its endpoints by walking its own state, so the walk
    /// borrows the studio and lives no longer than this call.
    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        let root = ReadRoot::new(&self.state);
        with(&Walk::new(&root))
    }

    fn skin(&self) -> &Skin {
        self.state.ui.package.skin()
    }

    fn tick(&mut self) {
        drop(update::update(&mut self.state, Message::Tick));
    }

    fn update(&mut self, event: UiEvent) {
        if matches!(event, UiEvent::Window(_)) {
            return;
        }
        if let Some(message) = ui::translate(&mut self.state, event) {
            drop(update::update(&mut self.state, message));
        }
    }
}

/// Opens the studio window and runs it under the retained host.
///
/// # Errors
/// Returns [`RunError`] when the studio document does not compile, or when the
/// window and its GPU surface cannot be brought up.
pub(crate) fn run(app: Studio) -> Result<(), RunError> {
    let package = Rc::clone(&app.state.ui.package);
    let (size, min_size) = (window_size(), window_min(app.state.ui.window_min()));
    let settings = app.settings.clone();
    app::run(
        app,
        Config::builder()
            .endpoints(package.registry())
            .resolver(package.resolver())
            .text(package.text())
            .decorations(false)
            .min_size(min_size)
            .settings(&settings)
            .title("Kithara - DJ Studio")
            .build(),
        size,
    )
}

pub(crate) fn window_size() -> (u32, u32) {
    (whole(WINDOW_SIZE.width), whole(WINDOW_SIZE.height))
}

fn window_min(min: Size) -> (u32, u32) {
    (whole(min.width), whole(min.height))
}

fn whole(value: f32) -> u32 {
    value.as_()
}

#[cfg(test)]
mod tests {
    use ::kithara::ui::{
        app::Ui,
        render::{ReadValue, Reads},
    };
    use kithara_test_utils::kithara;

    use super::{App, Config, Rc, Skin, ui, ui::package::Package};

    /// A studio with nothing loaded: every control falls back to what the
    /// document and the skin say, which is the hardest case for a host that
    /// only draws what it was told.
    struct Empty {
        package: Rc<Package>,
    }

    impl Reads for Empty {
        fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
            None
        }
    }

    impl App for Empty {
        fn document(&self) -> &str {
            self.package.document(ui::cache::DeckLayout::Dual)
        }

        fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
            with(self)
        }

        fn skin(&self) -> &Skin {
            self.package.skin()
        }

        fn update(&mut self, _event: ::kithara::ui::render::UiEvent) {}
    }

    /// The studio's own documents draw under the retained host. The control
    /// census answers for one control at a time; this answers for the page the
    /// application actually ships, mounted the way the window mounts it.
    #[kithara::test]
    fn the_studio_draws_under_the_retained_host() {
        let package = crate::gui::test_fixture::package(None)
            .expect("the app package must answer for both decks");
        let mut ui = Ui::new(
            Empty {
                package: Rc::clone(&package),
            },
            Config::builder()
                .endpoints(package.registry())
                .resolver(package.resolver())
                .text(package.text())
                .build(),
            (1280, 760),
            1.0,
        )
        .unwrap_or_else(|error| panic!("the studio must mount under the retained host: {error}"));

        let frame = ui
            .render()
            .unwrap_or_else(|error| panic!("the studio must reach a paint pass: {error}"));

        let encoding = frame.scene().encoding();

        assert!(
            !encoding.is_empty(),
            "the retained host mounted the studio and drew nothing"
        );
        assert!(
            !encoding.resources.glyphs.is_empty(),
            "the studio is a page of labelled controls; a scene with no glyphs in it is a \
             background and nothing else"
        );
    }
}

#[cfg(test)]
mod library {
    use std::rc::Rc;

    use ::kithara::{
        platform::{sync::Arc, tokio::sync::mpsc},
        ui::{
            app::{Config, Ui},
            draw::{Pt, Rect},
            interact::{Input, MOUSE, PointerInput, PointerPhase},
        },
    };
    use arc_swap::ArcSwap;
    use kithara_test_utils::kithara;

    use super::{Studio, window_size};
    use crate::{engine::EngineSnapshot, gui::test_fixture};

    fn studio() -> Studio {
        studio_on(&test_fixture::config())
    }

    fn studio_on(config: &crate::config::AppConfig) -> Studio {
        let runtime = test_fixture::runtime();
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let (commands, _receiver) = mpsc::unbounded_channel();
        Studio::new(test_fixture::boot(
            runtime.handle(),
            config,
            snapshots,
            commands,
        ))
    }

    fn mounted(check: impl FnOnce(&mut Ui<'_, Studio>)) {
        mounted_on(studio(), check);
    }

    fn mounted_on(studio: Studio, check: impl FnOnce(&mut Ui<'_, Studio>)) {
        let package = Rc::clone(&studio.state.ui.package);
        let mut ui = Ui::new(
            studio,
            Config::builder()
                .endpoints(package.registry())
                .resolver(package.resolver())
                .text(package.text())
                .build(),
            window_size(),
            1.0,
        )
        .unwrap_or_else(|error| panic!("the studio must mount: {error}"));
        check(&mut ui);
    }

    fn laid_out(ui: &mut Ui<'_, Studio>, path: &str) -> Option<Rect> {
        ui.scene()
            .unwrap_or_else(|error| panic!("the studio must draw: {error}"));
        ui.rect_of(path).filter(|rect| rect.w > 0.0 && rect.h > 0.0)
    }

    fn shown(ui: &mut Ui<'_, Studio>) -> Vec<&'static str> {
        ["startup", "explorer"]
            .into_iter()
            .filter(|source| laid_out(ui, &format!("library/pages/{source}/rows")).is_some())
            .collect()
    }

    fn row(ui: &mut Ui<'_, Studio>, row: u8, chevron: bool) -> Pt {
        let tree = laid_out(ui, "library/tree").expect("the library draws its tree");
        let skin = &ui.app().state.ui.package.skin().tree;
        let x = if chevron {
            tree.x + skin.marker_width + skin.indent_base + skin.chevron_width / 2.0
        } else {
            tree.x + tree.w / 2.0
        };
        Pt {
            x,
            y: tree.y + skin.panel_padding_top + skin.row_height * (f32::from(row) + 0.5),
        }
    }

    fn press(ui: &mut Ui<'_, Studio>, at: Pt) {
        for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
            ui.input(Input::Pointer(PointerInput::new(
                MOUSE,
                None,
                phase,
                Some(at),
                1,
            )));
        }
    }

    fn labels(ui: &Ui<'_, Studio>) -> Vec<String> {
        ui.app()
            .state
            .library
            .tree()
            .iter()
            .map(|row| row.label.to_owned())
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn the_page_on_screen_is_the_selected_sources() {
        mounted(|ui| {
            assert_eq!(shown(ui), ["startup"], "Startup starts selected");

            let explorer = row(ui, 3, false);
            press(ui, explorer);
            assert_eq!(shown(ui), ["explorer"]);

            let startup = row(ui, 1, false);
            press(ui, startup);
            assert_eq!(shown(ui), ["startup"]);
        });
    }

    #[cfg(feature = "zvuk")]
    #[kithara::test(native, flash(false))]
    fn the_zvuk_page_stands_when_its_source_is_selected() {
        let mut config = test_fixture::config();
        let section = serde_yaml_ng::from_str("{auth_token: token, user_agent: agent}")
            .unwrap_or_else(|error| panic!("the section parses: {error}"));
        config.sources.insert("zvuk".to_owned(), section);
        config.shutdown.cancel();
        mounted_on(studio_on(&config), |ui| {
            let zvuk = "library/pages/zvuk/query";
            assert!(laid_out(ui, zvuk).is_none(), "Startup starts selected");

            let search = labels(ui)
                .iter()
                .position(|label| label == "Search")
                .unwrap_or_else(|| panic!("Zvuk lists Search: {:?}", labels(ui)));
            let at = row(ui, u8::try_from(search).unwrap_or(u8::MAX), false);
            press(ui, at);

            assert_eq!(ui.app().state.library.page(), Some("zvuk"));
            assert!(laid_out(ui, zvuk).is_some(), "the Zvuk page stands");
            assert_eq!(shown(ui), Vec::<&str>::new());
        });
    }

    #[kithara::test(native, flash(false))]
    fn library_followup_section_label_toggles_without_selecting() {
        mounted(|ui| {
            let closed = labels(ui);
            let chevron = row(ui, 0, false);

            press(ui, chevron);
            assert_eq!(labels(ui).len(), closed.len() - 1);
            assert_eq!(shown(ui), ["startup"]);

            press(ui, chevron);
            assert_eq!(labels(ui), closed);
        });
    }
}
