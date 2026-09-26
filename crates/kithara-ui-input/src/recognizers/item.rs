use std::mem;

use kithara_ui_draw::Pt;

use super::super::{CursorShape, Hit, Input, Outcome, PointerPhase};

/// What a press-and-pull on one item of a list amounts to. Which item it was
/// stays with whoever owns the list; the recognizer reports only that a drag
/// began or ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragEvent {
    Started,
    Dropped,
}

/// Drag source for one item of a list. It watches the pointer without ever
/// capturing it, so the item keeps its own click behaviour and every other
/// control still sees the same events. There is nothing to configure, so the
/// gesture and its state are one value.
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct ItemDrag {
    origin: Option<Pt>,
    active: bool,
    #[field(get = is_held)]
    held: bool,
}

impl ItemDrag {
    /// Pointer travel that turns a press on an item into a drag; below it the
    /// press stays a plain click.
    const THRESHOLD: f32 = 4.0;

    #[must_use]
    pub const fn cursor(&self) -> CursorShape {
        if self.active {
            CursorShape::Grabbing
        } else {
            CursorShape::None
        }
    }

    pub fn on_input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<DragEvent> {
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
}
