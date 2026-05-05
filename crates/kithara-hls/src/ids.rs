#![forbid(unsafe_code)]

pub(crate) type VariantIndex = usize;
pub(crate) type SegmentIndex = usize;

/// Distinguishes media segments by index in download plans.
///
/// The cursor tracks `Media` segment positions only — init fetching is
/// orthogonal and controlled by `need_init` on the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SegmentId {
    /// Media segment at the given index.
    Media(SegmentIndex),
}
