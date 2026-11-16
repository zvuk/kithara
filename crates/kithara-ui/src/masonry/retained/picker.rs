use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use kithara_platform::time::Instant;
use masonry::{
    core::{EventCtx, PointerEvent, WidgetId},
    kurbo::{Affine, Rect as MasonryRect},
};
use num_traits::cast::AsPrimitive;

use super::{built::Within, custom::HostAction};
use crate::{
    atoms::{bar::context::Context, table::face::Drawn},
    draw::{Pt, Rect},
    engine::{Descriptor, Engine, PickerSnapshot, Target, TextInputSnapshot},
    hosts::{
        drag::Carried,
        hosted::{HostedControlPlan, SearchPlan, TablePlan, TreePlan},
    },
    interact::{
        CursorShape, Input, Outcome, PointerInput, PointerPhase,
        masonry::{pointer_button, portable_pointer_id, portable_scroll},
    },
    masonry::{
        event::engine_value,
        hosted::{MasonryHostedState, SearchProjection, TableProjection, TreeProjection},
        paint::tree::Drawn as TreeDrawn,
    },
    render::{Published, document::Ctx},
};

/// One control an engine drives: what it is and where it sits.
///
/// The box is a carrier, not a truth of its own: the tree owns where a node
/// stands, and the root fills this cell out of the tree after every event.
pub(crate) struct EngineTarget {
    pub(crate) plan: HostedControlPlan<MasonryHostedState>,
    pub(crate) area: Rc<Cell<MasonryRect>>,
    /// The widget that draws this control. It is the engine's own node only
    /// when the control hosts its engine itself; a module handed to an engine
    /// hosts one engine above every control in it, and Masonry paints the
    /// widget that asked for paint rather than its children.
    pub(crate) node: WidgetId,
}

impl EngineTarget {
    pub(crate) fn new(
        node: WidgetId,
        area: Rc<Cell<MasonryRect>>,
        plan: HostedControlPlan<MasonryHostedState>,
    ) -> Option<Self> {
        (!plan.descriptors().is_empty()).then_some(Self { plan, area, node })
    }
}

/// The menu an engine currently shows, ready to be drawn.
pub(crate) struct OpenPicker {
    pub(crate) highlighted: Option<usize>,
    pub(crate) anchor: Rect,
    pub(crate) items: Vec<String>,
}

/// What routing one event through the engine produced.
pub(crate) struct Routed {
    pub(crate) outcome: Outcome<HostAction>,
    pub(crate) drag: Option<Published>,
    /// The widgets whose face the event changed, which are the widgets that
    /// have to be painted again for it to be seen.
    pub(crate) repaint: Vec<WidgetId>,
    pub(crate) focused: bool,
}

/// The face one control an engine drives shows right now.
///
/// A face is read from the engine rather than kept, so it is the one thing that
/// says whether an event changed what a control looks like. A control whose
/// face the engine cannot answer for shows the value it was mounted with and
/// is repainted when that value is re-read, not here.
#[derive(PartialEq)]
enum Face {
    Table(Option<Drawn>),
    Tree(Option<TreeDrawn>),
    Search(Option<TextInputSnapshot>),
    Picker(Option<PickerSnapshot>),
    Unread,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct HostedEngine {
    /// The layer that draws this engine's open menu, and whether that menu has
    /// changed since the layer last drew it. The layer sits outside the tree
    /// this engine belongs to, so nothing below marks it: the root reads this
    /// after every event and repaints the layer itself.
    menu: Cell<Option<WidgetId>>,
    menu_changed: Cell<bool>,
    engine: Rc<RefCell<Engine>>,
    map_event: Rc<dyn Fn(Published) -> HostAction>,
    pointer: Rc<Cell<Option<Pt>>>,
    /// What draws each table, tree and search face from the engine.
    _projections: Vec<Rc<EngineProjection>>,
    targets: Vec<Within<EngineTarget>>,
    #[field(get(copy), vis = "pub(crate)")]
    owner: WidgetId,
    #[field(get(copy), vis = "pub(crate)", rename = "accepts_text_input")]
    text_input: bool,
}

impl HostedEngine {
    pub(crate) fn new(
        owner: WidgetId,
        targets: Vec<Within<EngineTarget>>,
        map_event: Rc<dyn Fn(Published) -> HostAction>,
    ) -> Rc<Self> {
        let text_input = targets.iter().any(|target| {
            matches!(
                target.item.plan,
                HostedControlPlan::Tree(_) | HostedControlPlan::Search(_)
            )
        });
        let mut engine = Engine::default();
        engine.reconcile(
            targets
                .iter()
                .flat_map(|target| target.item.plan.descriptors()),
        );
        let engine = Rc::new(RefCell::new(engine));
        let pointer = Rc::new(Cell::new(None));
        Rc::new_cyclic(|host| {
            let projections = targets
                .iter()
                .filter_map(|target| {
                    let projection = Rc::new(EngineProjection {
                        area: Rc::clone(&target.item.area),
                        engine: Rc::clone(&engine),
                        host: host.clone(),
                        pointer: Rc::clone(&pointer),
                    });
                    let bound: Weak<EngineProjection> = Rc::downgrade(&projection);
                    match &target.item.plan {
                        HostedControlPlan::Table(plan) => plan.bind_projection(bound),
                        HostedControlPlan::Tree(plan) => plan.bind_projection(bound),
                        HostedControlPlan::Search(plan) => plan.bind_projection(bound),
                        _ => return None,
                    }
                    Some(projection)
                })
                .collect();
            Self {
                map_event,
                owner,
                targets,
                text_input,
                engine: Rc::clone(&engine),
                menu: Cell::new(None),
                menu_changed: Cell::new(false),
                pointer: Rc::clone(&pointer),
                _projections: projections,
            }
        })
    }

