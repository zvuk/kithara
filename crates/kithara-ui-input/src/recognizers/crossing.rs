use super::super::{Hit, Input, Outcome, PointerPhase};

#[derive(Default)]
pub struct Crossing {
    over: bool,
}

impl Crossing {
    pub fn on_input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<bool> {
        let over = match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Move => hit.over(),
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Leave => false,
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => return Outcome::IGNORED,
        };
        if self.over == over {
            return Outcome::IGNORED;
        }
        self.over = over;
        Outcome::observed(over)
    }
}
