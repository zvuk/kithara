use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use masonry::{
    core::{EventCtx, PointerEvent, WidgetId},
    kurbo::{Affine, Rect as MasonryRect},
};
use num_traits::cast::AsPrimitive;

use super::custom::HostAction;
use crate::{
    atoms::{bar::context::Context, table::face::Drawn, tree::retained::Drawn as TreeDrawn},
    draw::{Pt, Rect},
    engine::{Engine, PickerSnapshot, Target},
    interact::{
        CursorShape, Input, MOUSE, Outcome, PointerInput, PointerPhase, masonry::pointer_button,
    },
    render::{
        HostedControlPlan, UiEvent,
        document::Ctx,
        engine_value,
        hosted::{TablePlan, TableProjection, TreePlan, TreeProjection},
    },
};

/// One control an engine drives: what it is and where it sits.
pub(crate) struct EngineTarget {
    pub(super) area: Rc<Cell<MasonryRect>>,
    pub(super) plan: HostedControlPlan,
}

impl EngineTarget {
    pub(super) fn new(area: Rc<Cell<MasonryRect>>, plan: HostedControlPlan) -> Option<Self> {
        (!plan.descriptors().is_empty()).then_some(Self { area, plan })
    }
}

/// The menu an engine currently shows, ready to be drawn.
pub(super) struct OpenPicker {
    pub(super) anchor: Rect,
    pub(super) highlighted: Option<usize>,
    pub(super) items: Vec<String>,
}

/// What routing one event through the engine produced.
pub(super) struct Routed {
    pub(super) focused: bool,
    pub(super) outcome: Outcome<HostAction>,
    pub(super) repaint: bool,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct HostedEngine {
    engine: Rc<RefCell<Engine>>,
    map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    #[field(get(copy), vis = "pub(super)")]
    owner: WidgetId,
    /// The layer that draws this engine's open menu, and whether that menu has
    /// changed since the layer last drew it. The layer sits outside the tree
    /// this engine belongs to, so nothing below marks it: the root reads this
    /// after every event and repaints the layer itself.
    menu: Cell<Option<WidgetId>>,
    menu_changed: Cell<bool>,
    pointer: Rc<Cell<Option<Pt>>>,
    _projections: Vec<Rc<dyn TableProjection>>,
    _tree_projections: Vec<Rc<dyn TreeProjection>>,
    targets: Vec<EngineTarget>,
    #[field(get(copy), vis = "pub(super)", rename = "accepts_text_input")]
    text_input: bool,
}

impl HostedEngine {
    pub(super) fn new(
        owner: WidgetId,
        targets: Vec<EngineTarget>,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    ) -> Rc<Self> {
        let text_input = targets
            .iter()
            .any(|target| matches!(target.plan, HostedControlPlan::Tree(_)));
        let mut engine = Engine::default();
        engine.reconcile(targets.iter().flat_map(|target| target.plan.descriptors()));
        let engine = Rc::new(RefCell::new(engine));
        let pointer = Rc::new(Cell::new(None));
        Rc::new_cyclic(|host| {
            let projections = targets
                .iter()
                .filter_map(|target| {
                    let HostedControlPlan::Table(plan) = &target.plan else {
                        return None;
                    };
                    let projection: Rc<dyn TableProjection> = Rc::new(EngineProjection {
                        area: Rc::clone(&target.area),
                        engine: Rc::clone(&engine),
                        host: host.clone(),
                        pointer: Rc::clone(&pointer),
                    });
                    plan.bind_projection(Rc::downgrade(&projection));
                    Some(projection)
                })
                .collect();
            let tree_projections = targets
                .iter()
                .filter_map(|target| {
                    let HostedControlPlan::Tree(plan) = &target.plan else {
                        return None;
                    };
                    let projection: Rc<dyn TreeProjection> = Rc::new(EngineProjection {
                        area: Rc::clone(&target.area),
                        engine: Rc::clone(&engine),
                        host: host.clone(),
                        pointer: Rc::clone(&pointer),
                    });
                    plan.bind_projection(Rc::downgrade(&projection));
                    Some(projection)
                })
                .collect();
            Self {
                engine: Rc::clone(&engine),
                map_event,
                owner,
                menu: Cell::new(None),
                menu_changed: Cell::new(false),
                pointer: Rc::clone(&pointer),
                _projections: projections,
                _tree_projections: tree_projections,
                targets,
                text_input,
            }
        })
    }

