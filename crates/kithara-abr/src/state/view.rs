use kithara_events::AbrVariant;

use crate::controller::AbrSettings;

/// Snapshot of the inputs an [`AbrState`](super::AbrState) needs to make a decision.
pub struct AbrView<'a> {
    pub settings: &'a AbrSettings,
    pub variants: &'a [AbrVariant],
    pub buffer_ahead: Option<std::time::Duration>,
    pub estimate_bps: Option<u64>,
    pub bytes_downloaded: u64,
}
