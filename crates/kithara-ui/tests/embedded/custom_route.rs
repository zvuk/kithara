//! The low-level retained route an application outside this workspace mounts
//! its own widget on: `MasonryHost::with_custom`, `MasonryRoot`, and its
//! actions out of `MasonryRoot::take_actions`, driven only through public API.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Pt, Rect},
    ids::SourceUri,
    interact::{
        Hit, Input, MOUSE, Outcome, PointerButton, PointerId, PointerInput, PointerOwnership,
        PointerPhase,
    },
    module::Pose,
    registry::{EndpointCategory, EndpointDesc, MapEndpoints, ValueKind},
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

/// The one endpoint the fixture reads: how far a driven object has travelled
/// toward its far pose.
const SQUASH: &str = "fixture.squash";

#[derive(Default)]
struct Squash {
    phase: Cell<f64>,
}

impl Reads for Squash {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        (endpoint == SQUASH).then(|| ReadValue::Scalar(self.phase.get()))
    }
}

fn ctx<'a>(ui: &'a CompiledUi, reads: &'a Squash) -> Ctx<'a, 'a> {
    Ctx::new(
        ui,
        reads,
        &view::EMPTY,
        builtin::skin_doc(),
        Clock::default(),
    )
}

/// The custom widget's slot, standing at the window's left edge.
const SLOT: &str = r#"Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0))))"#;

