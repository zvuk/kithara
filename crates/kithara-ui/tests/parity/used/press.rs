//! What the two hosts do with one and the same press.
//!
//! The rest of this module compares the boxes a document is laid out into. A
//! press is a second thing the two owe each other and neither the rect corpus
//! nor a photograph can see: the hosts route it through machinery with nothing
//! in common, one against boxes read out of a tree it keeps and the other by
//! letting iced walk a tree it rebuilt, and a press that reaches a different
//! control on one host than on the other draws exactly the same picture.

use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::compile,
    draw::Pt,
    ids::EndpointId,
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ControlAction, ReadValue, Reads, Skin, UiEvent},
    source::{MemResolver, UiConfig},
    view,
};

use crate::immediate::Immediate;

/// A burger menu hanging over a control of the page.
///
/// The surface a menu opens is drawn above the document, and the document goes
/// on laying controls out under it. The page control here is exactly the one a
/// menu row covers, which is the arrangement both shipped menus - the burger
/// and the quality picker - stand in.
const OVER_THE_PAGE: &str = r#"Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
    Popover(id: "menu", open: View(id: "menu"), align: Start,
        anchor: Pressable(id: "burger", press: View(id: "menu"),
            child: Spacer(id: "anchor", size: Some((w: Fixed(40.0), h: Fixed(20.0))))),
        content: Pressable(id: "item", press: Command(id: "fixture.pick"),
            child: Spacer(id: "item-face", size: Some((w: Fixed(100.0), h: Fixed(26.0)))))),
    Pressable(id: "page", press: Command(id: "fixture.page"),
        child: Spacer(id: "page-face", size: Some((w: Fill, h: Fill)))),
])"#;

/// The window both hosts open the document in.
const WINDOW: (u32, u32) = (240, 160);

/// The state the document names for the menu it opens and shuts.
const MENU: &str = "demo/menu";

/// What one gesture left behind: where each press landed, what the document
/// published, and whether the menu stands open at the end of it.
struct Played {
    points: Vec<Pt>,
    published: Vec<UiEvent>,
    open: bool,
}

/// An application that answers nothing and keeps every event published to it:
/// what a press reaches here is the host's own doing, which is the question.
#[derive(Default)]
struct Menu {
    published: Vec<UiEvent>,
}

impl Reads for Menu {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

impl App for Menu {
    fn document(&self) -> &str {
        "menu.klayout.ron"
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

struct Commands {
    press: EndpointDesc,
}

impl Default for Commands {
    fn default() -> Self {
        Self {
            press: EndpointDesc::new(ValueKind::Trigger),
        }
    }
}

impl EndpointRegistry for Commands {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        match (category, id.0.as_str()) {
            (EndpointCategory::Command, "fixture.pick" | "fixture.page") => Some(&self.press),
            _ => None,
        }
    }
}

/// The document, in the layout and module both hosts open it through.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "menu.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "menu",
            root: Module(instance: "demo", source: "menu.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "menu.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "menu", chrome: Plain,
                root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0,
                    children: [{OVER_THE_PAGE}]))"#
        ),
    );
    resolver
}

/// Plays the gesture through the retained host, answering with the point each
/// press landed on as well as what it published.
///
/// The points come from here because only this host can be asked where a
/// control stands. Pressing the other one at those same points is what makes
/// the two answers comparable; whether the two agree on where a control stands
/// is a separate question the rect corpus already asks.
fn retained(steps: &[&str]) -> Played {
    let (endpoints, resolver) = (Commands::default(), documents());
    let mut ui = Ui::new(
        Menu::default(),
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        WINDOW,
        1.0,
    )
    .unwrap_or_else(|error| panic!("the menu fixture must mount: {error}"));
    let mut points = Vec::with_capacity(steps.len());
    for path in steps {
        let rect = ui
            .rect_of(path)
            .unwrap_or_else(|| panic!("the retained host stands no control at {path}"));
        let at = Pt {
            x: rect.x + rect.w / 2.0,
            y: rect.y + rect.h / 2.0,
        };
        points.push(at);
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
    Played {
        points,
        open: ui.view().flag(MENU),
        published: ui.app().published.clone(),
    }
}

/// Plays the same gesture, point for point, through the immediate host.
fn immediate(points: &[Pt]) -> Played {
    let (endpoints, resolver) = (Commands::default(), documents());
    let ui = compile(
        "menu.klayout.ron",
        &resolver,
        &endpoints,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("both hosts open the same document: {error}"));
    let mut host = Immediate::mount(Menu::default(), &ui, builtin::skin(), WINDOW);
    for point in points {
        host.click_at(*point);
    }
    Played {
        open: host.view().flag(MENU),
        published: host.app().published.clone(),
        points: points.to_vec(),
    }
}

/// A menu is opened by pressing its burger and a row of it by pressing the row.
/// Both hosts draw the surface over a control of the page, and a host that
/// answers with the page underneath leaves the whole menu inert.
#[kithara::test]
fn both_hosts_answer_a_press_in_an_open_menu_with_the_row_it_landed_on() {
    let retained = retained(&["demo/anchor", "demo/item-face"]);
    let immediate = immediate(&retained.points);

    assert_eq!(
        immediate.published, retained.published,
        "the two hosts disagree on what a press inside an open menu reaches, at {:?}",
        retained.points
    );
}

/// The row under the press is the row that activates, whichever host is asked.
#[kithara::test]
fn a_press_in_an_open_menu_activates_the_row_it_landed_on() {
    let retained = retained(&["demo/anchor", "demo/item-face"]);

    assert!(
        retained.published.contains(&UiEvent::Control {
            path: "demo/item".to_owned(),
            action: ControlAction::Activate,
        }),
        "the retained host published {:?} for a press at {:?}",
        retained.published,
        retained.points
    );
}

/// The control that opens a surface is the control that shuts it, whichever
/// way each host routes the second press.
#[kithara::test]
fn both_hosts_shut_the_menu_when_the_burger_is_pressed_again() {
    let retained = retained(&["demo/anchor", "demo/anchor"]);
    let immediate = immediate(&retained.points);

    assert_eq!(
        immediate.open, retained.open,
        "the two hosts disagree on whether the menu still stands, published {:?} and {:?}",
        retained.published, immediate.published
    );
}

/// A second press on the burger shuts the menu it opened.
#[kithara::test]
fn a_second_press_on_the_burger_shuts_the_menu() {
    let retained = retained(&["demo/anchor", "demo/anchor"]);

    assert!(
        !retained.open,
        "the retained host published {:?}",
        retained.published
    );
}

/// A press on the page, with nothing standing over it, is the page's own.
#[kithara::test]
fn both_hosts_answer_a_press_on_the_bare_page_with_the_page_control() {
    let retained = retained(&["demo/page-face"]);
    let immediate = immediate(&retained.points);

    assert_eq!(
        immediate.published, retained.published,
        "the two hosts disagree on what a press on the bare page reaches, at {:?}",
        retained.points
    );
}
