use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{CursorShape, Hit, Input, Outcome, PointerPhase, recognizers::ItemDrag},
};

pub(crate) struct ItemComponent {
    drag: ItemDrag,
    index: Option<usize>,
    control_path: String,
    path: String,
    count: usize,
}

impl ItemComponent {
    pub(crate) fn new(target: String, path: String, count: usize) -> Self {
        Self {
            count,
            control_path: path,
            drag: ItemDrag::default(),
            index: None,
            path: target,
        }
    }

    pub(super) fn pressed_index(&self) -> Option<usize> {
        if self.drag.is_held() {
            self.index
        } else {
            None
        }
    }

    pub(crate) fn reconcile(mut self, next: Self) -> Self {
        if self.index.is_some_and(|index| index >= next.count) {
            self.drag = ItemDrag::default();
            self.index = None;
        }
        self.count = next.count;
        self.control_path = next.control_path;
        self.path = next.path;
        self
    }
}

impl Component for ItemComponent {
    fn cancel_pointer(&mut self) {
        self.drag = ItemDrag::default();
        self.index = None;
    }

    fn captures_pointer(&self) -> bool {
        false
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        let drag = self.drag.cursor();
        if drag == CursorShape::None && hit.over() {
            CursorShape::Pointer
        } else {
            drag
        }
    }

    fn event_path(&self) -> &str {
        &self.control_path
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        target_index: Option<usize>,
        _now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        if matches!(
            input,
            Input::Pointer(pointer) if pointer.presses_primary()
        ) {
            self.drag = ItemDrag::default();
            self.index = if hit.over() {
                target_index.filter(|index| *index < self.count)
            } else {
                None
            };
        }
        let drag = self.drag.on_input(input, hit);
        let outcome = self.index.map_or(Outcome::IGNORED, |index| {
            drag.map(|event| EngineEvent::Drag { event, index })
        });
        if outcome != Outcome::IGNORED {
            if matches!(
                input,
                Input::Pointer(pointer) if pointer.phase == PointerPhase::Up
            ) {
                self.index = None;
            }
            return (outcome, None);
        }
        let outcome = match input {
            Input::Pointer(pointer) if pointer.presses_primary() && self.index.is_some() => {
                Outcome::captured()
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up => {
                self.index.take().map_or(Outcome::IGNORED, |index| {
                    if target_index == Some(index) && hit.over() {
                        Outcome::set(EngineEvent::Index(index))
                    } else {
                        Outcome::captured()
                    }
                })
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        };
        (outcome, None)
    }

    fn kind(&self) -> Kind {
        Kind::Item
    }

    fn path(&self) -> &str {
        &self.path
    }
}
