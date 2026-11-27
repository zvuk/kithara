use kithara_ui::draw::{Pt, Rect};

/// What a frame is asked to answer for. A drag also presses, so the classes are
/// tested widest-first wherever one has to be chosen.
#[derive(Clone, Copy)]
pub(crate) enum Interaction {
    Drag,
    Press,
    /// A press on the leading part of the box rather than the middle of it.
    PressLeading,
    DataChange,
}

/// One neutral step both hosts translate into their own event vocabulary.
#[derive(Clone, Copy)]
pub(crate) enum Step {
    Move(Pt),
    Press,
    Release,
    Data,
}

impl Interaction {
    const START: (f32, f32) = (0.25, 0.25);
    const MIDDLE: (f32, f32) = (0.5, 0.5);
    const END: (f32, f32) = (0.75, 0.75);
    const LEADING: (f32, f32) = (0.2, 0.5);

    pub(crate) fn steps(self, rect: Rect) -> Vec<Step> {
        let at = |fraction| Step::Move(inside(rect, fraction));
        match self {
            Self::Drag => vec![
                at(Self::START),
                Step::Press,
                at(Self::MIDDLE),
                at(Self::END),
                Step::Release,
            ],
            Self::Press => vec![at(Self::MIDDLE), Step::Press, Step::Release],
            Self::PressLeading => vec![at(Self::LEADING), Step::Press, Step::Release],
            Self::DataChange => vec![Step::Data],
        }
    }
}

pub(crate) fn inside(rect: Rect, (x, y): (f32, f32)) -> Pt {
    Pt {
        x: rect.x + rect.w * x,
        y: rect.y + rect.h * y,
    }
}

/// One control, mounted alone, and the interaction its frame is driven by.
pub(crate) struct Scenario {
    pub(crate) control: &'static str,
    pub(crate) name: &'static str,
    pub(crate) interaction: Interaction,
}
