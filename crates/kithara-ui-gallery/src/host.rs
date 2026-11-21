use kithara_ui::{
    app::{App, RunError, Ui},
    module::ViewSet,
    render::{Reads, Skin, UiEvent},
    view::ViewState,
};

use crate::{capture::Shot, demo, sections};

/// Stands a mounted screen at the page a shot names, with the surface that
/// page is about open.
///
/// Every page lives in one screen, so a harness opens one by turning the
/// screen's own state rather than by mounting another document. The surface is
/// turned after the page, because the state naming it belongs to a module the
/// page mounts and the screen names nothing of a page it is not showing.
///
/// # Errors
/// Returns whatever standing the screen at that page fails with.
pub fn stand<Application>(ui: &mut Ui<'_, Application>, shot: Shot) -> Result<(), RunError>
where
    Application: App,
{
    for (state, page) in shot.stands() {
        ui.stand(state, page)?;
    }
    for state in shot.opens() {
        ui.set(state, ViewSet::On)?;
    }
    Ok(())
}

#[derive(Default)]
pub struct Gallery {
    reads: demo::DemoReads,
}

impl App for Gallery {
    fn document(&self) -> &str {
        sections::entry()
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(&self.reads)
    }

    /// The page on screen is the screen's own to say, so the feed behind a
    /// page starts and stops with the page rather than with a press.
    fn turned(&mut self, view: &ViewState) {
        if let Some(page) = view.page(sections::PAGE).and_then(sections::named) {
            self.reads.show(page);
        }
    }

    fn update(&mut self, event: UiEvent) {
        if let UiEvent::Write { key, value } = event {
            self.reads.write(&key, value);
        }
    }

    delegate::delegate! {
        to self.reads {
            fn skin(&self) -> &Skin;
            fn tick(&mut self);
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use kithara_ui::{
        app::{Config, Ui},
        builtin,
        compile::compile,
        draw::Pt,
        interact::{Input, MOUSE, PointerInput, PointerPhase},
        render::{ControlAction, Published},
    };

    use super::{App, Gallery, Shot, ViewState, demo, sections};
    use crate::{custom, fixture::resolver};

    /// Pressing a row on the skins page dresses the gallery in that skin.
    #[kithara::test]
    fn choosing_a_skin_dresses_the_gallery_in_it() {
        let mut gallery = Gallery::default();
        assert_eq!(gallery.skin().id(), "kithara-dark");

        pressing(&mut gallery, "skins/kithara-neon/item");

        assert_eq!(gallery.skin().id(), "kithara-neon");
    }

    /// The skin outlives the page it was chosen on, which is the whole point of
    /// choosing one: every widget the gallery shows is looked at in it.
    #[kithara::test]
    fn turning_the_page_keeps_the_skin_it_was_chosen_in() {
        let mut gallery = Gallery::default();
        pressing(&mut gallery, "skins/kithara-light/item");

        for page in sections::pages().iter().copied() {
            let mut view = ViewState::default();
            view.stand(sections::PAGE, page);
            gallery.turned(&view);
            assert_eq!(
                gallery.skin().id(),
                "kithara-light",
                "the gallery undressed itself on {page}"
            );
        }
    }

    fn pressing(gallery: &mut Gallery, path: &str) {
        let shot = Shot {
            tab: "skins",
            module: None,
        };
        let mut view = shot.standing();
        let ui = compile(
            sections::entry(),
            &resolver(),
            &demo::registry(),
            builtin::skin_doc(),
            builtin::text_doc(),
            custom::config(),
            &view,
        )
        .unwrap_or_else(|error| panic!("the skins page must compile: {error}"));
        let pressed = Published::Gesture {
            path: path.to_owned(),
            action: ControlAction::Activate,
        };
        let event = gallery.reads(|reads| ui.views().settle(pressed, reads, &mut view));
        gallery.update(event.unwrap_or_else(|| panic!("{path} must declare a write")));
    }

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
        let endpoints = demo::registry();
        let resolver = resolver();
        let kinds = custom::kinds();
        let config = Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .kinds(&kinds)
            .build();
        let size = (
            num_traits::cast::AsPrimitive::<u32>::as_(1100.0 * scale),
            num_traits::cast::AsPrimitive::<u32>::as_(720.0 * scale),
        );
        let mut ui = Ui::new(Gallery::default(), config, size, scale)
            .unwrap_or_else(|error| panic!("the gallery must mount: {error}"));
        ui.frame(Duration::from_millis(16));
        ui.render()
            .unwrap_or_else(|error| panic!("the gallery must draw: {error}"));
        assert_eq!(
            ui.view().page(sections::PAGE),
            None,
            "a screen pressed nowhere must stand at the page the document calls initial"
        );

        let at = Pt { x: 60.0, y: 113.0 };
        ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            PointerPhase::Down,
            Some(at),
            1,
        )));

        assert_eq!(
            ui.view().page(sections::PAGE),
            Some("buttons"),
            "a press on the BUTTONS nav item must turn the page at {scale}x"
        );
    }
}

#[cfg(test)]
mod fills {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use kithara_ui::{
        app::{Config, Ui},
        builtin,
        capture::Offscreen,
    };
    use masonry::vello::peniko::Color;

    use super::{Gallery, demo};
    use crate::{custom, fixture::resolver};

    /// The document must fill the surface it was handed, at any display scale.
    ///
    /// This is the property a window bug hid behind twice: a headless capture
    /// asks for the size it wants and is always satisfied, while a window that
    /// mixes up logical and physical points quietly lays the document out in a
    /// quarter of the glass. Rasterising and looking at the far corner is the
    /// cheapest honest check that the two agree.
    #[kithara::test(timeout(::std::time::Duration::from_millis(1492)))]
    fn the_document_reaches_the_far_corner_at_any_scale() {
        for scale in [1.0, 2.0] {
            let width = num_traits::cast::AsPrimitive::<u32>::as_(1100.0 * scale);
            let height = num_traits::cast::AsPrimitive::<u32>::as_(720.0 * scale);
            let endpoints = demo::registry();
            let resolver = resolver();
            let kinds = custom::kinds();
            let config = Config::builder()
                .endpoints(&endpoints)
                .resolver(&resolver)
                .text(builtin::text_doc())
                .kinds(&kinds)
                .build();
            let mut ui = Ui::new(Gallery::default(), config, (width, height), scale)
                .unwrap_or_else(|error| panic!("the gallery must mount at {scale}x: {error}"));
            ui.frame(Duration::from_millis(16));
            let frame = ui
                .render()
                .unwrap_or_else(|error| panic!("the gallery must draw at {scale}x: {error}"));
            let mut off = Offscreen::new(width, height)
                .unwrap_or_else(|error| panic!("offscreen at {scale}x: {error}"));
            // Black, not the skin's page colour: a corner that only shows what
            // the host cleared to would read as painted whatever the document
            // did, and the question here is whether the document reached it.
            let mut rgba = Vec::new();
            off.rasterise(&frame, scale, Color::BLACK, &mut rgba)
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
