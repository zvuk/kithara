use crate::{expand::DropSpec, ids::InternId, layout::FrameSides, module::ChromeStyle};

/// Resolved toolkit-neutral description of one compiled module shell.
#[derive(Debug)]
#[non_exhaustive]
pub struct Module<'a> {
    pub(super) instance: InternId,
    pub(super) module: InternId,
    pub(super) title: Option<InternId>,
    pub(super) chip: Option<InternId>,
    pub(super) assign: &'a [InternId],
    pub(super) chrome: ChromeStyle,
    pub(super) frame: FrameSides,
    pub(super) corners: bool,
    pub(super) footer: Option<String>,
    pub(super) drop: Option<&'a DropSpec>,
    pub(super) collapsed: bool,
    pub(super) chrome_hosted: bool,
}

impl Module<'_> {
    /// Compiled instance identifier.
    #[must_use]
    pub const fn instance(&self) -> InternId {
        self.instance
    }

    /// Compiled module identifier.
    #[must_use]
    pub const fn module(&self) -> InternId {
        self.module
    }

    /// Optional title identifier.
    #[must_use]
    pub const fn title(&self) -> Option<InternId> {
        self.title
    }

    /// Optional chip identifier.
    #[must_use]
    pub const fn chip(&self) -> Option<InternId> {
        self.chip
    }

    /// Assignment label identifiers.
    #[must_use]
    pub const fn assign(&self) -> &[InternId] {
        self.assign
    }

    /// Module chrome style.
    #[must_use]
    pub const fn chrome(&self) -> ChromeStyle {
        self.chrome
    }

    /// Module frame sides.
    #[must_use]
    pub const fn frame(&self) -> FrameSides {
        self.frame
    }

    /// Whether decorative frame corners are drawn.
    #[must_use]
    pub const fn corners(&self) -> bool {
        self.corners
    }

    /// Takes the resolved footer text.
    #[must_use]
    pub fn take_footer(&mut self) -> Option<String> {
        self.footer.take()
    }

    /// Optional compiled drop target.
    #[must_use]
    pub const fn drop(&self) -> Option<&DropSpec> {
        self.drop
    }

    /// Whether the module body is collapsed.
    #[must_use]
    pub const fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Whether chrome interaction is owned by a retained host.
    #[must_use]
    pub const fn chrome_hosted(&self) -> bool {
        self.chrome_hosted
    }
}