    /// Whether this engine holds the pointer, so the event is its alone.
    ///
    /// A gesture that took the pointer keeps it until it lets go, and the
    /// event that lets go is still the gesture's: a double click ends a drag
    /// and is answered by the control that was being dragged, not by whatever
    /// the release happens to be over. An item being carried out of a list is
    /// the other kind of gesture — it never takes the pointer, so what the
    /// hand is over hears the event as well.
    pub(crate) fn captures_pointer(&self) -> bool {
        self.engine.borrow().captures_pointer()
    }

    pub(crate) fn cursor(&self, point: Pt) -> CursorShape {
        let engine = self.engine.borrow();
        let targets = self.targets(&engine, Some(point));
        engine.cursor(&targets)
    }

    /// The face every control this engine drives shows, in target order.
    fn faces(&self, engine: &Engine, point: Option<Pt>) -> Vec<Face> {
        self.targets
            .iter()
            .map(|target| {
                if !target.within.shown() {
                    return Face::Unread;
                }
                let target = &target.item;
                match &target.plan {
                    HostedControlPlan::Table(plan) => {
                        Face::Table(plan.view(engine, point, target_bounds(target)))
                    }
                    HostedControlPlan::Search(plan) => {
                        Face::Search(engine.text_input_snapshot(&plan.path))
                    }
                    HostedControlPlan::Tree(plan) => {
                        Face::Tree(plan.view(engine, point, target_bounds(target)))
                    }
                    HostedControlPlan::Picker { path, .. } => {
                        Face::Picker(engine.picker_snapshot(path))
                    }
                    _ => Face::Unread,
                }
            })
            .collect()
    }

    pub(crate) fn has_open_picker(&self) -> bool {
        self.open_picker().is_some()
    }

    pub(crate) fn input_method_area(&self) -> Option<Rect> {
        let engine = self.engine.borrow();
        let targets = self.targets(&engine, None);
        engine.input_method(&targets).map(|request| request.caret)
    }

    /// The one menu this engine currently shows, if any: where it hangs, what
    /// it offers, and which option the pointer or the keyboard is on.
    ///
    /// A control shows at most one menu and a host raises at most one at a
    /// time, so the first open one is the answer.
    pub(crate) fn open_picker(&self) -> Option<OpenPicker> {
        let engine = self.engine.borrow();
        self.all().find_map(|target| {
            let HostedControlPlan::Picker {
                path, items, face, ..
            } = &target.plan
            else {
                return None;
            };
            let snapshot = engine.picker_snapshot(path)?;
            snapshot.open.then(|| OpenPicker {
                anchor: Context::placed(*face, target_bounds(target)),
                highlighted: snapshot.highlighted,
                items: items.clone(),
            })
        })
    }

    /// Re-reads every plan this engine drives against the frame just read.
    ///
    /// The tree stands between frames, so a plan resolved when the tree was
    /// built would measure every later gesture against a moment that has since
    /// passed. Nothing is reconciled here: the descriptors are rebuilt from
    /// these plans on the next event anyway.
    pub(crate) fn reread(&self, ctx: Ctx<'_, '_>) {
        for target in self.standing() {
            target.plan.reread(ctx);
        }
    }

