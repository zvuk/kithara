use std::mem;

use super::super::{CursorShape, Hit, Input, Outcome, PointerPhase};
use crate::draw::Pt;

/// What a press-and-pull on one item of a list amounts to. Which item it was
/// stays with whoever owns the list; the recognizer reports only that a drag
/// began or ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DragEvent {
    Started,
    Dropped,
}

/// Drag source for one item of a list. It watches the pointer without ever
/// capturing it, so the item keeps its own click behaviour and every other
/// control still sees the same events. There is nothing to configure, so the
/// gesture and its state are one value.
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ItemDrag {
    #[field(get = is_held, vis = "pub(crate)")]
    held: bool,
    origin: Option<Pt>,
    active: bool,
}

impl ItemDrag {
    /// Pointer travel that turns a press on an item into a drag; below it the
    /// press stays a plain click.
    const THRESHOLD: f32 = 4.0;

    pub(crate) fn on_input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<DragEvent> {
        match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down && hit.over() => {
                *self = Self {
                    held: true,
                    ..Self::default()
                };
                Outcome::IGNORED
            }
            Input::Pointer(pointer)
                if pointer.phase == PointerPhase::Move
                    && pointer.at.is_some()
                    && self.held
                    && !self.active =>
            {
                let Some(at) = pointer.at else {
                    return Outcome::IGNORED;
                };
                let Some(origin) = self.origin else {
                    self.origin = Some(at);
                    return Outcome::IGNORED;
                };
                if at.distance(origin) < Self::THRESHOLD {
                    return Outcome::IGNORED;
                }
                self.active = true;
                Outcome::observed(DragEvent::Started)
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up => {
                let dragging = mem::take(self).active;
                if dragging {
                    Outcome::observed(DragEvent::Dropped)
                } else {
                    Outcome::IGNORED
                }
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        }
    }

    pub(crate) const fn cursor(&self) -> CursorShape {
        if self.active {
            CursorShape::Grabbing
        } else {
            CursorShape::None
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{draw::Rect, interact::mouse as mouse_input};

    fn row() -> Rect {
        Rect {
            h: 26.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        }
    }

    fn at(x: f32) -> Hit {
        Hit::new(Some(Pt { x, y: 13.0 }), row())
    }

    /// The host reports a move while telling this widget it has no cursor.
    fn gone() -> Hit {
        Hit::new(None, row())
    }

    fn moved(x: f32) -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x, y: 13.0 })))
    }

    fn pointer(phase: PointerPhase) -> Input<'static> {
        Input::Pointer(mouse_input(phase, None))
    }

    #[kithara::test]
    fn item_drag_starts_past_the_threshold_and_ends_on_release() {
        let mut drag = ItemDrag::default();

        assert_eq!(
            drag.on_input(pointer(PointerPhase::Down), &at(10.0)),
            Outcome::IGNORED
        );
        assert_eq!(
            drag.on_input(moved(11.0), &at(11.0)),
            Outcome::IGNORED,
            "the first move only fixes the point the travel is measured from"
        );
        assert_eq!(
            drag.on_input(moved(13.0), &at(13.0)),
            Outcome::IGNORED,
            "travel below the threshold stays a click"
        );
        assert_eq!(
            drag.on_input(moved(40.0), &at(40.0)),
            Outcome::observed(DragEvent::Started),
            "crossing the threshold starts the drag without taking the pointer"
        );
        assert_eq!(
            drag.on_input(moved(80.0), &at(80.0)),
            Outcome::IGNORED,
            "a drag starts once"
        );
        assert_eq!(
            drag.on_input(pointer(PointerPhase::Up), &at(80.0)),
            Outcome::observed(DragEvent::Dropped)
        );
    }

    #[kithara::test]
    fn item_drag_starts_after_the_pointer_has_left_the_item() {
        let mut drag = ItemDrag::default();

        assert_eq!(
            drag.on_input(pointer(PointerPhase::Down), &at(8.0)),
            Outcome::IGNORED
        );
        assert_eq!(drag.on_input(moved(300.0), &gone()), Outcome::IGNORED);
        assert_eq!(
            drag.on_input(moved(340.0), &gone()),
            Outcome::observed(DragEvent::Started),
            "the drag follows the event, not the hit, so it starts away from the item"
        );
        assert_eq!(
            drag.on_input(pointer(PointerPhase::Up), &gone()),
            Outcome::observed(DragEvent::Dropped)
        );
    }

    #[kithara::test]
    fn a_press_restarts_the_gesture() {
        let mut drag = ItemDrag::default();

        drag.on_input(pointer(PointerPhase::Down), &at(10.0));
        drag.on_input(moved(10.0), &at(10.0));
        assert_eq!(
            drag.on_input(moved(60.0), &at(60.0)),
            Outcome::observed(DragEvent::Started)
        );

        assert_eq!(
            drag.on_input(pointer(PointerPhase::Down), &at(10.0)),
            Outcome::IGNORED
        );
        assert_eq!(
            drag.on_input(pointer(PointerPhase::Up), &at(10.0)),
            Outcome::IGNORED,
            "the new gesture never became a drag, so its release is a click"
        );
    }

    #[kithara::test]
    fn item_drag_is_silent_without_a_press_on_the_item() {
        let mut drag = ItemDrag::default();
        let away = at(600.0);

        for input in [
            pointer(PointerPhase::Down),
            moved(620.0),
            pointer(PointerPhase::Up),
        ] {
            assert_eq!(drag.on_input(input, &away), Outcome::IGNORED);
        }
    }

    #[kithara::test]
    fn the_cursor_grabs_only_while_the_drag_runs() {
        let mut drag = ItemDrag::default();

        drag.on_input(pointer(PointerPhase::Down), &at(10.0));
        drag.on_input(moved(10.0), &at(10.0));
        assert_eq!(
            drag.cursor(),
            CursorShape::None,
            "a press that has not travelled is still a click"
        );

        drag.on_input(moved(60.0), &at(60.0));
        assert_eq!(drag.cursor(), CursorShape::Grabbing);

        drag.on_input(pointer(PointerPhase::Up), &at(60.0));
        assert_eq!(drag.cursor(), CursorShape::None);
    }
}
