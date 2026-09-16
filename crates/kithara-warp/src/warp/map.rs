use super::WarpCursor;
use crate::{SessionBeat, SessionFrame, WarpMapRevision};

/// One immutable session-output-to-source map revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct WarpMap {
    /// Immutable owner-assigned map revision.
    #[field(get, copy)]
    revision: WarpMapRevision,
}

impl WarpMap {
    /// Creates an immutable identity-map revision.
    #[must_use]
    pub const fn identity(revision: WarpMapRevision) -> Self {
        Self { revision }
    }

    /// Creates renderer-local progress at an exact discontinuity boundary.
    #[must_use]
    pub const fn reanchor(
        &self,
        source: u64,
        output: SessionFrame,
        beat: SessionBeat,
    ) -> WarpCursor {
        WarpCursor::new(self.revision, source, output, beat)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn reanchor_carries_the_map_revision_and_exact_frontier() {
        let revision = WarpMapRevision::first();
        let map = WarpMap::identity(revision);
        let beat = SessionBeat::new(3.0).expect("fixture beat is finite");
        let cursor = map.reanchor(80, SessionFrame::new(120), beat);

        assert_eq!(map.revision(), revision);
        assert_eq!(cursor.revision(), revision);
        assert_eq!(cursor.source(), 80);
        assert_eq!(cursor.output(), SessionFrame::new(120));
        assert_eq!(cursor.beat(), beat);
    }
}
