use std::{cell::RefCell, fs, rc::Rc, sync::Arc};

use kithara_platform::time::Duration;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Paint, Pt, Rect, Rgba, Stop, Stops, Transform},
    ids::SourceUri,
    interact::{
        Hit, Input, Outcome, PointerButton, PointerId, PointerInput, PointerOwnership, PointerPhase,
    },
    module::Pose,
    registry::MapEndpoints,
    render::{
        CustomSkin, Published, ReadValue, Reads, Skin, document,
        document::{Clock, Ctx},
        masonry::{
            CustomWidget, MasonryHost, MasonryRoot, Repaint, Size2, SizeLimits, TextMeasurer,
        },
    },
    shaping::{FontPolicy, GlyphFace, GlyphRun},
    skin::{TextRoleSkin, parse_skin_over},
    source::{MemResolver, UiConfig},
    view,
};
use masonry::{
    app::{RenderRootOptions, WindowSizePolicy},
    core::WindowEvent,
    dpi::{PhysicalPosition, PhysicalSize},
    theme::default_property_set,
    ui_events::pointer::{
        PointerButton as Button, PointerButtonEvent, PointerButtons, PointerEvent,
        PointerId as NativeId, PointerInfo, PointerState, PointerType,
    },
};

const FONT: &str = env!(
    "EXTERNAL_UI_FONT",
    "set EXTERNAL_UI_FONT to the supplied font file"
);
const SLOT: &str = "Spacer(id: \"custom\", size: Some((w: Fixed(40.0), h: Fixed(40.0))))";
const WORDS: &str = "\u{0426}\u{0430}\u{0440}\u{044c}";

#[derive(Debug, PartialEq)]
enum Payload {
    Tap {
        by: PointerId,
        cell: u32,
        button: Option<PointerButton>,
        clicks: u8,
    },
    Held {
        by: PointerId,
        cell: u32,
    },
}
#[derive(Debug, PartialEq)]
enum Out {
    Document(Published),
    Consumer(Payload),
}
#[derive(Default)]
struct Trace {
    level: u32,
    frames: u32,
    painted: Vec<u32>,
    runs: Vec<GlyphRun>,
    input: Vec<PointerInput>,
    hits: Vec<Pt>,
}
struct Widget {
    trace: Rc<RefCell<Trace>>,
    role: TextRoleSkin,
    owner: Option<PointerId>,
    elapsed: Duration,
    reported: bool,
}

impl CustomWidget for Widget {
    type Action = Payload;
    fn measure(&mut self, _: &mut TextMeasurer<'_>, _: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }
    fn repaint(&self) -> Repaint {
        if (self.owner.is_some() && !self.reported) || self.trace.borrow().frames > 0 {
            Repaint::Continuous
        } else {
            Repaint::None
        }
    }
    fn frame(&mut self, elapsed: Duration) -> Option<Payload> {
        let mut trace = self.trace.borrow_mut();
        trace.frames = trace.frames.saturating_sub(1);
        let by = self.owner?;
        self.elapsed += elapsed;
        if self.reported || self.elapsed < Duration::from_millis(500) {
            return None;
        }
        self.reported = true;
        Some(Payload::Held {
            by,
            cell: trace.level,
        })
    }
    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Payload> {
        let Input::Pointer(pointer) = input else {
            return Outcome::IGNORED;
        };
        self.trace.borrow_mut().input.push(pointer);
        match (pointer.phase, self.owner) {
            (PointerPhase::Down, None) if hit.inside().is_some() => {
                self.trace.borrow_mut().hits.push(hit.inside().unwrap());
                self.owner = Some(pointer.id);
                self.elapsed = Duration::ZERO;
                self.reported = false;
                Outcome::captured().with_ownership(PointerOwnership::Claim)
            }
            (PointerPhase::Cancel, Some(by)) if by == pointer.id => {
                self.owner = None;
                Outcome::captured().with_ownership(PointerOwnership::Release)
            }
            (PointerPhase::Up | PointerPhase::DoubleClick, Some(by)) if by == pointer.id => {
                self.owner = None;
                if self.reported {
                    Outcome::captured()
                } else {
                    Outcome::set(Payload::Tap {
                        by,
                        cell: self.trace.borrow().level,
                        button: pointer.button,
                        clicks: pointer.clicks,
                    })
                }
            }
            (_, Some(_)) => Outcome::captured(),
            _ => Outcome::IGNORED,
        }
    }
    fn paint(
        &mut self,
        list: &mut DrawListBuilder,
        text: &mut TextMeasurer<'_>,
        bounds: Rect,
        _: &CustomSkin,
    ) {
        let ink = Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let stops = Stops::new(&[
            Stop {
                offset: 0.0,
                color: ink,
            },
            Stop {
                offset: 1.0,
                color: Rgba { r: 1.0, ..ink },
            },
        ])
        .unwrap();
        list.fill_rect(
            bounds,
            Paint::Linear {
                from: Pt {
                    x: bounds.x,
                    y: bounds.y,
                },
                to: Pt {
                    x: bounds.x + bounds.w,
                    y: bounds.y,
                },
                stops,
            },
        );
        let run = text.shape(WORDS, self.role, None);
        list.text(
            &run,
            WORDS,
            Transform::translate(Pt {
                x: bounds.x,
                y: bounds.y + run.height(),
            }),
            ink,
        );
        let mut trace = self.trace.borrow_mut();
        let level = trace.level;
        trace.painted.push(level);
        trace.runs.push(run);
    }
}

struct Empty;
impl Reads for Empty {
    fn get(&self, _: &str) -> Option<ReadValue<'_>> {
        None
    }
}

