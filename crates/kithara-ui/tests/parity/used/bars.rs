use iced::{
    Point as IcedPoint, Rectangle, Size, Vector,
    advanced::{
        layout::{Layout, Limits},
        widget::Tree,
    },
};
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::{CompiledUi, compile},
    draw::{Pt, Rect},
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    render::{Clock, ControlAction, ReadValue, Reads, Skin, UiEvent, tree},
    source::{MemResolver, UiConfig},
    view,
};
use num_traits::cast::AsPrimitive;

use crate::shared::{Endpoints, collect_rows, renderer};

/// The shape of the document below: the bars it hangs a menu on, the rows each
/// menu is made of, and the windows the two hosts are compared at.
struct Fixture;

impl Fixture {
    /// Every bar the document hangs a menu on.
    const BARS: [&'static str; 2] = ["narrow", "wide"];
    /// A window short enough that only the narrow bar's band is reached and one
    /// tall enough that only the wide bar's is, each carrying the bar the room
    /// reaches there.
    const CASES: [(u32, u32, &'static str); 2] = [(400, 240, "narrow"), (400, 480, "wide")];
    /// The rows the menu is made of, in the order the document holds them.
    const ROWS: [&'static str; 2] = ["head", "item"];
}

/// A document that hangs one menu on two bars and reaches whichever the window
/// is tall enough for, the way the shipped app bar carries a wide strip and a
/// narrow one.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "bars.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "bars",
            root: Split(axis: Vertical, measure: Height, size: (w: Fill, h: Fill), children: [
                (until: Some(300.0), node: Module(instance: "narrow", source: "bar.kmodule.ron",
                    size: (w: Fill, h: Fixed(42.0)))),
                (from: 300.0, node: Module(instance: "wide", source: "bar.kmodule.ron",
                    size: (w: Fill, h: Fixed(42.0)))),
                (weight: 1.0, node: Module(instance: "body", source: "body.kmodule.ron",
                    size: (w: Fill, h: Fill))),
            ]))"#,
    );
    resolver.insert(
        "bar.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "bar", chrome: Plain,
            root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Popover(id: "pop", open: Model(id: "fixture.menu"), align: Start,
                    anchor: Pressable(id: "burger", press: Command(id: "fixture.toggle"),
                        child: Spacer(id: "anchor", size: Some((w: Fixed(40.0), h: Fixed(42.0))))),
                    content: Column(id: "surface", size: (w: Fixed(180.0), h: Shrink), gap: 0.0,
                        children: [
                            Spacer(id: "head", size: Some((w: Fill, h: Fixed(26.0)))),
                            Spacer(id: "item", size: Some((w: Fill, h: Fixed(24.0)))),
                        ])),
            ]))"#,
    );
    resolver.insert(
        "body.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "body", chrome: Plain,
            root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: []))"#,
    );
    resolver
}

/// An application whose one flag every bar's menu reads, so a press on the
/// burger that stands opens as many menus as the document holds.
#[derive(Default)]
struct Bars {
    open: bool,
}

impl Reads for Bars {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _)| id);
        (id == "fixture.menu").then_some(ReadValue::Bool(self.open))
    }
}

impl App for Bars {
    fn document(&self) -> &str {
        "bars.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, event: UiEvent) {
        if let UiEvent::Control { action, .. } = event
            && action == ControlAction::Activate
        {
            self.open = !self.open;
        }
    }
}

