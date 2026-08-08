use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{
        CursorShape, Hit, Input, Outcome, PointerPhase,
        recognizers::{Scalar, ScalarState},
    },
};

pub(in crate::engine) struct ScalarComponent {
    path: String,
    kind: Kind,
    scalar: Scalar,
    drag_step: Option<f64>,
    state: ScalarState,
}

impl ScalarComponent {
    pub(super) fn new(path: String, kind: Kind, scalar: Scalar, drag_step: Option<f64>) -> Self {
        Self {
            path,
            kind,
            scalar,
            drag_step,
            state: ScalarState::default(),
        }
    }

    pub(super) fn reconcile(mut self, next: Self) -> Self {
        if self.kind != next.kind {
            return next;
        }
        self.path = next.path;
        self.scalar = next.scalar;
        self.drag_step = next.drag_step;
        self
    }
}

impl Component for ScalarComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        self.kind
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        _index: Option<usize>,
        now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let outcome = self.scalar.on_input(&mut self.state, input, hit, now);
        (
            outcome.map(|value| EngineEvent::Scalar(scalar_value(input, value, self.drag_step))),
            None,
        )
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        self.scalar.cursor(&self.state, hit)
    }

    delegate::delegate! {
        to self.state {
            fn captures_pointer(&self) -> bool;
            fn cancel_pointer(&mut self);
        }
    }
}

pub(crate) fn scalar_value(input: Input<'_>, value: f32, drag_step: Option<f64>) -> f64 {
    let value = f64::from(value);
    if value == 0.0 || value == 1.0 {
        return value;
    }
    match (input, drag_step) {
        (Input::Pointer(pointer), Some(step))
            if matches!(pointer.phase, PointerPhase::Down | PointerPhase::Move) =>
        {
            ((value / step).round() * step).min(1.0)
        }
        _ => value,
    }
}
