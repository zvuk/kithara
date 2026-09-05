use iced::{
    Size,
    advanced::{
        layout::{Layout, Limits},
        widget::Tree,
    },
};
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;

use super::shared::{Endpoints, collect_rows, renderer};
use crate::{
    app::{App, Config, Ui},
    builtin,
    compile::compile,
    draw::{Pt, Rect},
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    render::{Clock, ControlAction, ReadValue, Reads, Skin, UiEvent, tree},
    source::{MemResolver, UiConfig},
    view,
};

/// The shape of the documents below, and the window both hosts are given.
struct Consts;

impl Consts {
    /// The room the window leaves, wide and tall enough for every block.
    const CASE: (u32, u32) = (300, 240);
    /// Every leaf the flow document lays out, in the order it holds them: a
    /// row of the flow, the block that flow hides, the row after it, and the
    /// cell the split hides beside them.
    const FLOW_LEAVES: [&'static str; 4] = ["flow/head", "flow/body", "flow/tail", "aside/aside"];
    /// The same three leaves of the slot document, the middle one held by a
    /// slot rather than by the column itself.
    const SLOT_LEAVES: [&'static str; 3] = ["well/head", "well/body", "well/tail"];
}

/// One document, and the leaves it lays out in the order it holds them.
#[derive(Clone, Copy)]
struct Case {
    document: &'static str,
    leaves: &'static [&'static str],
}

impl Case {
    /// A block held by the flow that draws it, and one held by a split.
    const FLOW: Self = Self {
        document: "blocks.klayout.ron",
        leaves: &Consts::FLOW_LEAVES,
    };
    /// A block held by a slot, which is a flow the facade used to speak for.
    const SLOT: Self = Self {
        document: "slot.klayout.ron",
        leaves: &Consts::SLOT_LEAVES,
    };
}

/// A document that hides a child of a flow and a cell of a split behind the
/// same flag, the way the shipped menu hides a group and the shipped layout
/// hides a module.
///
/// The slot stands its children in the middle of the room it is given, so its
/// heights leave an even number of pixels free: the retained toolkit snaps
/// every child origin to a whole pixel and the immediate one does not, and a
/// half-pixel centre would measure that rather than where the block stands.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "blocks.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "blocks",
            root: Split(axis: Horizontal, size: (w: Fill, h: Fill), children: [
                (weight: 1.0, node: Module(instance: "flow", source: "flow.kmodule.ron",
                    size: (w: Fill, h: Fill))),
                (node: Optional(id: "aside-block", hidden: Model(id: "fixture.hidden"),
                    node: Module(instance: "aside", source: "aside.kmodule.ron",
                        size: (w: Fixed(80.0), h: Fill)))),
            ]))"#,
    );
    resolver.insert(
        "flow.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "flow", chrome: Plain,
            root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Pressable(id: "open", press: Command(id: "fixture.toggle"),
                    child: Spacer(id: "head", size: Some((w: Fill, h: Fixed(26.0))))),
                Optional(id: "body-block", hidden: Model(id: "fixture.hidden"),
                    child: Spacer(id: "body", size: Some((w: Fill, h: Fixed(40.0))))),
                Spacer(id: "tail", size: Some((w: Fill, h: Fixed(26.0)))),
            ]))"#,
    );
    resolver.insert(
        "slot.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "slot",
            root: Module(instance: "well", source: "well.kmodule.ron",
                size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "well.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "well", chrome: Plain,
            root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Pressable(id: "open", press: Command(id: "fixture.toggle"),
                    child: Spacer(id: "head", size: Some((w: Fill, h: Fixed(26.0))))),
                Slot(id: "held", size: (w: Fill, h: Fill), default: [
                    Optional(id: "body-block", hidden: Model(id: "fixture.hidden"),
                        child: Spacer(id: "body", size: Some((w: Fill, h: Fixed(40.0))))),
                    Spacer(id: "tail", size: Some((w: Fill, h: Fixed(27.0)))),
                ]),
            ]))"#,
    );
    resolver.insert(
        "aside.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "aside", chrome: Plain,
            root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Spacer(id: "aside", size: Some((w: Fill, h: Fill))),
            ]))"#,
    );
    resolver
}

