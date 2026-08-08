//! The gallery as a toolkit-neutral application.
//!
//! Everything here is stated in terms of `kithara_ui::app`; nothing names a
//! toolkit. Which host shows it is decided by the feature the crate was built
//! with.

use std::env;

use kithara_ui::{
    app::{App, Config},
    builtin,
    render::{Reads, UiEvent},
};

use super::{Consts, Tab, capture::Shot, mock, resolver};

/// Runs the gallery in a window when asked. Returns `false` when the
/// environment variable is absent, so the caller falls through.
pub(super) fn run() -> bool {
    if env::var_os("KITHARA_GALLERY_MASONRY").is_none() {
        return false;
    }
    let endpoints = mock::registry();
    let resolver = resolver();
    let size = (
        num_traits::cast::AsPrimitive::<u32>::as_(Consts::WIDTH),
        num_traits::cast::AsPrimitive::<u32>::as_(Consts::HEIGHT),
    );
    // The document carries its own title bar and window buttons, so the system
    // frame stays off, exactly as it does under the other host.
    let config = Config::builder()
        .endpoints(&endpoints)
        .resolver(&resolver)
        .skin(builtin::skin())
        .skin_doc(builtin::skin_doc())
        .decorations(false)
        .min_size(size)
        .title("Kithara UI Gallery")
        .build();
    if let Err(error) = kithara_ui::app::run(Gallery::default(), config, size) {
        eprintln!("gallery did not run: {error}");
    }
    true
}

#[derive(Default)]
pub(super) struct Gallery {
    reads: mock::MockReads,
}

impl Gallery {
    /// The gallery already turned to one page, for photographing it.
    pub(super) fn at(shot: Shot) -> Self {
        let mut reads = mock::MockReads::default();
        reads.select_tab(shot.tab);
        if let Some(module) = shot.module {
            reads.select_module(module);
        }
        Self { reads }
    }
}

impl App for Gallery {
    fn document(&self) -> &str {
        let tab = self.reads.active_tab();
        if tab == Tab::Modules {
            self.reads.active_module().entry()
        } else {
            tab.entry()
        }
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(&self.reads)
    }

    fn update(&mut self, event: UiEvent) {
        match event {
            UiEvent::Control { path, action } => {
                if let Ok(tab) = Tab::try_from(path.as_str()) {
                    self.reads.select_tab(tab);
                } else {
                    self.reads.apply(&path, &action);
                }
            }
            UiEvent::LibraryQuery(query) => self.reads.set_library_query(query),
            UiEvent::ToggleModule(module) => self.reads.toggle_module(module),
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.reads.tick();
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use kithara_ui::{
        app::Ui,
        draw::Pt,
        interact::{Input, MOUSE, PointerInput, PointerPhase},
    };

    use super::{App, Config, Gallery, builtin, mock, resolver};

    /// Presses where the nav reads BUTTONS and expects the gallery to turn to
    /// that page. This is the whole chain the window depends on: pointer, leaf,
    /// action, application, rebuilt document.
    #[kithara::test]
    fn pressing_a_nav_item_turns_the_page() {
        for scale in [1.0, 2.0] {
            turns_the_page_at(scale);
        }
    }

    /// The window runs at whatever the display reports, so the same logical
    /// press has to land on the same control at any scale.
    fn turns_the_page_at(scale: f64) {
        let endpoints = mock::registry();
        let resolver = resolver();
        let config = Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .skin(builtin::skin())
            .skin_doc(builtin::skin_doc())
            .build();
        let size = (
            num_traits::cast::AsPrimitive::<u32>::as_(1100.0 * scale),
            num_traits::cast::AsPrimitive::<u32>::as_(720.0 * scale),
        );
        let mut ui = Ui::new(Gallery::default(), config, size, scale)
            .unwrap_or_else(|error| panic!("the gallery must mount: {error}"));
        ui.frame(Duration::from_millis(16));
        ui.scene()
            .unwrap_or_else(|error| panic!("the gallery must draw: {error}"));
        assert_eq!(ui.app().document(), "gallery-atoms.klayout.ron");

        let at = Pt { x: 60.0, y: 113.0 };
        ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            PointerPhase::Down,
            Some(at),
            1,
        )));

        assert_eq!(
            ui.app().document(),
            "gallery-buttons.klayout.ron",
            "a press on the BUTTONS nav item must turn the page at {scale}x"
        );
    }
}

#[cfg(test)]
mod fills {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use kithara_ui::app::Ui;

    use super::{Config, Gallery, builtin, mock, resolver};
    use crate::masonry_shots::Offscreen;

    /// The document must fill the surface it was handed, at any display scale.
    ///
    /// This is the property a window bug hid behind twice: a headless capture
    /// asks for the size it wants and is always satisfied, while a window that
    /// mixes up logical and physical points quietly lays the document out in a
    /// quarter of the glass. Rasterising and looking at the far corner is the
    /// cheapest honest check that the two agree.
    #[kithara::test]
    fn the_document_reaches_the_far_corner_at_any_scale() {
        for scale in [1.0, 2.0] {
            let width = num_traits::cast::AsPrimitive::<u32>::as_(1100.0 * scale);
            let height = num_traits::cast::AsPrimitive::<u32>::as_(720.0 * scale);
            let endpoints = mock::registry();
            let resolver = resolver();
            let config = Config::builder()
                .endpoints(&endpoints)
                .resolver(&resolver)
                .skin(builtin::skin())
                .skin_doc(builtin::skin_doc())
                .build();
            let mut ui = Ui::new(Gallery::default(), config, (width, height), scale)
                .unwrap_or_else(|error| panic!("the gallery must mount at {scale}x: {error}"));
            ui.frame(Duration::from_millis(16));
            let scene = ui
                .scene()
                .unwrap_or_else(|error| panic!("the gallery must draw at {scale}x: {error}"));
            let mut off = Offscreen::new(width, height)
                .unwrap_or_else(|error| panic!("offscreen at {scale}x: {error}"));
            let rgba = off
                .rasterise(&scene)
                .unwrap_or_else(|error| panic!("rasterise at {scale}x: {error}"));

            let width_px = width as usize;
            let corner = ((height as usize - 1) * width_px + width_px - 1) * 4;
            let lit = rgba[corner..corner + 3].iter().any(|channel| *channel > 8);
            assert!(
                lit,
                "at {scale}x the far corner is unpainted, so the document covered only part of \
                 the {width}x{height} surface it was given"
            );
        }
    }
}
