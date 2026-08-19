use crate::draw::Pt;

/// Stable identity of one pointer for the duration of its gesture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PointerId(pub u64);

/// Identity used by hosts that expose one mouse pointer.
pub const MOUSE: PointerId = PointerId(0);

/// Toolkit-neutral pointer button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PointerButton {
    /// Primary selection button.
    Primary,
    /// Secondary/context button.
    Secondary,
    /// Auxiliary button, commonly the wheel button.
    Auxiliary,
    /// Navigation back button.
    Back,
    /// Navigation forward button.
    Forward,
    /// A host-defined button number outside the portable set.
    Other(u32),
}

/// Raw and recognized stages of a pointer gesture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PointerPhase {
    /// A button began a gesture.
    Down,
    /// The pointer moved without a recognized long press.
    Move,
    /// A button ended a gesture.
    Up,
    /// The pointer left the host surface.
    Leave,
    /// The host cancelled the gesture.
    Cancel,
    /// A completed click was recognized as the second click of a pair. This is
    /// the terminal phase for that press; consumers release or reset retained
    /// gesture state here just as they do on [`Self::Up`].
    DoubleClick,
    /// A held press crossed the host's long-press threshold.
    LongPress,
    /// The pointer moved after a long press was recognized.
    MoveLongPress,
}

/// One toolkit-neutral pointer input packet.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct PointerInput {
    /// Stable identity of the pointer that produced this event.
    pub id: PointerId,
    /// Button changed by this event. Motion and lifecycle events carry `None`.
    pub button: Option<PointerButton>,
    /// Raw or recognized phase reported by the host.
    pub phase: PointerPhase,
    /// Host-space logical point, when the source event reports one.
    pub at: Option<Pt>,
    /// Recognized click count. Hosts without multi-click data report one.
    pub clicks: u8,
}

impl PointerInput {
    /// Builds one neutral pointer event without toolkit-specific types.
    #[must_use]
    pub const fn new(
        id: PointerId,
        button: Option<PointerButton>,
        phase: PointerPhase,
        at: Option<Pt>,
        clicks: u8,
    ) -> Self {
        Self {
            id,
            button,
            phase,
            at,
            clicks,
        }
    }
}

pub(crate) const fn mouse(phase: PointerPhase, at: Option<Pt>) -> PointerInput {
    let button = if matches!(phase, PointerPhase::Down | PointerPhase::Up) {
        Some(PointerButton::Primary)
    } else {
        None
    };
    PointerInput::new(MOUSE, button, phase, at, 1)
}
