use super::super::{Hit, Input, Outcome, PointerPhase};

/// A press that lands on the control and asks it to act. There is no state and
/// nothing to configure: where the press landed is the whole gesture, so a
/// later event has nothing to change and the cursor rule stays with [`Hover`].
///
/// [`Hover`]: super::super::Hover
#[must_use]
pub fn on_input(input: Input<'_>, hit: &Hit) -> Outcome<()> {
    match input {
        Input::Pointer(pointer) if pointer.phase == PointerPhase::Down && hit.over() => {
            Outcome::set(())
        }
        Input::InputMethod(_)
        | Input::KeyPressed { .. }
        | Input::KeyReleased { .. }
        | Input::ModifiersChanged(_)
        | Input::Pointer(_)
        | Input::Wheel(_) => Outcome::IGNORED,
    }
}
