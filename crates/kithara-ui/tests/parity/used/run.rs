use iced::{
    Size,
    advanced::{
        layout::{Layout, Limits},
        widget::Tree,
    },
};
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::compile,
    draw::Rect,
    render::{Clock, ReadValue, Reads, Skin, UiEvent, tree},
    source::{MemResolver, UiConfig},
    view,
};
use num_traits::cast::AsPrimitive;

use crate::shared::{Endpoints, collect_rows, renderer, snapped};

/// A strip carrying one run wider than the room the window leaves it, so each
/// host has to say what a squeezed run asks its parent for.
struct Squeeze;

impl Squeeze {
    /// The room down the window, which the run never competes for.
    const HEIGHT: u32 = 60;
    /// A window narrower across than the run wants to be, one about as wide,
    /// and one with room to spare.
    const WIDTHS: [u32; 3] = [40, 60, 200];

    /// A document whose whole content is one run of words in a strip across the
    /// window, so the only thing either host has to decide is how wide that run
    /// is.
    fn document() -> MemResolver {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "strip.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "strip",
                root: Split(axis: Vertical, measure: Height, size: (w: Fill, h: Fill), children: [
                    (weight: 1.0, node: Module(instance: "strip", source: "strip.kmodule.ron",
                        size: (w: Fill, h: Fill))),
                ]))"#,
        );
        resolver.insert(
            "strip.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "strip", chrome: Plain,
                root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                    Text(id: "label", style: MicroLabel, label: "1 / WINDOW"),
                ]))"#,
        );
        resolver
    }

    /// The box the immediate host laid the strip's run into.
    fn neutral(width: u32) -> Rect {
        let ui = compile(
            "strip.klayout.ron",
            &Self::document(),
            &Endpoints::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
            &view::EMPTY,
        )
        .unwrap_or_else(|error| panic!("the strip fixture must compile: {error}"));
        let renderer = renderer();
        let viewport = Size::new(width.as_(), Self::HEIGHT.as_());
        let mut element = tree::render(
            &ui.root,
            &ui,
            &Strip,
            &view::EMPTY,
            builtin::skin(),
            Clock::default(),
            None,
        );
        let mut state = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut rows = Vec::new();
        collect_rows(Layout::new(&node), &mut rows);
        let [run] = rows[..] else {
            panic!(
                "the strip holds one run, and the immediate host laid out {}",
                rows.len()
            )
        };
        run
    }

    /// The box the retained host laid the strip's run into.
    fn retained(width: u32) -> Rect {
        let endpoints = Endpoints::default();
        let resolver = Self::document();
        let mut ui = Ui::new(
            Strip,
            Config::builder()
                .endpoints(&endpoints)
                .resolver(&resolver)
                .text(builtin::text_doc())
                .build(),
            (width, Self::HEIGHT),
            1.0,
        )
        .unwrap_or_else(|error| panic!("the strip fixture must mount: {error}"));
        ui.scene()
            .unwrap_or_else(|error| panic!("the retained host must draw the strip: {error}"));
        ui.rect_of("strip/label")
            .unwrap_or_else(|| panic!("the run must be laid out at {width} across"))
    }
}

/// An application that reads nothing, because the strip asks nothing of it.
struct Strip;

impl Reads for Strip {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

impl App for Strip {
    fn document(&self) -> &str {
        "strip.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, _event: UiEvent) {}
}

/// A run asks both hosts for the same box, whatever room it is offered.
///
/// A run says how wide it wants to be before it can know how much room there
/// is. Shaped against the room instead, it asks for the width its broken lines
/// happen to need, which is narrower than the room it was already offered: the
/// same words then land on a different number of lines on the two hosts, and
/// everything beside them moves.
#[kithara::test]
fn both_hosts_give_a_squeezed_run_the_same_box() {
    for width in Squeeze::WIDTHS {
        assert_eq!(
            snapped(Squeeze::retained(width)),
            snapped(Squeeze::neutral(width)),
            "at {width} across the two hosts disagree on the box one run of words asks for"
        );
    }
}
