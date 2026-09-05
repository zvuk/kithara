//! What this host does with a flag the application turns under it.
//!
//! A document dresses a row by naming a flag: while the flag reads true the row
//! shows one face, and otherwise another. This host keeps the tree it mounted,
//! so a flag it read once and never again freezes the face the document was
//! mounted at - which is what a picked row in the quality menu did, staying
//! undressed however many times it was picked.
//!
//! The other host is not asked here. It rebuilds every frame, so it follows a
//! flag by construction, and nothing in a test can read a colour back out of it
//! without rasterising a window: the picture it draws is iced's, not this
//! crate's. Comparing the two pictures needs a rasteriser for each host in one
//! lane, which is a harness this crate does not have.

use kithara_test_utils::kithara;

use crate::{
    app::{App, Config, Ui},
    builtin,
    draw::Rgba,
    ids::EndpointId,
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ControlAction, ReadValue, Reads, Skin, UiEvent},
    source::MemResolver,
};

/// Two rows a press picks between, each dressed by a flag of its own.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "pick.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "pick",
            root: Module(instance: "demo", source: "pick.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "pick.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "pick", chrome: Plain,
            root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Pressable(id: "one", press: Command(id: "fixture.pick"),
                    child: Row(id: "one-row", size: (w: Fill, h: Fixed(26.0)), gap: 0.0,
                        active: Model(id: "fixture.first"), active_background: BgSelect,
                        children: [
                            Text(id: "one-label", style: Mono, label: "ONE",
                                active_color: Text, active: Model(id: "fixture.first")),
                        ])),
                Pressable(id: "two", press: Command(id: "fixture.pick"),
                    child: Row(id: "two-row", size: (w: Fill, h: Fixed(26.0)), gap: 0.0,
                        active: Model(id: "fixture.second"), active_background: BgSelect,
                        children: [
                            Text(id: "two-label", style: Mono, label: "TWO",
                                active_color: Text, active: Model(id: "fixture.second")),
                        ])),
            ]))"#,
    );
    resolver
}

struct Endpoints {
    flag: EndpointDesc,
    press: EndpointDesc,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            flag: EndpointDesc::new(ValueKind::Bool),
            press: EndpointDesc::new(ValueKind::Trigger),
        }
    }
}

impl EndpointRegistry for Endpoints {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        match (category, id.0.as_str()) {
            (EndpointCategory::Model, "fixture.first" | "fixture.second") => Some(&self.flag),
            (EndpointCategory::Command, "fixture.pick") => Some(&self.press),
            _ => None,
        }
    }
}

/// An application that keeps which row stands picked, and answers each row's
/// flag from it. Nothing else about the document moves.
#[derive(Default)]
struct Picked {
    second: bool,
}

impl Reads for Picked {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "fixture.first" => Some(ReadValue::Bool(!self.second)),
            "fixture.second" => Some(ReadValue::Bool(self.second)),
            _ => None,
        }
    }
}

impl App for Picked {
    fn document(&self) -> &str {
        "pick.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, event: UiEvent) {
        if let UiEvent::Control { path, action } = event
            && matches!(action, ControlAction::Activate)
        {
            self.second = path == "demo/two";
        }
    }
}

/// The document mounted in this host, in a window tall enough for both rows.
fn mounted<'a>(endpoints: &'a Endpoints, resolver: &'a MemResolver) -> Ui<'a, Picked> {
    Ui::new(
        Picked::default(),
        Config::builder()
            .endpoints(endpoints)
            .resolver(resolver)
            .text(builtin::text_doc())
            .build(),
        (160, 80),
        1.0,
    )
    .unwrap_or_else(|error| panic!("the pick fixture must mount: {error}"))
}

/// The colour each row's label is written in, first row then second.
fn inks(ui: &Ui<'_, Picked>) -> (Rgba, Rgba) {
    let ink = |path: &str| {
        ui.ink_of(path)
            .unwrap_or_else(|| panic!("the retained host stands no text at {path}"))
    };
    (ink("demo/one-label"), ink("demo/two-label"))
}

/// Presses the second row, through the pointer route a person uses.
fn pick_second(ui: &mut Ui<'_, Picked>) {
    let row = ui
        .rect_of("demo/two-label")
        .expect("the second row must be laid out");
    let at = crate::draw::Pt {
        x: row.x + row.w / 2.0,
        y: row.y + row.h / 2.0,
    };
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

/// The fixture is worth running: the two faces differ, so a row that never
/// followed its flag would be visible.
#[kithara::test]
fn the_document_dresses_a_picked_row_apart_from_the_rest() {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let ui = mounted(&endpoints, &resolver);

    let (picked, rest) = inks(&ui);

    assert_ne!(
        picked, rest,
        "the fixture must dress the picked row apart from the rest"
    );
}

/// The row a press picks is dressed as the picked one.
#[kithara::test]
fn the_row_a_press_picks_takes_the_picked_face() {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let mut ui = mounted(&endpoints, &resolver);
    let (picked, _) = inks(&ui);

    pick_second(&mut ui);

    assert_eq!(
        inks(&ui).1,
        picked,
        "the row a press picked must take the face the picked row wears"
    );
}

/// And the row it leaves goes back to the face of a row that stands unpicked.
#[kithara::test]
fn the_row_a_press_leaves_takes_the_unpicked_face() {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let mut ui = mounted(&endpoints, &resolver);
    let (_, rest) = inks(&ui);

    pick_second(&mut ui);

    assert_eq!(
        inks(&ui).0,
        rest,
        "the row a press left must take the face an unpicked row wears"
    );
}
