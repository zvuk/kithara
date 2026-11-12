//! The low-level retained route an application outside this workspace mounts
//! its own widget on: `MasonryHost::with_custom`, `MasonryRoot`, and its
//! actions out of `MasonryRoot::take_actions`, driven only through public API.

use std::{cell::RefCell, rc::Rc};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Rect},
    ids::{EndpointId, SourceUri},
    interact::{
        Hit, Input, MOUSE, Outcome, PointerButton, PointerId, PointerInput, PointerOwnership,
        PointerPhase,
    },
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry},
    render::{
        CustomSkin, Published, ReadValue, Reads, Skin, document,
        document::{Clock, Ctx},
        masonry::{
            CustomWidget, MasonryHost, MasonryRoot, Repaint, Size2, SizeLimits, TextMeasurer,
        },
    },
    shaping::FontPolicy,
    source::{MemResolver, UiConfig},
    view,
};
use masonry::{
    app::{RenderRootOptions, WindowSizePolicy},
    core::WindowEvent,
    dpi::{PhysicalPosition, PhysicalSize},
    theme::default_property_set,
    ui_events::pointer::{
        PointerButton as MasonryButton, PointerButtonEvent, PointerButtons, PointerEvent,
        PointerId as MasonryPointerId, PointerInfo, PointerState, PointerType, PointerUpdate,
    },
};

/// How long a press is held before the probe reports it as a long press.
const LONG_PRESS: Duration = Duration::from_millis(500);

/// Inside the 40 by 40 custom widget the fixture lays out at the window's left.
const INSIDE: (f64, f64) = (10.0, 50.0);
/// Well clear of the custom widget.
const OUTSIDE: (f64, f64) = (150.0, 50.0);

/// The consumer's own action, carrying a payload the host must not touch.
#[derive(Clone, Debug, PartialEq)]
enum Probe {
    Pressed { by: PointerId, label: String },
    Held { by: PointerId, label: String },
}

#[derive(Debug, PartialEq)]
enum Out {
    Document(Published),
    Probe(Probe),
}

/// A press recognizer that owns one pointer from Down to its end, and only
/// lets that pointer finish what it started.
struct Presser {
    seen: Rc<RefCell<Vec<PointerInput>>>,
    owner: Option<PointerId>,
    held: Duration,
    reported: bool,
}

impl Presser {
    fn new(seen: &Rc<RefCell<Vec<PointerInput>>>) -> Self {
        Self {
            seen: Rc::clone(seen),
            owner: None,
            held: Duration::ZERO,
            reported: false,
        }
    }

    fn release(&mut self) -> Outcome<Probe> {
        self.owner = None;
        self.held = Duration::ZERO;
        self.reported = false;
        Outcome::captured().with_ownership(PointerOwnership::Release)
    }
}

impl CustomWidget for Presser {
    type Action = Probe;

    fn frame(&mut self, elapsed: Duration) -> Option<Probe> {
        let by = self.owner?;
        self.held += elapsed;
        if self.reported || self.held < LONG_PRESS {
            return None;
        }
        self.reported = true;
        Some(Probe::Held {
            by,
            label: format!("held {} ms", self.held.as_millis()),
        })
    }

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Probe> {
        let Input::Pointer(pointer) = input else {
            return Outcome::IGNORED;
        };
        self.seen.borrow_mut().push(pointer);
        match (pointer.phase, self.owner) {
            (PointerPhase::Down, None) if hit.inside().is_some() => {
                self.owner = Some(pointer.id);
                Outcome::captured().with_ownership(PointerOwnership::Claim)
            }
            (PointerPhase::Up | PointerPhase::DoubleClick, Some(owner)) if owner == pointer.id => {
                let finished = !self.reported;
                let _ = self.release();
                if finished {
                    Outcome::set(Probe::Pressed {
                        by: owner,
                        label: format!("{:?} x{}", pointer.button, pointer.clicks),
                    })
                } else {
                    Outcome::captured()
                }
            }
            (PointerPhase::Cancel, Some(owner)) if owner == pointer.id => self.release(),
            (_, Some(_)) => Outcome::captured(),
            (_, None) => Outcome::IGNORED,
        }
    }

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
    }

    fn repaint(&self) -> Repaint {
        if self.owner.is_some() && !self.reported {
            Repaint::Continuous
        } else {
            Repaint::None
        }
    }
}

