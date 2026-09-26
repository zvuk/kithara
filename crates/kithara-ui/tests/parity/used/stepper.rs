use iced::{
    Event, Point as IcedPoint, Rectangle, Size,
    advanced::{
        Shell, clipboard,
        layout::{Layout, Limits},
        mouse::{Cursor, ScrollDelta},
        widget::Tree,
    },
    mouse,
};
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::compile,
    draw::Pt,
    interact::{Input, MOUSE, PointerInput, PointerPhase, Scroll},
    render::{Clock, ReadValue, Reads, Skin, UiEvent, tree},
    source::{MemResolver, UiConfig},
    view,
};
use num_traits::cast::AsPrimitive;

use crate::shared::{Endpoints, renderer};

/// The shape of the document below, and the window both hosts are given.
mod consts {
    /// The room the window leaves, which the row fills.
    pub(super) const CASE: (u32, u32) = (300, 60);
    /// One detent up, which both hosts owe the same reading of.
    pub(super) const DETENT: f32 = -1.0;
}

/// A document whose whole content is a row that names what it writes, the way a
/// deck's tempo block is the surface its readings stand on.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "tempo.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "tempo",
            root: Split(axis: Vertical, size: (w: Fill, h: Fill), children: [
                (weight: 1.0, node: Module(instance: "deck", source: "tempo.kmodule.ron",
                    size: (w: Fill, h: Fill))),
            ]))"#,
    );
    resolver.insert(
        "tempo.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "tempo", chrome: Plain,
            root: Row(id: "tempo", size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0,
                write: Parameter(id: "fixture.rate"), children: [
                    Text(id: "label", style: MicroLabel, label: "TEMPO"),
                ]))"#,
    );
    resolver
}

/// An application that keeps every event the document publishes to it.
#[derive(Default)]
struct Tempo {
    published: Vec<UiEvent>,
}

impl Reads for Tempo {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

impl App for Tempo {
    fn document(&self) -> &str {
        "tempo.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, event: UiEvent) {
        self.published.push(event);
    }
}

/// What the retained host publishes for one detent over the row.
fn retained() -> Vec<UiEvent> {
    let endpoints = Endpoints::default();
    let resolver = documents();
    let (width, height) = consts::CASE;
    let mut ui = Ui::new(
        Tempo::default(),
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (width, height),
        1.0,
    )
    .unwrap_or_else(|error| panic!("the tempo fixture must mount: {error}"));
    let label = ui
        .rect_of("deck/label")
        .expect("the row that writes the tempo must be laid out");
    ui.input(Input::Pointer(PointerInput::new(
        MOUSE,
        None,
        PointerPhase::Move,
        Some(Pt {
            x: label.x + label.w / 2.0,
            y: label.y + label.h / 2.0,
        }),
        1,
    )));

    ui.input(Input::Wheel(Scroll::Lines {
        x: 0.0,
        y: consts::DETENT,
    }));

    ui.app().published.clone()
}

/// What the immediate host publishes for the same detent over the same row.
fn neutral() -> Vec<UiEvent> {
    let ui = compile(
        "tempo.klayout.ron",
        &documents(),
        &Endpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the tempo fixture must compile: {error}"));
    let (width, height) = consts::CASE;
    let renderer = renderer();
    let viewport = Size::new(width.as_(), height.as_());
    let mut element = tree::render(
        &ui.root,
        &ui,
        &Tempo::default(),
        &view::EMPTY,
        builtin::skin(),
        Clock::default(),
        None,
    );
    let mut state = Tree::new(element.as_widget());
    let node =
        element
            .as_widget_mut()
            .layout(&mut state, &renderer, &Limits::new(Size::ZERO, viewport));
    let mut clipboard = clipboard::Null;
    let mut published = Vec::new();
    let mut shell = Shell::new(&mut published);
    element.as_widget_mut().update(
        &mut state,
        &Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Lines {
                x: 0.0,
                y: consts::DETENT,
            },
        }),
        Layout::new(&node),
        Cursor::Available(IcedPoint::new(viewport.width / 2.0, viewport.height / 2.0)),
        &renderer,
        &mut clipboard,
        &mut shell,
        &Rectangle::with_size(viewport),
    );
    drop(shell);
    published
}

/// A detent over a row that names what it writes publishes the same step on
/// both hosts.
///
/// A deck's tempo has no control of its own: the block of readings is the
/// surface, and a detent anywhere on it moves the tempo. A host that mounts the
/// readings and not the surface draws a tempo nobody can change.
#[kithara::test]
fn both_hosts_publish_the_same_step_for_one_detent() {
    assert_eq!(
        retained(),
        neutral(),
        "the two hosts disagree on what a detent over the row that writes the tempo publishes"
    );
}
