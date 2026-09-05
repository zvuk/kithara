//! The hand each host asks for over a stepping surface.
//!
//! A surface is a property of a group, not a control standing in one, so the
//! gesture census - which enumerates kinds of control - has no row for it and
//! never asked either host what it shows over one. The two answered
//! differently: the immediate host reads the surface as a thing that steps, the
//! retained host read nothing at all.

use iced::mouse;
use kithara_test_utils::kithara;
use masonry::core::CursorIcon;

use super::{immediate::Immediate, shared::Endpoints};
use crate::{
    app::{App, Config, Ui},
    builtin,
    compile::{CompiledUi, compile},
    draw::Pt,
    interact::{CursorShape, Input, MOUSE, PointerInput, PointerPhase},
    render::{ReadValue, Reads, Skin, UiEvent, masonry::cursor_icon},
    source::{MemResolver, UiConfig},
    view,
};

/// The window both hosts are given, and the hand the surface owes.
struct Consts;

impl Consts {
    /// A window with room below the surface, so a drag can walk off it without
    /// walking off the window.
    const CASE: (u32, u32) = (200, 120);
    /// The hand a stepping surface asks for, on either host.
    const HAND: CursorShape = CursorShape::ResizeV;
    /// Well below the surface, which ends 40 down.
    const OFF: f32 = 90.0;
}

/// A document whose top band is a row that names what it writes, with plain
/// room under it.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "tempo.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "tempo",
            root: Module(instance: "deck", source: "tempo.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "tempo.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "tempo", chrome: Plain,
            root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Row(id: "tempo", size: (w: Fill, h: Fixed(40.0)), gap: 0.0, pad: 0.0,
                    write: Parameter(id: "fixture.rate"), children: [
                        Text(id: "label", style: MicroLabel, label: "TEMPO"),
                    ]),
                Spacer(id: "room", size: Some((w: Fill, h: Fill))),
            ]))"#,
    );
    resolver
}

/// An application that reads nothing and keeps what it is told.
#[derive(Default)]
struct Tempo;

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

    fn update(&mut self, _event: UiEvent) {}
}

/// A retained host with the document mounted in it.
fn mounted<'a>(endpoints: &'a Endpoints, resolver: &'a MemResolver) -> Ui<'a, Tempo> {
    Ui::new(
        Tempo,
        Config::builder()
            .endpoints(endpoints)
            .resolver(resolver)
            .text(builtin::text_doc())
            .build(),
        Consts::CASE,
        1.0,
    )
    .unwrap_or_else(|error| panic!("the tempo fixture must mount: {error}"))
}

/// The same document, compiled for a host that is handed one.
fn compiled() -> CompiledUi {
    compile(
        "tempo.klayout.ron",
        &documents(),
        &Endpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the tempo fixture must compile: {error}"))
}

/// A point on the surface, and one well below it.
///
/// The retained host is asked where the surface stands because it is the one
/// that can be asked, and only a leaf carries a path, so the reading inside the
/// row answers for the row. Whether the hosts agree on where a control stands
/// is what the rect corpus is for.
fn over_and_off() -> (Pt, Pt) {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let ui = mounted(&endpoints, &resolver);
    let row = ui
        .rect_of("deck/label")
        .expect("the row that writes the tempo must be laid out");
    let at = Pt {
        x: row.x + row.w / 2.0,
        y: row.y + row.h / 2.0,
    };
    (
        at,
        Pt {
            x: at.x,
            y: Consts::OFF,
        },
    )
}

/// The last hand the retained host asked its window for, after the pointer has
/// walked the given path with the button held or not.
fn retained_hand(steps: &[(PointerPhase, Pt)]) -> Option<CursorIcon> {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let mut ui = mounted(&endpoints, &resolver);
    for (phase, at) in steps {
        ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            *phase,
            Some(*at),
            1,
        )));
    }
    ui.take_cursor()
}

/// The hand the immediate host asks for after the same walk.
fn immediate_hand(drag: bool) -> mouse::Interaction {
    let ui = compiled();
    let (at, off) = over_and_off();
    let mut host = Immediate::mount(Tempo, &ui, builtin::skin(), Consts::CASE);
    if drag {
        host.press_at(at);
        host.hover_at(off);
    } else {
        host.hover_at(at);
        host.hover_at(off);
    }
    host.hand()
}

/// The retained host reads a stepping surface as a thing that steps.
#[kithara::test]
fn the_retained_host_asks_for_the_stepping_hand_over_the_surface() {
    let (at, _) = over_and_off();

    let hand = retained_hand(&[(PointerPhase::Move, at)]);

    assert_eq!(
        hand,
        Some(cursor_icon(Consts::HAND)),
        "the retained host must read a stepping surface as one"
    );
}

/// And so does the immediate host, which is where the shape comes from.
#[kithara::test]
fn the_immediate_host_asks_for_the_stepping_hand_over_the_surface() {
    let ui = compiled();
    let (at, _) = over_and_off();
    let mut host = Immediate::mount(Tempo, &ui, builtin::skin(), Consts::CASE);

    host.hover_at(at);

    assert_eq!(
        host.hand(),
        mouse::Interaction::from(Consts::HAND),
        "the immediate host must read a stepping surface as one"
    );
}

/// A drag holds the hand after the pointer has left the surface, because the
/// surface holds the pointer for as long as the drag lasts.
#[kithara::test]
fn the_retained_host_keeps_the_stepping_hand_when_a_drag_leaves_the_surface() {
    let (at, off) = over_and_off();

    let hand = retained_hand(&[
        (PointerPhase::Move, at),
        (PointerPhase::Down, at),
        (PointerPhase::Move, off),
    ]);

    assert_eq!(
        hand,
        Some(cursor_icon(Consts::HAND)),
        "a drag under way must go on reading as a step wherever the pointer has got to"
    );
}

/// The other host holds it for exactly as long.
#[kithara::test]
fn the_immediate_host_keeps_the_stepping_hand_when_a_drag_leaves_the_surface() {
    assert_eq!(
        immediate_hand(true),
        mouse::Interaction::from(Consts::HAND),
        "a drag under way must go on reading as a step on the immediate host too"
    );
}

/// Off the surface with nothing held, the retained host drops the hand.
///
/// This is what makes the drag above worth pinning: the point it ends on is one
/// the surface does not answer for.
#[kithara::test]
fn the_retained_host_drops_the_hand_off_the_surface() {
    let (at, off) = over_and_off();

    let hand = retained_hand(&[(PointerPhase::Move, at), (PointerPhase::Move, off)]);

    assert_eq!(
        hand,
        Some(cursor_icon(CursorShape::None)),
        "a pointer that walked off the surface holding nothing must drop the hand"
    );
}

/// And so does the immediate host.
#[kithara::test]
fn the_immediate_host_drops_the_hand_off_the_surface() {
    assert_eq!(
        immediate_hand(false),
        mouse::Interaction::from(CursorShape::None),
        "a pointer that walked off the surface holding nothing must drop the hand"
    );
}