fn skin() -> Skin {
    let origin = SourceUri("consumer/skin.ron".into());
    let document = parse_skin_over(
        builtin::skin_doc(),
        r#"(schema: "kithara.skin", version: 1, id: "consumer", fonts: {Sans: ["Roboto.ttf"]})"#,
        &origin,
    )
    .unwrap();
    let mut resolver = builtin::resolver();
    resolver.insert_bytes("consumer/Roboto.ttf", &fs::read(FONT).unwrap());
    Skin::resolve_with_font_policy(
        document,
        builtin::text_doc(),
        &origin,
        &resolver,
        FontPolicy::Embedded,
    )
    .unwrap()
}

fn document(custom: &str) -> CompiledUi {
    let mut resolver = MemResolver::default();
    resolver.insert("layout.ron", r#"(schema: "kithara.layout", version: 1, id: "consumer", root: Module(instance: "demo", source: "module.ron", size: (w: Fill, h: Fill)))"#);
    resolver.insert("module.ron", &format!(r#"(schema: "kithara.module", version: 1, id: "consumer", chrome: Plain, root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{custom}]))"#));
    compile(
        "layout.ron",
        &resolver,
        &MapEndpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap()
}

fn mount(custom: &str, trace: &Rc<RefCell<Trace>>) -> (MasonryRoot<Out>, vello::Scene) {
    let ui = document(custom);
    let skin = skin();
    let ctx = Ctx::new(
        &ui,
        &Empty,
        &view::EMPTY,
        builtin::skin_doc(),
        Clock::default(),
    );
    let widget = Widget {
        trace: Rc::clone(trace),
        role: skin.text.body,
        owner: None,
        elapsed: Duration::ZERO,
        reported: false,
    };
    let host = MasonryHost::map_actions(ctx, &skin, Out::Document).with_custom(
        "demo/custom",
        widget,
        Out::Consumer,
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
    .unwrap();
    let (scene, _) = root.redraw().unwrap();
    drain(&mut root);
    (root, scene)
}

fn drain(root: &mut MasonryRoot<Out>) -> u32 {
    let mut frames = 0;
    let mut wanted = root.needs_frame();
    while wanted {
        assert!(frames < 16, "finite work must return to idle");
        root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(16)))
            .unwrap();
        if root.needs_frame() {
            root.redraw().unwrap();
        }
        wanted = root.complete_frame();
        frames += 1;
    }
    frames
}

fn info(id: u64) -> PointerInfo {
    PointerInfo {
        pointer_id: NativeId::new(id),
        persistent_device_id: None,
        pointer_type: PointerType::Touch,
    }
}
fn pointer(id: u64, at: (f64, f64), down: bool, count: u8) -> PointerEvent {
    let mut buttons = PointerButtons::new();
    if down {
        buttons.insert(Button::Primary);
    }
    let event = PointerButtonEvent {
        state: PointerState {
            position: PhysicalPosition::new(at.0, at.1),
            buttons,
            count,
            scale_factor: 1.0,
            ..PointerState::default()
        },
        button: Some(Button::Primary),
        pointer: info(id),
    };
    if down {
        PointerEvent::Down(event)
    } else {
        PointerEvent::Up(event)
    }
}

fn one_public_route_keeps_fonts_actions_cancellation_and_wake() {
    let trace = Rc::new(RefCell::new(Trace::default()));
    let (mut root, scene) = mount(SLOT, &trace);
    let bytes = fs::read(FONT).unwrap();
    assert!(
        scene
            .encoding()
            .resources
            .glyph_runs
            .iter()
            .any(|run| run.font.data.as_ref() == bytes)
    );
    assert!(!root.needs_frame());
    assert_eq!(trace.borrow().painted, [0]);
    for run in &trace.borrow().runs {
        assert!(
            run.segments()
                .iter()
                .all(|segment| matches!(segment.face(), GlyphFace::System(_)))
        );
    }
    trace.borrow_mut().level = 7;
    assert!(root.invalidate("demo/custom").unwrap());
    assert_eq!(drain(&mut root), 1);
    assert_eq!(trace.borrow().painted, [0, 7]);
    assert!(!root.needs_frame());
    root.handle_pointer_event(pointer(4, (10.0, 50.0), true, 2))
        .unwrap();
    root.handle_pointer_event(pointer(4, (10.0, 50.0), false, 2))
        .unwrap();
    assert_eq!(
        root.take_actions(),
        [Out::Consumer(Payload::Tap {
            by: PointerId(3),
            cell: 7,
            button: Some(PointerButton::Primary),
            clicks: 2
        })]
    );
    root.handle_pointer_event(pointer(6, (10.0, 50.0), true, 1))
        .unwrap();
    root.handle_pointer_event(PointerEvent::Cancel(info(6)))
        .unwrap();
    root.handle_pointer_event(pointer(8, (150.0, 50.0), false, 1))
        .unwrap();
    assert!(root.take_actions().is_empty());
    assert!(
        trace
            .borrow()
            .input
            .iter()
            .any(|pointer| pointer.id == PointerId(5) && pointer.phase == PointerPhase::Cancel)
    );
    root.handle_pointer_event(pointer(2, (10.0, 50.0), true, 1))
        .unwrap();
    for _ in 0..3 {
        root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(200)))
            .unwrap();
    }
    root.handle_pointer_event(pointer(2, (10.0, 50.0), false, 1))
        .unwrap();
    assert_eq!(
        root.take_actions(),
        [Out::Consumer(Payload::Held {
            by: PointerId(1),
            cell: 7
        })]
    );
    drain(&mut root);
    trace.borrow_mut().frames = 3;
    root.invalidate("demo/custom").unwrap();
    assert_eq!(drain(&mut root), 3);
    assert!(!root.needs_frame());
}

fn public_host_inverts_paint_poses_and_refuses_singular_poses() {
    for (position, scale, rotation) in [
        ((60.0, -20.0), (1.0, 1.0), 0.0),
        ((0.0, 0.0), (2.0, 0.5), 0.0),
        ((0.0, 0.0), (1.0, 1.0), 90.0),
    ] {
        let mut pose = Pose::default();
        pose.position = position;
        pose.anchor = (20.0, 20.0);
        pose.scale = scale;
        pose.rotation = rotation;
        let custom = format!(
            "Object(id: \"posed\", transform: (position: {:?}, anchor: {:?}, scale: {:?}, rotation: {}), child: {SLOT})",
            pose.position, pose.anchor, pose.scale, pose.rotation
        );
        let trace = Rc::new(RefCell::new(Trace::default()));
        let (mut root, _) = mount(&custom, &trace);
        let local = Pt { x: 30.0, y: 10.0 };
        let shown = pose.matrix().apply(local);
        root.handle_pointer_event(pointer(
            1,
            (f64::from(shown.x), f64::from(40.0 + shown.y)),
            true,
            1,
        ))
        .unwrap();
        let trace = trace.borrow();
        assert_eq!(trace.hits.len(), 1);
        assert!(
            (trace.hits[0].x - local.x).abs() < 0.001 && (trace.hits[0].y - local.y).abs() < 0.001
        );
    }
    let trace = Rc::new(RefCell::new(Trace::default()));
    let (mut root, _) = mount(
        &format!("Object(id: \"flat\", transform: (scale: (0.0, 1.0)), child: {SLOT})"),
        &trace,
    );
    root.handle_pointer_event(pointer(1, (10.0, 50.0), true, 1))
        .unwrap();
    assert!(trace.borrow().hits.is_empty());
    assert!(root.take_actions().is_empty());
}

fn main() {
    one_public_route_keeps_fonts_actions_cancellation_and_wake();
    public_host_inverts_paint_poses_and_refuses_singular_poses();
}
