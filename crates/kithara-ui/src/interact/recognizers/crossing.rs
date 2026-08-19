use super::super::{Hit, Input, Outcome, PointerPhase};

#[derive(Default)]
pub(crate) struct Crossing {
    over: bool,
}

impl Crossing {
    pub(crate) fn on_input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<bool> {
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{
        super::super::{Hit, Input, Modifiers, Outcome, PointerPhase, mouse as mouse_input},
        Crossing,
    };
    use crate::draw::{Pt, Rect};

    fn hit(x: f32) -> Hit {
        Hit::new(
            Some(Pt { x, y: 10.0 }),
            Rect {
                h: 20.0,
                w: 20.0,
                x: 0.0,
                y: 0.0,
            },
        )
    }

    fn moved() -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x: 0.0, y: 0.0 })))
    }

    #[kithara::test]
    fn crossing_observes_each_boundary_once_without_capture() {
        let mut crossing = Crossing::default();

        assert_eq!(crossing.on_input(moved(), &hit(30.0)), Outcome::IGNORED);
        let entered = crossing.on_input(moved(), &hit(10.0));
        assert_eq!(entered, Outcome::observed(true));
        assert!(!entered.is_captured());
        assert_eq!(crossing.on_input(moved(), &hit(11.0)), Outcome::IGNORED);
        assert_eq!(
            crossing.on_input(Input::ModifiersChanged(Modifiers::default()), &hit(11.0),),
            Outcome::IGNORED
        );
        let exited = crossing.on_input(moved(), &hit(30.0));
        assert_eq!(exited, Outcome::observed(false));
        assert!(!exited.is_captured());
        assert_eq!(crossing.on_input(moved(), &hit(31.0)), Outcome::IGNORED);
    }

    #[kithara::test]
    fn leaving_the_pointer_surface_exits_once() {
        let mut crossing = Crossing::default();
        assert_eq!(
            crossing.on_input(moved(), &hit(10.0)),
            Outcome::observed(true)
        );

        assert_eq!(
            crossing.on_input(
                Input::Pointer(mouse_input(PointerPhase::Leave, None)),
                &hit(10.0)
            ),
            Outcome::observed(false)
        );
        assert_eq!(
            crossing.on_input(
                Input::Pointer(mouse_input(PointerPhase::Leave, None)),
                &hit(10.0)
            ),
            Outcome::IGNORED
        );
    }
}
