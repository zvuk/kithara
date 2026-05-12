#![forbid(unsafe_code)]

use kithara_abr::{AbrDecision, AbrState};
use kithara_events::AbrReason;
use kithara_platform::time::Instant;

/// Pin the ABR state to a fixed variant via the documented production
/// write path (`AbrState::apply` with a `ManualOverride` decision). Same
/// effect as the controller applying a manual mode change, without needing
/// a controller tick.
pub fn pin_abr_variant(abr: &AbrState, idx: usize) {
    abr.apply(
        &AbrDecision {
            reason: AbrReason::ManualOverride,
            did_change: true,
            target_variant_index: idx,
        },
        Instant::now(),
    );
}