/// An application that hides every block until something presses the row that
/// shows them.
struct Blocks {
    document: &'static str,
    shown: bool,
}

impl Blocks {
    /// The application as a window first shows it: every block hidden.
    const fn hidden(case: Case) -> Self {
        Self {
            document: case.document,
            shown: false,
        }
    }
}

impl Reads for Blocks {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _)| id);
        (id == "fixture.hidden").then_some(ReadValue::Bool(!self.shown))
    }
}

impl App for Blocks {
    fn document(&self) -> &str {
        self.document
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
            self.shown = !self.shown;
        }
    }
}

/// The boxes the retained host laid the document's leaves into once a press
/// has shown the blocks.
///
/// The blocks are shown by pressing the row the document names for it, so the
/// boxes below are the ones a person sees after using the document rather than
/// a state the test declared.
fn retained(case: Case) -> Vec<Rect> {
    let endpoints = Endpoints::default();
    let resolver = documents();
    let (width, height) = Consts::CASE;
    let mut ui = Ui::new(
        Blocks::hidden(case),
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (width, height),
        1.0,
    )
    .unwrap_or_else(|error| panic!("the block fixture must mount: {error}"));
    let head = ui
        .rect_of(case.leaves[0])
        .expect("the row that shows the blocks must be laid out");
    let at = Pt {
        x: head.x + head.w / 2.0,
        y: head.y + head.h / 2.0,
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
        ui.app().shown,
        "the press on the row must show the blocks the document hides"
    );
    ui.scene()
        .unwrap_or_else(|error| panic!("the retained host must draw the shown blocks: {error}"));
    case.leaves
        .iter()
        .filter_map(|leaf| ui.rect_of(leaf))
        .filter(|rect| rect.w > 0.0 && rect.h > 0.0)
        .collect()
}

/// The boxes the immediate host laid the same leaves into, reading the flag the
/// press on the other host set.
///
/// This host has no pointer of its own here: it builds its whole tree from that
/// reading, which is exactly what makes a block it never mounts invisible.
fn neutral(case: Case) -> Vec<Rect> {
    let ui = compile(
        case.document,
        &documents(),
        &Endpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the block fixture must compile: {error}"));
    let (width, height) = Consts::CASE;
    let renderer = renderer();
    let viewport = Size::new(width.as_(), height.as_());
    let mut element = tree::render(
        &ui.root,
        &ui,
        &Blocks {
            document: case.document,
            shown: true,
        },
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
    let mut rows = Vec::new();
    collect_rows(Layout::new(&node), &mut rows);
    rows
}

/// A block shown by a press stands in the same box on both hosts.
///
/// The host that throws its tree away every frame builds the block the moment
/// the document stops hiding it. The host that keeps its tree has to mount the
/// block while it is hidden and show it in place, because a block missing from
/// a mounted tree could never come back.
#[kithara::test]
fn both_hosts_lay_a_shown_block_out_the_same_way() {
    assert_eq!(
        retained(Case::FLOW),
        neutral(Case::FLOW),
        "the two hosts disagree on where the blocks a press showed stand"
    );
}

/// A slot holds a block the same way, because a slot is a flow.
///
/// The facade used to answer the hiding question for the slot itself, which
/// left the retained host no block to show: the child was never mounted, and
/// nothing short of rebuilding the document could bring it back.
#[kithara::test]
fn both_hosts_lay_a_block_a_slot_holds_out_the_same_way() {
    assert_eq!(
        retained(Case::SLOT),
        neutral(Case::SLOT),
        "the two hosts disagree on where the block a slot holds stands after a press showed it"
    );
}