/// A module whose one row holds `custom`, the document node the widget mounts
/// in place of.
fn document(custom: &str) -> CompiledUi {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "route.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "route",
            root: Module(instance: "demo", source: "route.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "route.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "route", chrome: Plain,
            root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{custom}]))"#
        ),
    );
    let mut endpoints = MapEndpoints::default();
    endpoints.insert(
        EndpointCategory::Model,
        SQUASH,
        EndpointDesc::new(ValueKind::Scalar),
    );
    compile(
        "route.klayout.ron",
        &resolver,
        &endpoints,
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

fn mounted<Widget>(ui: &CompiledUi, skin: &Skin, reads: &Squash, widget: Widget) -> MasonryRoot<Out>
where
    Widget: CustomWidget<Action = Probe>,
{
    let ctx = ctx(ui, reads);
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
    let (ui, skin) = (document(SLOT), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, &Squash::default(), Presser::new(&seen));

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
    let (ui, skin) = (document(SLOT), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, &Squash::default(), Presser::new(&seen));

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
    let (ui, skin) = (document(SLOT), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, &Squash::default(), Presser::new(&seen));

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
    let (ui, skin) = (document(SLOT), skin());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, &Squash::default(), Presser::new(&seen));

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

/// Where the identity-posed slot's box stands in the window: the row centres
/// its one 40 by 40 child in the window's 120-pixel height.
const ORIGIN: Pt = Pt { x: 0.0, y: 40.0 };

/// Records where, in its own space, the widget was pressed.
struct Spot {
    pressed: Rc<RefCell<Vec<Pt>>>,
}

impl CustomWidget for Spot {
    type Action = Probe;

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Probe> {
        match (input, hit.inside()) {
            (
                Input::Pointer(PointerInput {
                    phase: PointerPhase::Down,
                    ..
                }),
                Some(at),
            ) => {
                self.pressed.borrow_mut().push(at);
                Outcome::captured()
            }
            _ => Outcome::IGNORED,
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
}

fn posed(pose: &Pose) -> String {
    format!(
        "Object(id: \"posed\", transform: (position: ({:?}, {:?}), anchor: ({:?}, {:?}), \
         scale: ({:?}, {:?}), rotation: {:?}), child: {SLOT})",
        pose.position.0,
        pose.position.1,
        pose.anchor.0,
        pose.anchor.1,
        pose.scale.0,
        pose.scale.1,
        pose.rotation,
    )
}

fn pose(position: (f32, f32), anchor: (f32, f32), scale: (f32, f32), rotation: f32) -> Pose {
    let mut pose = Pose::default();
    pose.position = position;
    pose.anchor = anchor;
    pose.scale = scale;
    pose.rotation = rotation;
    pose
}

/// A point the widget painted at `local` is pressed where the pose put it on
/// screen, worked out forward from the document's own pose; the widget must
/// be handed `local` back. The moved case lands wholly outside the box the
/// row laid out, which only reaches the widget if routing follows the pose.
#[kithara::test]
#[case::unposed(pose((0.0, 0.0), (0.0, 0.0), (1.0, 1.0), 0.0))]
#[case::moved(pose((60.0, -20.0), (0.0, 0.0), (1.0, 1.0), 0.0))]
#[case::scaled(pose((0.0, 0.0), (20.0, 20.0), (2.0, 0.5), 0.0))]
#[case::turned(pose((0.0, 0.0), (20.0, 20.0), (1.0, 1.0), 90.0))]
#[case::all_at_once(pose((15.0, 5.0), (10.0, 30.0), (1.5, 1.5), 30.0))]
fn a_posed_widget_is_hit_where_it_was_painted(#[case] pose: Pose) {
    const LOCAL: Pt = Pt { x: 30.0, y: 10.0 };
    let (ui, skin) = (document(&posed(&pose)), skin());
    let pressed = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(
        &ui,
        &skin,
        &Squash::default(),
        Spot {
            pressed: Rc::clone(&pressed),
        },
    );
    let shown = pose.matrix().apply(LOCAL);
    let at = (f64::from(ORIGIN.x + shown.x), f64::from(ORIGIN.y + shown.y));

    send(&mut root, [down(1, at, MasonryButton::Primary, 1)]);

    let pressed = pressed.borrow();
    assert_eq!(
        pressed.len(),
        1,
        "the press at {at:?} must reach the widget"
    );
    assert!(
        (pressed[0].x - LOCAL.x).abs() < 1.0e-3 && (pressed[0].y - LOCAL.y).abs() < 1.0e-3,
        "the widget must be pressed at {LOCAL:?}, the point it painted there, not {:?}",
        pressed[0]
    );
}

/// A pose that flattens the widget onto a line has no inverse: no point on
/// screen names one point of the widget. The host says so rather than handing
/// the widget a point it did not paint.
#[kithara::test]
fn a_pose_without_an_inverse_is_refused_rather_than_hit() {
    let flat = pose((0.0, 0.0), (20.0, 20.0), (0.0, 1.0), 0.0);
    let (ui, skin) = (document(&posed(&flat)), skin());
    let pressed = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(
        &ui,
        &skin,
        &Squash::default(),
        Spot {
            pressed: Rc::clone(&pressed),
        },
    );

    send(
        &mut root,
        [
            down(1, (20.0, 60.0), MasonryButton::Primary, 1),
            down(1, (10.0, 50.0), MasonryButton::Primary, 1),
        ],
    );

    assert!(
        pressed.borrow().is_empty(),
        "a flattened widget must not be pressed"
    );
    assert!(
        kithara_test_utils::flight::tail()
            .iter()
            .any(|line| line.contains("demo/custom") && line.contains("no inverse")),
        "the refusal must be reported, naming the widget"
    );
}

/// The pose can also go flat under a gesture already held. The widget is not
/// handed the pointer through a transform that no longer names a point of it:
/// it is told its gesture was cancelled, and the capture ends there.
#[kithara::test]
fn a_held_gesture_is_cancelled_when_its_pose_goes_flat() {
    let (ui, skin) = (
        document(&format!(
            "Object(id: \"squashed\", transform: (anchor: (20.0, 20.0)), \
             to: (anchor: (20.0, 20.0), scale: (0.0, 1.0)), \
             phase: Model(id: \"{SQUASH}\"), child: {SLOT})"
        )),
        skin(),
    );
    let reads = Squash::default();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut root = mounted(&ui, &skin, &reads, Presser::new(&seen));

    send(&mut root, [down(3, INSIDE, MasonryButton::Primary, 1)]);
    reads.phase.set(1.0);
    root.refresh(ctx(&ui, &reads));
    send(
        &mut root,
        [
            moved(3, INSIDE),
            moved(3, OUTSIDE),
            up(3, OUTSIDE, MasonryButton::Primary, 1),
        ],
    );

    assert_eq!(
        identities(&seen),
        [
            (PointerId(2), PointerPhase::Down),
            (PointerId(2), PointerPhase::Cancel),
        ],
        "the held gesture must end once, as cancelled, and nothing after it may reach the widget"
    );
    assert!(
        root.take_actions().is_empty(),
        "a cancelled gesture completes nothing"
    );
}