    /// Re-reads every plan this engine drives against the frame just read.
    ///
    /// The tree stands between frames, so a plan resolved when the tree was
    /// built would measure every later gesture against a moment that has since
    /// passed. Nothing is reconciled here: the descriptors are rebuilt from
    /// these plans on the next event anyway.
    pub(super) fn reread(&self, ctx: Ctx<'_, '_>) {
        for target in &self.targets {
            target.plan.reread(ctx);
        }
    }

    pub(super) fn route(&self, input: Input<'_>, point: Option<Pt>) -> Routed {
        let mut engine = self.engine.borrow_mut();
        let before = self.table_views(&engine, self.pointer.get());
        let tree_before = self.tree_views(&engine, self.pointer.get());
        let picker_before = self.picker_views(&engine);
        if matches!(input, Input::Pointer(_) | Input::Wheel(_)) {
            self.pointer.set(point);
        }
        let targets = self.targets(&engine, point);
        let descriptors = self
            .targets
            .iter()
            .flat_map(|target| target.plan.active_descriptors(&targets))
            .collect::<Vec<_>>();
        engine.reconcile(descriptors);
        for target in &targets {
            engine.set_scroll_viewport(target.path, target.hit.area());
        }
        let emission = engine.handle(input, &targets, kithara_platform::time::Instant::now());
        let focused = engine.focused_path().is_some();
        // A menu that just opened, closed, or moved its highlight is a picture
        // the tree cannot know changed: it is drawn by a layer above the tree,
        // out of the engine's own state, so nothing below asks for the frame.
        let menu_changed = picker_before != self.picker_views(&engine);
        if menu_changed {
            self.menu_changed.set(true);
        }
        let repaint = menu_changed
            || before != self.table_views(&engine, self.pointer.get())
            || tree_before != self.tree_views(&engine, self.pointer.get());
        let Some(emission) = emission else {
            return Routed {
                focused,
                outcome: Outcome::IGNORED,
                repaint,
            };
        };
        let path = emission.path;
        let child = emission.child;
        let outcome = emission
            .outcome
            .map(|event| (self.map_event)(engine_value(&path, child, event)));
        Routed {
            focused,
            outcome,
            repaint,
        }
    }

    pub(super) fn input_method_area(&self) -> Option<Rect> {
        let engine = self.engine.borrow();
        let targets = self.targets(&engine, None);
        engine.input_method(&targets).map(|request| request.caret)
    }

    pub(super) fn cursor(&self, point: Pt) -> CursorShape {
        let engine = self.engine.borrow();
        let targets = self.targets(&engine, Some(point));
        engine.cursor(&targets)
    }

    pub(super) fn has_open_picker(&self) -> bool {
        self.open_picker().is_some()
    }

    /// Whether a hand is pulling an item out of a list this engine drives.
    pub(super) fn holds_item(&self) -> bool {
        self.engine.borrow().holds_item()
    }

    pub(super) fn set_menu_layer(&self, layer: WidgetId) {
        self.menu.set(Some(layer));
    }

    /// The layer to repaint, once, because the menu it draws has changed.
    pub(super) fn take_changed_menu(&self) -> Option<WidgetId> {
        self.menu_changed.replace(false).then(|| self.menu.get())?
    }

