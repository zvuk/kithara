use crate::{
    expand::{Binding, SurfaceSpec},
    layout::{Axis, FrameCorners, FrameSides},
    module::{MeasureAxis, TextAlign},
    size::SizeSpec,
    skin::ColorRole,
};

/// Resolved toolkit-neutral layout and surface description for a row or column.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Group<'a> {
    pub(super) axis: Axis,
    pub(super) frame_color: ColorRole,
    /// The window corners this group stands at, which only the root of a module
    /// with no shell of its own ever has.
    pub(super) round: FrameCorners,
    pub(super) background: Option<ColorRole>,
    pub(super) background_alpha: Option<f32>,
    /// The face this group shows instead while the flag it names reads true.
    pub(super) lit: Option<Lit<'a>>,
    pub(super) frame: Option<FrameSides>,
    /// The axis whose room decides which of its children stand, when the
    /// document says its children come and go with the room.
    pub(super) measure: Option<MeasureAxis>,
    pub(super) size: Option<SizeSpec>,
    pub(super) surface: Option<&'a SurfaceSpec>,
    pub(super) alignment: TextAlign,
    pub(super) frame_width: f32,
    pub(super) gap: f32,
    pub(super) padding_x: f32,
    pub(super) padding_y: f32,
}

impl Group<'_> {
    /// Cross-axis child alignment.
    #[must_use]
    pub const fn alignment(&self) -> TextAlign {
        self.alignment
    }

    /// Main layout axis.
    #[must_use]
    pub const fn axis(&self) -> Axis {
        self.axis
    }

    /// Resolved background role.
    #[must_use]
    pub const fn background(&self) -> Option<ColorRole> {
        self.background
    }

    /// Optional background alpha override.
    #[must_use]
    pub const fn background_alpha(&self) -> Option<f32> {
        self.background_alpha
    }

    /// Sides carrying the document frame.
    #[must_use]
    pub const fn frame(&self) -> Option<FrameSides> {
        self.frame
    }

    /// Resolved frame colour role.
    #[must_use]
    pub const fn frame_color(&self) -> ColorRole {
        self.frame_color
    }

    /// Frame width in logical pixels.
    #[must_use]
    pub const fn frame_width(&self) -> f32 {
        self.frame_width
    }

    /// Gap between adjacent visible children.
    #[must_use]
    pub const fn gap(&self) -> f32 {
        self.gap
    }

    /// The face this group shows while the flag it names reads true, and the
    /// flag. A group that names no flag has one face and answers `None`.
    #[must_use]
    pub const fn lit(&self) -> Option<Lit<'_>> {
        self.lit
    }

    /// The axis whose room decides which children stand, when they do.
    #[must_use]
    pub const fn measure(&self) -> Option<MeasureAxis> {
        self.measure
    }

    /// Resolved horizontal padding.
    #[must_use]
    pub const fn padding_x(&self) -> f32 {
        self.padding_x
    }

    /// Resolved vertical padding.
    #[must_use]
    pub const fn padding_y(&self) -> f32 {
        self.padding_y
    }

    /// The window corners this group stands at.
    #[must_use]
    pub const fn round(&self) -> FrameCorners {
        self.round
    }

    /// Effective document size, when one is declared or intrinsic.
    #[must_use]
    pub const fn size(&self) -> Option<SizeSpec> {
        self.size
    }

    /// Optional wheel surface attached to the group.
    #[must_use]
    pub const fn surface(&self) -> Option<&SurfaceSpec> {
        self.surface
    }
}

/// The other face of a group, and the flag that decides which one stands.
///
/// The flag travels rather than its reading, because the two kinds of host read
/// it at different moments: one resolves it afresh on every frame it draws, the
/// other in place, into a tree it keeps across frames. A host handed only the
/// reading freezes the face the document was mounted at.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Lit<'a> {
    pub(super) flag: &'a Binding,
    pub(super) background: Option<ColorRole>,
    pub(super) frame_color: ColorRole,
}

impl<'a> Lit<'a> {
    /// Background role while the flag reads true.
    #[must_use]
    pub const fn background(&self) -> Option<ColorRole> {
        self.background
    }

    /// The flag that decides between the two faces.
    #[must_use]
    pub const fn flag(&self) -> &'a Binding {
        self.flag
    }

    /// Frame colour role while the flag reads true.
    #[must_use]
    pub const fn frame_color(&self) -> ColorRole {
        self.frame_color
    }
}