    pub(crate) fn stand(&self, shown: bool) {
        let open = self.has_open_picker();
        let descriptors: Vec<Descriptor> = if shown {
            self.standing()
                .flat_map(|target| target.plan.descriptors())
                .collect()
        } else {
            Vec::new()
        };
        self.engine.borrow_mut().reconcile(descriptors);
        if self.has_open_picker() != open {
            self.menu_changed.set(true);
        }
    }

    fn all(&self) -> impl Iterator<Item = &EngineTarget> {
        self.targets.iter().map(|target| &target.item)
    }

    fn standing(&self) -> impl Iterator<Item = &EngineTarget> {
        self.targets
            .iter()
            .filter(|target| target.within.shown())
            .map(|target| &target.item)
    }

    pub(crate) fn route(&self, input: Input<'_>, point: Option<Pt>) -> Routed {
        let mut engine = self.engine.borrow_mut();
        let before = self.faces(&engine, self.pointer.get());
        if matches!(input, Input::Pointer(_) | Input::Wheel(_)) {
            self.pointer.set(point);
        }
        let targets = self.targets(&engine, point);
        let descriptors = self
            .standing()
            .flat_map(|target| target.plan.descriptors())
            .collect::<Vec<Descriptor>>();
        engine.reconcile(descriptors);
        for target in &targets {
            engine.set_scroll_viewport(target.path, target.hit.area());
        }
        let emission = engine.handle(input, &targets, Instant::now());
        let focused = engine.focused_path().is_some();
        let after = self.faces(&engine, self.pointer.get());
        if menu_changed(&before, &after) {
            self.menu_changed.set(true);
        }
        let repaint = self
            .all()
            .zip(&before)
            .zip(&after)
            .filter(|((_, before), after)| before != after)
            .map(|((target, _), _)| target.node)
            .collect::<Vec<WidgetId>>();
        let Some(emission) = emission else {
            return Routed {
                focused,
                repaint,
                drag: None,
                outcome: Outcome::IGNORED,
            };
        };
        let path = emission.path;
        let child = emission.child;
        let outcome = emission
            .outcome
            .map(|event| engine_value(&path, child, event));
        let mut drag = None;
        let outcome = outcome.map(|event| {
            if matches!(event, Published::Carry { .. }) {
                drag = Some(event.clone());
            }
            (self.map_event)(event)
        });
        Routed {
            outcome,
            drag,
            repaint,
            focused,
        }
    }

    pub(crate) fn carried(&self, table: &str, index: usize) -> Option<Carried> {
        self.all()
            .find_map(|target| target.plan.carried(table, index))
    }

    pub(crate) fn action(&self, event: Published) -> HostAction {
        (self.map_event)(event)
    }

    pub(crate) fn set_menu_layer(&self, layer: WidgetId) {
        self.menu.set(Some(layer));
    }

    /// The layer to repaint, once, because the menu it draws has changed.
    pub(crate) fn take_changed_menu(&self) -> Option<WidgetId> {
        self.menu_changed.replace(false).then(|| self.menu.get())?
    }

    fn targets<'a>(&'a self, engine: &Engine, point: Option<Pt>) -> Vec<Target<'a>> {
        let mut targets = Vec::new();
        for target in self.standing() {
            let area = target.area.get();
            target.plan.append_targets(
                Rect {
                    x: area.x0.as_(),
                    y: area.y0.as_(),
                    w: area.width().as_(),
                    h: area.height().as_(),
                },
                point,
                Some(engine),
                &mut targets,
            );
        }
        targets
    }

    delegate::delegate! {
        to self.engine.borrow_mut() {
            pub(crate) fn clear_focus(&self);
        }
    }
}

struct EngineProjection {
    area: Rc<Cell<MasonryRect>>,
    engine: Rc<RefCell<Engine>>,
    pointer: Rc<Cell<Option<Pt>>>,
    host: Weak<HostedEngine>,
}

impl TableProjection for EngineProjection {
    fn project(&self, plan: &TablePlan<MasonryHostedState>) -> Option<Drawn> {
        let engine = self.engine.borrow();
        plan.view(&engine, self.pointer.get(), bounds(self.area.get()))
    }

    fn reconcile(&self) {
        self.reconcile_engine();
    }
}

impl SearchProjection for EngineProjection {
    fn project(&self, plan: &SearchPlan<MasonryHostedState>) -> Option<TextInputSnapshot> {
        self.engine.borrow().text_input_snapshot(&plan.path)
    }
    fn reconcile(&self) {
        self.reconcile_engine();
    }
}

