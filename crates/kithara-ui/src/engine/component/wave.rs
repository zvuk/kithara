use std::ops::Range;

use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{
        CursorShape, Hit, Hover, Input, Modifiers, Outcome, PointerPhase,
        recognizers::{Scalar, ScalarState, Track},
    },
};

/// The endpoints a hero wave publishes under instead of its own path: the two
/// ends of a loop region, and the zoom the wheel sets.
#[derive(Clone, Copy)]
enum Endpoint {
    LoopStart,
    LoopEnd,
    Zoom,
}

impl Endpoint {
    const fn name(self) -> &'static str {
        match self {
            Self::LoopStart => "loop_start",
            Self::LoopEnd => "loop_end",
            Self::Zoom => "zoom",
        }
    }
}

pub(in crate::engine) struct HeroWaveComponent {
    path: String,
    drag: Scalar,
    drag_state: ScalarState,
    modifiers: Modifiers,
    loop_active: bool,
    visible: Range<f32>,
    wheel_positive: f32,
    wheel_non_positive: f32,
}

impl HeroWaveComponent {
    pub(super) fn new(
        path: String,
        scale: f32,
        progress: f32,
        visible: Range<f32>,
        wheel_positive: f32,
        wheel_non_positive: f32,
    ) -> Self {
        Self {
            path,
            drag: plain_drag(scale, progress),
            drag_state: ScalarState::default(),
            modifiers: Modifiers::default(),
            loop_active: false,
            visible,
            wheel_positive,
            wheel_non_positive,
        }
    }

    pub(super) fn reconcile(mut self, next: Self) -> Self {
        self.path = next.path;
        self.drag = next.drag;
        self.visible = next.visible;
        self.wheel_positive = next.wheel_positive;
        self.wheel_non_positive = next.wheel_non_positive;
        self
    }

    fn position(&self, hit: &Hit) -> Option<f32> {
        let position = hit.at()?;
        let area = hit.area();
        (area.w > 0.0).then(|| {
            (self.visible.start
                + (position.x - area.x) / area.w * (self.visible.end - self.visible.start))
                .clamp(0.0, 1.0)
        })
    }
}

impl Component for HeroWaveComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        Kind::HeroWave
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        _index: Option<usize>,
        now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        match input {
            Input::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
                (Outcome::IGNORED, None)
            }
            Input::Pointer(pointer)
                if pointer.phase == PointerPhase::Down && self.modifiers.shift() && hit.over() =>
            {
                let Some(position) = self.position(hit) else {
                    return (Outcome::IGNORED, None);
                };
                self.loop_active = true;
                (
                    Outcome::set(EngineEvent::Scalar(f64::from(position))),
                    Some(Endpoint::LoopStart.name()),
                )
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Move && self.loop_active => {
                let Some(position) = self.position(hit) else {
                    return (Outcome::IGNORED, None);
                };
                (
                    Outcome::set(EngineEvent::Scalar(f64::from(position))),
                    Some(Endpoint::LoopEnd.name()),
                )
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up && self.loop_active => {
                self.loop_active = false;
                (Outcome::captured(), None)
            }
            Input::Wheel(scroll) if hit.over() => (
                Outcome::set(EngineEvent::Scalar(f64::from(if scroll.y() > 0.0 {
                    self.wheel_positive
                } else {
                    self.wheel_non_positive
                }))),
                Some(Endpoint::Zoom.name()),
            ),
            _ => (
                self.drag
                    .on_input(&mut self.drag_state, input, hit, now)
                    .map(|value| EngineEvent::Scalar(f64::from(value))),
                None,
            ),
        }
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        self.drag.cursor(&self.drag_state, hit)
    }

    fn captures_pointer(&self) -> bool {
        self.loop_active || self.drag_state.captures_pointer()
    }

    fn cancel_pointer(&mut self) {
        self.loop_active = false;
        self.drag_state.cancel_pointer();
    }
}

fn plain_drag(scale: f32, progress: f32) -> Scalar {
    Scalar::builder()
        .track(Track::RelativeHorizontal {
            scale,
            value: progress,
        })
        .hover(Hover::new(CursorShape::Grab))
        .build()
}