struct NoEndpoints;

impl EndpointRegistry for NoEndpoints {
    fn endpoint(&self, _category: EndpointCategory, _id: &EndpointId) -> Option<&EndpointDesc> {
        None
    }
}

struct NoReads;

impl Reads for NoReads {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

fn document() -> CompiledUi {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "route.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "route",
            root: Module(instance: "demo", source: "route.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "route.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "route", chrome: Plain,
            root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
            ]))"#,
    );
    compile(
        "route.klayout.ron",
        &resolver,
        &NoEndpoints,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the route fixture must compile: {error}"))
}

fn skin() -> Skin {
    Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        builtin::text_doc(),
        &SourceUri("fixture:custom-route".to_owned()),
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("the route skin must resolve: {error}"))
}

fn mounted(ui: &CompiledUi, skin: &Skin, widget: Presser) -> MasonryRoot<Out> {
    let reads = NoReads;
    let ctx = Ctx::new(
        ui,
        &reads,
        &view::EMPTY,
        builtin::skin_doc(),
        Clock::default(),
    );
    let host = MasonryHost::map_actions(ctx, skin, Out::Document).with_custom(
        "demo/custom",
        widget,
        Out::Probe,
    );
    let node = document::render(&ui.root, ctx, host);
    let mut root = MasonryRoot::new(
        node,
        RenderRootOptions {
            default_properties: Arc::new(default_property_set()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(200, 120),
            scale_factor: 1.0,
            test_font: None,
        },
    )
    .unwrap_or_else(|error| panic!("the route must mount: {error}"));
    root.redraw()
        .unwrap_or_else(|error| panic!("the route must lay out: {error}"));
    root
}

fn info(id: u64) -> PointerInfo {
    let pointer_id = MasonryPointerId::new(id);
    PointerInfo {
        pointer_id,
        persistent_device_id: None,
        pointer_type: if pointer_id.is_some_and(MasonryPointerId::is_primary_pointer) {
            PointerType::Mouse
        } else {
            PointerType::Touch
        },
    }
}

fn state((x, y): (f64, f64), pressed: Option<MasonryButton>, count: u8) -> PointerState {
    let mut buttons = PointerButtons::new();
    if let Some(button) = pressed {
        buttons.insert(button);
    }
    PointerState {
        position: PhysicalPosition::new(x, y),
        buttons,
        count,
        scale_factor: 1.0,
        ..PointerState::default()
    }
}

fn down(id: u64, at: (f64, f64), button: MasonryButton, count: u8) -> PointerEvent {
    PointerEvent::Down(PointerButtonEvent {
        state: state(at, Some(button), count),
        button: Some(button),
        pointer: info(id),
    })
}

fn up(id: u64, at: (f64, f64), button: MasonryButton, count: u8) -> PointerEvent {
    PointerEvent::Up(PointerButtonEvent {
        state: state(at, None, count),
        button: Some(button),
        pointer: info(id),
    })
}

fn moved(id: u64, at: (f64, f64)) -> PointerEvent {
    PointerEvent::Move(PointerUpdate {
        pointer: info(id),
        current: state(at, None, 1),
        coalesced: Vec::new(),
        predicted: Vec::new(),
    })
}

fn send(root: &mut MasonryRoot<Out>, events: impl IntoIterator<Item = PointerEvent>) {
    for event in events {
        root.handle_pointer_event(event)
            .unwrap_or_else(|error| panic!("pointer input must remain typed: {error}"));
    }
}

fn identities(seen: &Rc<RefCell<Vec<PointerInput>>>) -> Vec<(PointerId, PointerPhase)> {
    seen.borrow()
        .iter()
        .map(|input| (input.id, input.phase))
        .collect()
}

/// Masonry numbers its pointers from one, the primary pointer being one; the
/// neutral model numbers them from zero, the mouse being zero.
#[kithara::test]
fn every_pointer_reaches_the_widget_under_its_own_identity() {
    let (ui, skin) = (document(), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, Presser::new(&seen));

    send(
        &mut root,
        [
            down(1, INSIDE, MasonryButton::Primary, 1),
            up(1, INSIDE, MasonryButton::Primary, 1),
            down(7, INSIDE, MasonryButton::Primary, 1),
            up(7, INSIDE, MasonryButton::Primary, 1),
        ],
    );

    assert_eq!(
        identities(&seen),
        [
            (MOUSE, PointerPhase::Down),
            (MOUSE, PointerPhase::Up),
            (PointerId(6), PointerPhase::Down),
            (PointerId(6), PointerPhase::Up),
        ],
    );
    assert_eq!(
        root.take_actions(),
        [
            Out::Probe(Probe::Pressed {
                by: MOUSE,
                label: "Some(Primary) x1".to_owned(),
            }),
            Out::Probe(Probe::Pressed {
                by: PointerId(6),
                label: "Some(Primary) x1".to_owned(),
            }),
        ],
        "each press must leave as the consumer's own action, payload intact"
    );
}

#[kithara::test]
fn buttons_and_click_counts_arrive_as_the_platform_reported_them() {
    let (ui, skin) = (document(), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, Presser::new(&seen));

    send(
        &mut root,
        [
            down(4, INSIDE, MasonryButton::Secondary, 1),
            up(4, INSIDE, MasonryButton::Secondary, 1),
            down(4, INSIDE, MasonryButton::Primary, 2),
            up(4, INSIDE, MasonryButton::Primary, 2),
        ],
    );

    let seen = seen
        .borrow()
        .iter()
        .map(|input| (input.id, input.phase, input.button, input.clicks))
        .collect::<Vec<_>>();
    assert_eq!(
        seen,
        [
            (
                PointerId(3),
                PointerPhase::Down,
                Some(PointerButton::Secondary),
                1
            ),
            (
                PointerId(3),
                PointerPhase::Up,
                Some(PointerButton::Secondary),
                1
            ),
            (
                PointerId(3),
                PointerPhase::Down,
                Some(PointerButton::Primary),
                2
            ),
            (
                PointerId(3),
                PointerPhase::DoubleClick,
                Some(PointerButton::Primary),
                2
            ),
        ],
    );
    assert_eq!(
        root.take_actions(),
        [
            Out::Probe(Probe::Pressed {
                by: PointerId(3),
                label: "Some(Secondary) x1".to_owned(),
            }),
            Out::Probe(Probe::Pressed {
                by: PointerId(3),
                label: "Some(Primary) x2".to_owned(),
            }),
        ],
    );
}

/// The widget that claimed a pointer keeps it until that pointer is
/// cancelled; a second pointer cannot finish the first one's press, and once
/// the cancel ends the capture, input elsewhere no longer reaches the widget.
#[kithara::test]
fn a_cancelled_capture_ends_and_no_other_pointer_completes_it() {
    let (ui, skin) = (document(), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, Presser::new(&seen));

    send(
        &mut root,
        [
            down(3, INSIDE, MasonryButton::Primary, 1),
            moved(3, OUTSIDE),
            moved(5, OUTSIDE),
            PointerEvent::Cancel(info(3)),
            down(5, OUTSIDE, MasonryButton::Primary, 1),
            up(5, OUTSIDE, MasonryButton::Primary, 1),
        ],
    );

    assert_eq!(
        identities(&seen),
        [
            (PointerId(2), PointerPhase::Down),
            (PointerId(2), PointerPhase::Move),
            (PointerId(4), PointerPhase::Move),
            (PointerId(2), PointerPhase::Cancel),
        ],
        "the captured widget sees both pointers while it holds one, then nothing off its box"
    );
    assert!(
        root.take_actions().is_empty(),
        "neither a cancel nor a foreign pointer completes the press"
    );
}

#[kithara::test]
fn a_held_press_reports_its_long_press_from_the_frame_loop() {
    let (ui, skin) = (document(), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, Presser::new(&seen));

    send(&mut root, [down(2, INSIDE, MasonryButton::Primary, 1)]);
    for _ in 0..4 {
        root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(200)))
            .unwrap_or_else(|error| panic!("frames must remain typed: {error}"));
    }
    send(&mut root, [up(2, INSIDE, MasonryButton::Primary, 1)]);

    assert_eq!(
        root.take_actions(),
        [Out::Probe(Probe::Held {
            by: PointerId(1),
            label: "held 600 ms".to_owned(),
        })],
        "the long press leaves once, from the frame that crossed the threshold, and its release completes nothing"
    );
}
