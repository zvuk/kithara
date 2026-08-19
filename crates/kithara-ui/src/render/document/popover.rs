use crate::{
    ids::InternId,
    module::{PopoverAlign, PopoverAt},
    size::SizeSpec,
};

/// Resolved toolkit-neutral placement of one document popover.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Popover {
    pub(super) path: InternId,
    pub(super) at: PopoverAt,
    pub(super) align: PopoverAlign,
    pub(super) open: bool,
    pub(super) size: Option<SizeSpec>,
}

impl Popover {
    /// Event path published by the anchor.
    #[must_use]
    pub const fn path(&self) -> InternId {
        self.path
    }

    /// Geometry the overlay opens from.
    #[must_use]
    pub const fn at(&self) -> PopoverAt {
        self.at
    }

    /// Edge alignment against the opening geometry.
    #[must_use]
    pub const fn align(&self) -> PopoverAlign {
        self.align
    }

    /// Whether overlay content is currently present.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Effective in-flow size, inherited from the anchor.
    #[must_use]
    pub const fn size(&self) -> Option<SizeSpec> {
        self.size
    }
}
