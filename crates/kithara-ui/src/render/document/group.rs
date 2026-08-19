use crate::{
    expand::SurfaceSpec,
    layout::{Axis, FrameSides},
    module::TextAlign,
    size::SizeSpec,
    skin::ColorRole,
};

/// Resolved toolkit-neutral layout and surface description for a row or column.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Group<'a> {
    pub(super) axis: Axis,
    pub(super) alignment: TextAlign,
    pub(super) gap: f32,
    pub(super) padding_x: f32,
    pub(super) padding_y: f32,
    pub(super) frame: Option<FrameSides>,
    pub(super) background: Option<ColorRole>,
    pub(super) background_alpha: Option<f32>,
    pub(super) frame_color: ColorRole,
    pub(super) frame_width: f32,
    pub(super) surface: Option<&'a SurfaceSpec>,
    pub(super) size: Option<SizeSpec>,
}

impl Group<'_> {
    /// Main layout axis.
    #[must_use]
    pub const fn axis(&self) -> Axis {
        self.axis
    }

    /// Cross-axis child alignment.
    #[must_use]
    pub const fn alignment(&self) -> TextAlign {
        self.alignment
    }

    /// Gap between adjacent visible children.
    #[must_use]
    pub const fn gap(&self) -> f32 {
        self.gap
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

    /// Sides carrying the document frame.
    #[must_use]
    pub const fn frame(&self) -> Option<FrameSides> {
        self.frame
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

    /// Optional wheel surface attached to the group.
    #[must_use]
    pub const fn surface(&self) -> Option<&SurfaceSpec> {
        self.surface
    }

    /// Effective document size, when one is declared or intrinsic.
    #[must_use]
    pub const fn size(&self) -> Option<SizeSpec> {
        self.size
    }
}
