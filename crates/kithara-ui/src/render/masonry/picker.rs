use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use masonry::{
    core::{PointerEvent, WidgetId},
    kurbo::Rect as MasonryRect,
};
use num_traits::cast::AsPrimitive;

use super::{custom::HostAction, node::pointer_button};
use crate::{
    draw::{Pt, Rect},
    engine::{Engine, Target},
    interact::{CursorShape, Input, MOUSE, Outcome, PointerInput, PointerPhase},
    render::{HostedControlPlan, UiEvent, engine_value},
};

/// One control an engine drives: what it is and where it sits.
pub(crate) struct EngineTarget {
    pub(super) area: Rc<Cell<MasonryRect>>,
    pub(super) plan: HostedControlPlan,
}

/// What routing one event through the engine produced.
pub(super) struct Routed {
    pub(super) focused: bool,
    pub(super) outcome: Outcome<HostAction>,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct HostedEngine {
    engine: RefCell<Engine>,
    map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    #[field(get(copy), vis = "pub(super)")]
    owner: WidgetId,
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
            .any(|target| matches!(target.plan, HostedControlPlan::Tree { .. }));
        let mut engine = Engine::default();
        engine.reconcile(targets.iter().flat_map(|target| target.plan.descriptors()));
        Rc::new(Self {
            engine: RefCell::new(engine),
            map_event,
            owner,
            targets,
            text_input,
        })
    }

    pub(super) fn route(&self, input: Input<'_>, point: Option<Pt>) -> Routed {
        let mut engine = self.engine.borrow_mut();
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
        let Some(emission) = emission else {
            return Routed {
                focused,
                outcome: Outcome::IGNORED,
            };
        };
        let path = emission.path;
        let child = emission.child;
        let outcome = emission
            .outcome
            .map(|event| (self.map_event)(engine_value(&path, child, event)));
        Routed { focused, outcome }
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
        let engine = self.engine.borrow();
        self.targets.iter().any(|target| {
            let HostedControlPlan::Picker { path, .. } = &target.plan else {
                return false;
            };
            engine
                .picker_snapshot(path)
                .is_some_and(|snapshot| snapshot.open)
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
}

pub(super) fn input(event: &PointerEvent) -> Option<(Input<'static>, Pt)> {
    let (phase, button, state) = match event {
        PointerEvent::Down(button) => (
            PointerPhase::Down,
            button.button.map(pointer_button),
            &button.state,
        ),
        PointerEvent::Move(update) => (PointerPhase::Move, None, &update.current),
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
