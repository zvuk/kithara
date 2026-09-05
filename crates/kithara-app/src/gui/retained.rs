use std::rc::Rc;

use iced::window;
use kithara::ui::{
    app::{App, Config, RunError},
    render::{Reads, Skin, UiEvent, Walk},
};

use super::{
    app::{Decks, Kithara},
    frontend::{window_min, window_size},
    message::Message,
    reads::ReadRoot,
    ui::{self, AppUi},
    update,
};
use crate::{catalog::Catalog, config::AppConfig, deck::DeckSet, gui::ui::endpoints};

/// The studio driven by the retained host.
///
/// Everything below the surface is the studio the immediate host shows: the
/// same state, the same reads, the same translation of what the document
/// published. Only the shell differs.
pub(crate) struct Studio {
    state: Kithara,
}

impl Studio {
    pub(crate) fn new(
        session: DeckSet,
        decks: Decks,
        catalog: Catalog,
        config: AppConfig,
        studio: AppUi,
        broadcast: crate::broadcast::Broadcaster,
    ) -> Self {
        Self {
            state: Kithara::mounted(
                session,
                decks,
                catalog,
                config,
                studio,
                broadcast,
                window::Id::unique(),
            ),
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
    let endpoints = endpoints::Registry::default();
    let (size, min_size) = (window_size(), window_min(app.state.ui.window_min()));
    // Cloned into a local rather than borrowed from `app.state.config.ui`
    // directly: `app` itself is moved into the call below, and a reference
    // into one of its fields cannot outlive that move.
    let settings = app.state.config.ui.clone();
    kithara::ui::app::run(
        app,
        Config::builder()
            .endpoints(&endpoints)
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

#[cfg(test)]
mod tests {
    use ::kithara::ui::{
        app::Ui,
        render::{ReadValue, Reads},
    };
    use kithara_test_utils::kithara;

    use super::{App, Config, Rc, Skin, endpoints, ui, ui::package::Package};

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
        let package = Package::load(None).expect("the app package must answer for both decks");
        let endpoints = endpoints::Registry::default();
        let mut ui = Ui::new(
            Empty {
                package: Rc::clone(&package),
            },
            Config::builder()
                .endpoints(&endpoints)
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
