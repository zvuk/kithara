use super::{modifiers::Modifiers, pointer::PointerInput};
use crate::draw::{Pt, Rect};

/// The input vocabulary observed from a mounted control.
///
/// The gesture census keeps this test-only: production truth remains in the
/// recognizers, and both host adapters report what they actually constructed.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Gestures {
    pub(crate) double_click: bool,
    pub(crate) drag: bool,
    pub(crate) keyboard: bool,
    pub(crate) press: bool,
    pub(crate) wheel: bool,
}

#[cfg(test)]
impl Gestures {
    pub(crate) const NONE: Self = Self {
        double_click: false,
        drag: false,
        keyboard: false,
        press: false,
        wheel: false,
    };

    pub(crate) const PRESS: Self = Self {
        press: true,
        ..Self::NONE
    };

    pub(crate) const DRAG: Self = Self {
        drag: true,
        press: true,
        ..Self::NONE
    };

    pub(crate) const fn union(self, other: Self) -> Self {
        Self {
            double_click: self.double_click || other.double_click,
            drag: self.drag || other.drag,
            keyboard: self.keyboard || other.keyboard,
            press: self.press || other.press,
            wheel: self.wheel || other.wheel,
        }
    }
}

/// Toolkit-neutral input delivered to a custom component.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Input<'a> {
    KeyPressed {
        key: Key<'a>,
        modifiers: Modifiers,
        text: Option<&'a str>,
    },
    KeyReleased {
        key: Key<'a>,
        modifiers: Modifiers,
    },
    InputMethod(InputMethod<'a>),
    ModifiersChanged(Modifiers),
    /// `PointerInput::at` is where the host says the pointer went. It answers a
    /// different question from [`Hit::at`] and the two are not interchangeable:
    /// this one is reported independently of hit testing and is only comparable
    /// against itself. A recognizer measuring travel reads it; one normalizing
    /// against an area must read the hit, expressed in that area's space.
    Pointer(PointerInput),
    Wheel(Scroll),
}

/// Toolkit-neutral keyboard key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Key<'a> {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    Backspace,
    Delete,
    End,
    Enter,
    Escape,
    Home,
    Space,
    Character(&'a str),
    Other,
}

/// Toolkit-neutral text-input-method event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InputMethod<'a> {
    Opened,
    Preedit {
        content: &'a str,
        selection: Option<(usize, usize)>,
    },
    Commit(&'a str),
    Closed,
}

/// Axis selected from a neutral scroll delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ScrollAxis {
    Horizontal,
    Vertical,
}

/// Toolkit-neutral line or pixel scroll delta.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Scroll {
    Lines { x: f32, y: f32 },
    Pixels { x: f32, y: f32 },
}

impl Scroll {
    pub(crate) const fn lines(y: f32) -> Self {
        Self::Lines { x: 0.0, y }
    }

    #[cfg(test)]
    pub(crate) const fn pixels(y: f32) -> Self {
        Self::Pixels { x: 0.0, y }
    }

    #[cfg(test)]
    pub(crate) const fn x(self) -> f32 {
        self.delta(ScrollAxis::Horizontal)
    }

    pub(crate) const fn y(self) -> f32 {
        self.delta(ScrollAxis::Vertical)
    }

    pub(crate) const fn delta(self, axis: ScrollAxis) -> f32 {
        let (x, y) = match self {
            Self::Lines { x, y } | Self::Pixels { x, y } => (x, y),
        };
        match axis {
            ScrollAxis::Horizontal => x,
            ScrollAxis::Vertical => y,
        }
    }

    pub(crate) const fn is_pixels(self) -> bool {
        matches!(self, Self::Pixels { .. })
    }
}

/// Pointer position paired with the component area used for hit testing.
///
/// The point and area always share one coordinate space. A retained engine may
/// use host space, while a custom leaf receives its own local space.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Hit {
    at: Option<Pt>,
    area: Rect,
}

impl Hit {
    /// Pairs an optional point with this component's area in the same space.
    #[must_use]
    pub const fn new(at: Option<Pt>, area: Rect) -> Self {
        Self { at, area }
    }

    /// The pointer wherever it is, in or out of the area.
    ///
    /// A gesture already under way tracks it past the edge, which is why this
    /// is separate from [`Self::inside`].
    #[must_use]
    pub const fn at(self) -> Option<Pt> {
        self.at
    }

    /// The pointer, only while it is within the area.
    ///
    /// A recognizer that starts a gesture needs the position and needs it to
    /// be inside, so one call answers both and leaves no unreachable arm.
    #[must_use]
    pub fn inside(self) -> Option<Pt> {
        self.at.filter(|point| self.area.contains(*point))
    }

    #[must_use]
    pub fn over(self) -> bool {
        self.inside().is_some()
    }

    pub(crate) fn uniform_horizontal_index(self, count: usize) -> Option<usize> {
        self.area.uniform_horizontal_index(self.inside()?, count)
    }

    /// The box the pointer is tested against, for a recognizer that normalizes
    /// a position against it rather than only asking whether it landed inside.
    #[must_use]
    pub const fn area(self) -> Rect {
        self.area
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::interact::{PointerButton, PointerId, PointerInput, PointerPhase};

    #[kithara::test]
    fn pointer_input_preserves_identity_button_phase_position_and_clicks() {
        let pointer = PointerInput::new(
            PointerId(17),
            Some(PointerButton::Secondary),
            PointerPhase::DoubleClick,
            Some(Pt { x: 13.0, y: 21.0 }),
            2,
        );

        let Input::Pointer(decoded) = Input::Pointer(pointer) else {
            panic!("pointer input must stay in the neutral pointer vocabulary");
        };
        assert_eq!(decoded, pointer);
    }

    #[kithara::test]
    fn pointer_phase_names_every_required_recognized_stage() {
        assert_eq!(
            [
                PointerPhase::Down,
                PointerPhase::Move,
                PointerPhase::Up,
                PointerPhase::Leave,
                PointerPhase::Cancel,
                PointerPhase::DoubleClick,
                PointerPhase::LongPress,
                PointerPhase::MoveLongPress,
            ]
            .len(),
            8
        );
    }

    #[kithara::test]
    fn hit_keeps_the_unclamped_point_separate_from_containment() {
        let point = Pt { x: 120.0, y: 25.0 };
        let hit = Hit::new(
            Some(point),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 50.0,
            },
        );

        assert_eq!(hit.at(), Some(point));
        assert_eq!(hit.inside(), None);
    }
}
