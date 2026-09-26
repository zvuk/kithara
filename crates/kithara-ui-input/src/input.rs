use kithara_ui_draw::{Pt, Rect};

use super::{modifiers::Modifiers, pointer::PointerInput};

/// Toolkit-neutral input delivered to a custom component.
#[derive(Clone, Copy, Debug, PartialEq)]
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
pub enum ScrollAxis {
    Horizontal,
    Vertical,
}

/// Toolkit-neutral line or pixel scroll delta.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scroll {
    Lines { x: f32, y: f32 },
    Pixels { x: f32, y: f32 },
}

impl Scroll {
    #[must_use]
    pub const fn delta(self, axis: ScrollAxis) -> f32 {
        let (x, y) = match self {
            Self::Lines { x, y } | Self::Pixels { x, y } => (x, y),
        };
        match axis {
            ScrollAxis::Horizontal => x,
            ScrollAxis::Vertical => y,
        }
    }

    #[must_use]
    pub const fn is_pixels(self) -> bool {
        matches!(self, Self::Pixels { .. })
    }

    #[must_use]
    pub const fn lines(y: f32) -> Self {
        Self::Lines { y, x: 0.0 }
    }

    #[must_use]
    pub const fn y(self) -> f32 {
        self.delta(ScrollAxis::Vertical)
    }
}

/// Pointer position paired with the component area used for hit testing.
///
/// The point and area always share one coordinate space. A retained engine may
/// use host space, while a custom leaf receives its own local space.
#[derive(Clone, Copy, Debug, PartialEq)]
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

    /// The box the pointer is tested against, for a recognizer that normalizes
    /// a position against it rather than only asking whether it landed inside.
    #[must_use]
    pub const fn area(self) -> Rect {
        self.area
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

    #[must_use]
    pub fn uniform_horizontal_index(self, count: usize) -> Option<usize> {
        self.area.uniform_horizontal_index(self.inside()?, count)
    }
}
