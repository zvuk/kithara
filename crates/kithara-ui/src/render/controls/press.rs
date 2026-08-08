use crate::{
    atoms::button::VisualState,
    interact::{Hit, Input, PointerPhase},
};

/// What the pointer is doing to a control right now, for the painters that draw
/// differently under it.
///
/// One tracker for both hosts: what a hover or a press looks like is settled in
/// the painter, so when the pointer counts as resting on or pushing a control
/// has to be settled in one place too. The two fields are written through
/// separate doors because the hosts learn about them differently — a retained
/// host is told about crossing out of band, an immediate one reads it off the
/// same input stream as the press.
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Press {
    hovered: bool,
    #[field(get = is_pressed, vis = "pub(crate)", copy)]
    pressed: bool,
}

impl Press {
    pub(crate) const fn visual(&self) -> VisualState {
        if self.pressed && self.hovered {
            VisualState::Pressed
        } else if self.hovered {
            VisualState::Hovered
        } else {
            VisualState::Idle
        }
    }

    /// Follows the pointer's button through one input, answering whether that
    /// changed the picture.
    pub(crate) fn press(&mut self, input: Input<'_>, hit: &Hit) -> bool {
        let Input::Pointer(pointer) = input else {
            return false;
        };
        let pressed = match pointer.phase {
            PointerPhase::Down => hit.over(),
            PointerPhase::Cancel
            | PointerPhase::DoubleClick
            | PointerPhase::Leave
            | PointerPhase::Up => false,
            PointerPhase::LongPress | PointerPhase::Move | PointerPhase::MoveLongPress => {
                self.pressed
            }
        };
        std::mem::replace(&mut self.pressed, pressed) != pressed
    }

    /// Records where the pointer is relative to the control, answering whether
    /// that changed the picture.
    pub(crate) fn hover(&mut self, hovered: bool) -> bool {
        std::mem::replace(&mut self.hovered, hovered) != hovered
    }
}