    /// The one menu this engine currently shows, if any: where it hangs, what
    /// it offers, and which option the pointer or the keyboard is on.
    ///
    /// A control shows at most one menu and a host raises at most one at a
    /// time, so the first open one is the answer.
    pub(super) fn open_picker(&self) -> Option<OpenPicker> {
        let engine = self.engine.borrow();
        self.targets.iter().find_map(|target| {
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

    #[cfg(test)]
    pub(super) fn tree_picture(&self, path: &str) -> Option<(usize, String)> {
        self.targets.iter().find_map(|target| {
            let HostedControlPlan::Tree(plan) = &target.plan else {
                return None;
            };
            if plan.path != path {
                return None;
            }
            let picture = plan.picture();
            Some((picture.row_count(), picture.query().to_owned()))
        })
    }

    delegate::delegate! {
        to self.engine.borrow_mut() {
            pub(super) fn clear_focus(&self);
        }
    }

    fn targets<'a>(&'a self, engine: &Engine, point: Option<Pt>) -> Vec<Target<'a>> {
        let mut targets = Vec::new();
        for target in &self.targets {
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

    fn table_views(&self, engine: &Engine, point: Option<Pt>) -> Vec<Drawn> {
        self.targets
            .iter()
            .filter_map(|target| {
                let HostedControlPlan::Table(plan) = &target.plan else {
                    return None;
                };
                plan.view(engine, point, target_bounds(target))
            })
            .collect()
    }

    fn picker_views(&self, engine: &Engine) -> Vec<PickerSnapshot> {
        self.targets
            .iter()
            .filter_map(|target| {
                let HostedControlPlan::Picker { path, .. } = &target.plan else {
                    return None;
                };
                engine.picker_snapshot(path)
            })
            .collect()
    }

    fn tree_views(&self, engine: &Engine, point: Option<Pt>) -> Vec<TreeDrawn> {
        self.targets
            .iter()
            .filter_map(|target| {
                let HostedControlPlan::Tree(plan) = &target.plan else {
                    return None;
                };
                plan.view(engine, point, target_bounds(target))
            })
            .collect()
    }
}

struct EngineProjection {
    area: Rc<Cell<MasonryRect>>,
    engine: Rc<RefCell<Engine>>,
    host: Weak<HostedEngine>,
    pointer: Rc<Cell<Option<Pt>>>,
}

impl TableProjection for EngineProjection {
    fn project(&self, plan: &TablePlan) -> Option<Drawn> {
        let engine = self.engine.borrow();
        plan.view(&engine, self.pointer.get(), bounds(self.area.get()))
    }

    fn reconcile(&self) {
        self.reconcile_engine();
    }
}

impl TreeProjection for EngineProjection {
    fn project(&self, plan: &TreePlan) -> Option<TreeDrawn> {
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
            host.engine.borrow_mut().reconcile(
                host.targets
                    .iter()
                    .flat_map(|target| target.plan.descriptors()),
            );
        }
    }
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

pub(super) fn sync_ime_area(ctx: &mut EventCtx<'_>, engine: &HostedEngine) {
    if let Some(area) = local_ime_area(engine, ctx.window_transform()) {
        ctx.set_ime_area(area);
    } else {
        ctx.clear_ime_area();
    }
}

pub(super) fn local_ime_area(engine: &HostedEngine, transform: Affine) -> Option<MasonryRect> {
    engine.input_method_area().map(|area| {
        transform.inverse().transform_rect_bbox(MasonryRect::new(
            f64::from(area.x),
            f64::from(area.y),
            f64::from(area.x + area.w),
            f64::from(area.y + area.h),
        ))
    })
}

pub(super) fn input(event: &PointerEvent) -> Option<(Input<'static>, Pt)> {
    let (phase, button, state) = match event {
        PointerEvent::Down(button) => (
            PointerPhase::Down,
            button.button.map(pointer_button),
            &button.state,
        ),
        PointerEvent::Move(update) => (PointerPhase::Move, None, &update.current),
        PointerEvent::Up(button) => (
            PointerPhase::Up,
            button.button.map(pointer_button),
            &button.state,
        ),
        _ => return None,
    };
    let position = state.logical_position();
    let point = Pt {
        x: position.x.as_(),
        y: position.y.as_(),
    };
    Some((
        Input::Pointer(PointerInput::new(
            MOUSE,
            button,
            phase,
            Some(point),
            state.count,
        )),
        point,
    ))
}
