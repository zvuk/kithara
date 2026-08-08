/// Chooses which font collections may answer a shaping request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FontPolicy {
    /// Use only the deterministic embedded catalog.
    Embedded,
    /// Use the embedded catalog and machine-owned system fonts.
    System,
}

impl FontPolicy {
    pub(super) const fn system_fonts(self) -> bool {
        matches!(self, Self::System)
    }
}
