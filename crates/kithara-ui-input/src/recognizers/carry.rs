use kithara_ui_draw::Pt;

use super::super::{CursorShape, Hit, Input, Outcome, PointerOwnership, PointerPhase};

/// Press-and-move that carries one placement of a scene.
///
/// It answers in the space the hit is expressed in: the press fixes where
/// inside the placement the pointer took hold, and every move afterwards says
/// where that placement's corner has to be for the pointer to stay on the same
/// spot of it. Which corner that is in a scene, and whether a magnet moves it
/// somewhere else, belongs to whoever mounted the placement.
#[derive(Default)]
pub struct Carry {
    /// Where the press landed, from the placement's own corner.
    grab: Option<Pt>,
}

impl Carry {
    fn corner(&self, hit: &Hit) -> Option<Pt> {
        let grab = self.grab?;
        let at = hit.at()?;
        Some(Pt {
            x: at.x - grab.x,
            y: at.y - grab.y,
        })
    }

    #[must_use]
    pub const fn cursor(&self) -> CursorShape {
        if self.grab.is_some() {
            CursorShape::Grabbing
        } else {
            CursorShape::Grab
        }
    }

    /// Whether a pointer is carrying this placement right now.
    #[must_use]
    pub const fn is_carried(&self) -> bool {
        self.grab.is_some()
    }

    pub fn on_input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<Pt> {
        match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                let Some(at) = hit.inside() else {
                    return Outcome::IGNORED;
                };
                let area = hit.area();
                self.grab = Some(Pt {
                    x: at.x - area.x,
                    y: at.y - area.y,
                });
                Outcome::captured().with_ownership(PointerOwnership::Claim)
            }
            Input::Pointer(pointer)
                if matches!(
                    pointer.phase,
                    PointerPhase::Move | PointerPhase::MoveLongPress
                ) =>
            {
                self.corner(hit).map_or(Outcome::IGNORED, Outcome::set)
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up => {
                let corner = self.corner(hit);
                self.grab = None;
                corner.map_or_else(
                    || Outcome::IGNORED.with_ownership(PointerOwnership::Release),
                    |corner| Outcome::set(corner).with_ownership(PointerOwnership::Release),
                )
            }
            Input::Pointer(pointer)
                if matches!(pointer.phase, PointerPhase::Cancel | PointerPhase::Leave) =>
            {
                self.grab = None;
                Outcome::IGNORED.with_ownership(PointerOwnership::Release)
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
