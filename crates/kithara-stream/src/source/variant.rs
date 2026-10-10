use std::ops::Range;

use kithara_platform::time::Duration;

use crate::{
    StreamResult, VariantPromotion, VariantReaderPlan, VariantReaderTake, VariantTransition,
    profile::ReaderProfile,
};

/// HLS-only variant-coordination surface, vended as `Some` by adaptive
/// sources and `None` by everything else.
///
/// All methods take `&self`; the HLS impl (`HlsCoord`) uses interior
/// mutability, so callers hold an `Arc<dyn VariantControl>` and never
/// need `&mut`.
pub trait VariantControl: Send + Sync + 'static {
    /// Cancel one exact incoming session without disturbing the authoritative
    /// outgoing variant or a newer ABR intent.
    #[must_use]
    fn abort_variant(&self, transition: VariantTransition) -> bool;

    /// Byte range of the header the decoder must re-read after a format
    /// change (HLS ABR cross-codec switch).
    ///
    /// # Errors
    /// `Err(SourceError::FormatChangeNotApplicable)` when the active variant
    /// was served with a non-zero `served_from` so the init prefix lives
    /// outside the virtual range.
    fn format_change_segment_range(&self) -> StreamResult<Range<u64>>;

    /// Capture the exact target media facts needed to select a decoder and
    /// compute its reader profile before an incoming session is opened.
    ///
    /// `landing` is the content time the caller will require the incoming
    /// variant to cover — the decode frontier the splice is proved against, not
    /// the audible position. `None` when the caller has no frontier to name, in
    /// which case the source keeps its own seek-derived target.
    ///
    /// # Errors
    ///
    /// Returns a source error when the pending target cannot be resolved.
    fn plan_variant_reader(
        &self,
        landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>>;

    /// Claim the current exact ABR intent and start preparing its independent
    /// reader session. Repeated calls for the same intent return the same
    /// transition; a newer intent supersedes the previous incoming session.
    ///
    /// # Errors
    ///
    /// Returns a source error when the target session cannot be prepared.
    fn prepare_variant_reader(
        &self,
        plan: VariantReaderPlan,
        profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>>;

    /// Publish the incoming variant only when `transition` still identifies
    /// the exact pending ABR intent and its prepared source session.
    fn promote_variant(&self, transition: VariantTransition) -> VariantPromotion;

    /// Transfer the prepared reader exactly once. The typed result keeps
    /// readiness, prior transfer, and stale identity distinct.
    ///
    /// # Errors
    ///
    /// Returns a source error when preparation ended in a terminal failure.
    fn take_prepared_variant_reader(
        &self,
        transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake>;

    /// Whether the named pending transition is stalled on a source demand
    /// that is actually being serviced — the incoming session's next byte
    /// waits on a planned or in-flight fetch. Drives the audio worker's
    /// hang classification: `true` parks the worker without ticking its
    /// watchdog, `false` keeps a wedged transition visible to it. The
    /// default is the strict answer.
    fn transition_demand_in_flight(&self, _transition: VariantTransition) -> bool {
        false
    }
}