fn compiled() -> CompiledUi {
    compile(
        "bars.klayout.ron",
        &documents(),
        &Endpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the bar fixture must compile: {error}"))
}

/// Every menu the retained host has standing, each one holding the boxes its
/// rows were laid out into.
///
/// The menu is opened by pressing the burger the room reached, so the boxes
/// below are the boxes a person sees after using the bar rather than a state
/// the test declared.
fn retained_menus(standing: &str, width: u32, height: u32) -> Vec<Vec<Rect>> {
    let endpoints = Endpoints::default();
    let resolver = documents();
    let mut ui = Ui::new(
        Bars::default(),
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (width, height),
        1.0,
    )
    .unwrap_or_else(|error| panic!("the bar fixture must mount: {error}"));
    let anchor = ui
        .rect_of(&format!("{standing}/anchor"))
        .expect("the bar the room reached must be laid out");
    let at = Pt {
        x: anchor.x + anchor.w / 2.0,
        y: anchor.y + anchor.h / 2.0,
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
    assert!(
        ui.app().open,
        "the press on the burger that stands must open the menu"
    );
    ui.scene()
        .unwrap_or_else(|error| panic!("the retained host must draw the open menu: {error}"));
    Fixture::BARS
        .iter()
        .map(|bar| {
            Fixture::ROWS
                .iter()
                .filter_map(|row| ui.rect_of(&format!("{bar}/{row}")))
                .filter(|rect| rect.w > 0.0 && rect.h > 0.0)
                .collect::<Vec<Rect>>()
        })
        .filter(|menu| !menu.is_empty())
        .collect()
}

/// Every menu the immediate host has standing, read from the overlay it hands
/// its renderer, each one holding the boxes its rows were laid out into.
///
/// This host has no pointer of its own here: it reads the same flag the press
/// on the other host set, and builds its whole tree from that reading.
fn neutral_menus(width: u32, height: u32) -> Vec<Vec<Rect>> {
    let ui = compiled();
    let reads = Bars { open: true };
    let renderer = renderer();
    let viewport = Size::new(width.as_(), height.as_());
    let mut element = tree::render(
        &ui.root,
        &ui,
        &reads,
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
    let bounds = Rectangle::new(IcedPoint::ORIGIN, viewport);
    let Some(mut overlay) = element.as_widget_mut().overlay(
        &mut state,
        Layout::new(&node),
        &renderer,
        &bounds,
        Vector::ZERO,
    ) else {
        return Vec::new();
    };
    let node = overlay.as_overlay_mut().layout(&renderer, viewport);
    let mut menus = Vec::new();
    collect_menus(Layout::new(&node), viewport, &mut menus);
    menus
}

/// Gathers each surface the overlay put on top of the document.
///
/// The overlay a document hands over is a nest of groups, one for every level
/// that forwarded it, and each of those takes the whole window. A surface is
/// the first node under them that asked for a box of its own.
fn collect_menus(layout: Layout<'_>, viewport: Size, menus: &mut Vec<Vec<Rect>>) {
    for child in layout.children() {
        if fills(child.bounds(), viewport) {
            collect_menus(child, viewport, menus);
            continue;
        }
        let mut rows = Vec::new();
        collect_rows(child, &mut rows);
        menus.push(rows);
    }
}

/// The boxes the leaves of one surface were laid out into, in document order.
fn fills(bounds: Rectangle, viewport: Size) -> bool {
    bounds.width == viewport.width && bounds.height == viewport.height
}

/// A menu that stands on one host has to stand on the other.
///
/// One document can hang the same menu on two bars and reach whichever the
/// window is tall enough for. The host that throws its tree away every frame
/// only ever builds the bar it reached; the host that keeps its tree mounts
/// both and stands one aside, and a surface hanging on the bar that stands
/// aside would open on the one flag they share.
#[kithara::test]
fn both_hosts_stand_the_same_number_of_menus() {
    for (width, height, standing) in Fixture::CASES {
        assert_eq!(
            retained_menus(standing, width, height).len(),
            neutral_menus(width, height).len(),
            "at {width}x{height} the two hosts disagree on how many menus one press opens"
        );
    }
}

/// An opened menu lands in the same boxes on both hosts.
#[kithara::test]
fn both_hosts_lay_an_opened_menu_out_the_same_way() {
    for (width, height, standing) in Fixture::CASES {
        assert_eq!(
            retained_menus(standing, width, height),
            neutral_menus(width, height),
            "at {width}x{height} the menu opened from the `{standing}` bar was laid out \
             differently by the two hosts"
        );
    }
}