impl TreeProjection for EngineProjection {
    fn project(&self, plan: &TreePlan<MasonryHostedState>) -> Option<TreeDrawn> {
        let engine = self.engine.borrow();
        plan.view(&engine, self.pointer.get(), bounds(self.area.get()))
    }

    fn reconcile(&self) {
        self.reconcile_engine();
    }
}

impl EngineProjection {
    fn reconcile_engine(&self) {
        if let Some(host) = self.host.upgrade() {
            host.engine
                .borrow_mut()
                .reconcile(host.standing().flat_map(|target| target.plan.descriptors()));
        }
    }
}

/// Whether a menu one of these controls raises has changed.
///
/// The menu is drawn by a layer of its own, outside the tree the control sits
/// in, so it is repainted by the root rather than by the widget that raised it.
fn menu_changed(before: &[Face], after: &[Face]) -> bool {
    before
        .iter()
        .zip(after)
        .any(|(before, after)| matches!(before, Face::Picker(_)) && before != after)
}

fn target_bounds(target: &EngineTarget) -> Rect {
    bounds(target.area.get())
}

fn bounds(area: MasonryRect) -> Rect {
    Rect {
        x: area.x0.as_(),
        y: area.y0.as_(),
        w: area.width().as_(),
        h: area.height().as_(),
    }
}

pub(crate) fn sync_ime_area(ctx: &mut EventCtx<'_>, engine: &HostedEngine) {
    if let Some(area) = local_ime_area(engine, ctx.window_transform()) {
        ctx.set_ime_area(area);
    } else {
        ctx.clear_ime_area();
    }
}

pub(crate) fn local_ime_area(engine: &HostedEngine, transform: Affine) -> Option<MasonryRect> {
    engine.input_method_area().map(|area| {
        transform.inverse().transform_rect_bbox(MasonryRect::new(
            f64::from(area.x),
            f64::from(area.y),
            f64::from(area.x + area.w),
            f64::from(area.y + area.h),
        ))
    })
}

/// Where the window says the hand is, in the coordinates the document is laid
/// out in.
pub(crate) fn at(event: &PointerEvent) -> Option<Pt> {
    let position = match event {
        PointerEvent::Down(button) | PointerEvent::Up(button) => button.state.logical_position(),
        PointerEvent::Move(update) => update.current.logical_position(),
        PointerEvent::Scroll(scroll) => scroll.state.logical_position(),
        PointerEvent::Gesture(gesture) => gesture.state.logical_position(),
        PointerEvent::Cancel(_) | PointerEvent::Enter(_) | PointerEvent::Leave(_) => return None,
    };
    Some(Pt {
        x: position.x.as_(),
        y: position.y.as_(),
    })
}

/// One pointer event in the neutral vocabulary, and where the hand was when
/// the window reported it.
///
/// The phases a control answers are not the phases the window reports: a
/// second press inside the platform's interval arrives as another press, and
/// is a double click only once the release that follows it is in hand. So the
/// count is carried across the two events here, where the whole document is
/// routed from, rather than inside any one control.
pub(crate) fn pointing(
    event: &PointerEvent,
    double_click: &mut bool,
    scale: f64,
) -> Option<(Input<'static>, Option<Pt>)> {
    let pointer = match event {
        PointerEvent::Down(button) => {
            *double_click = button.state.count >= 2;
            Some((
                PointerPhase::Down,
                button.button.map(pointer_button),
                button.state.count,
            ))
        }
        PointerEvent::Move(update) => Some((PointerPhase::Move, None, update.current.count)),
        PointerEvent::Up(button) => {
            let phase = if std::mem::take(double_click) {
                PointerPhase::DoubleClick
            } else {
                PointerPhase::Up
            };
            Some((phase, button.button.map(pointer_button), button.state.count))
        }
        PointerEvent::Leave(_) => Some((PointerPhase::Leave, None, 0)),
        PointerEvent::Cancel(_) => Some((PointerPhase::Cancel, None, 0)),
        PointerEvent::Enter(_) | PointerEvent::Scroll(_) | PointerEvent::Gesture(_) => None,
    };
    let at = at(event);
    match (pointer, event) {
        (Some((phase, button, clicks)), _) => Some((
            Input::Pointer(PointerInput::new(
                portable_pointer_id(event),
                button,
                phase,
                at,
                clicks,
            )),
            at,
        )),
        (None, PointerEvent::Scroll(scroll)) => {
            Some((Input::Wheel(portable_scroll(scroll.delta, scale)?), at))
        }
        (None, _) => None,
    }
}
