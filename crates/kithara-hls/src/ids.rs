#![forbid(unsafe_code)]

pub(crate) type VariantIndex = usize;
pub(crate) type SegmentIndex = usize;

/// Distinguishes init segments from media segments in download plans.
///
/// The cursor tracks `Media` segment positions only — init fetching is
/// orthogonal, controlled by `need_init` on the plan or by a separate
/// `SegmentId::Init` plan entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SegmentId {
    /// Variant initialization segment (moov / codec configuration).
    #[expect(dead_code, reason = "reserved for standalone init-only fetch plans")]
    Init,
    /// Media segment at the given index.
    Media(SegmentIndex),
}

impl SegmentId {
    /// Extract the media segment index, or `None` for init segments.
    pub(crate) fn media_index(self) -> Option<SegmentIndex> {
        match self {
            Self::Media(i) => Some(i),
            Self::Init => None,
        }
    }
}
