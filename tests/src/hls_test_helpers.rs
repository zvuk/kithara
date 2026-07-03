#![forbid(unsafe_code)]

use kithara::{
    abr::AbrHandle,
    events::{AbrMode, VariantIndex},
};

/// Pin the ABR state to a fixed variant by switching the handle into
/// `Manual` mode — same effect as a user-driven mode change without
/// needing a controller tick.
pub fn pin_abr_variant(abr: &AbrHandle, idx: usize) {
    abr.set_mode(AbrMode::Manual(VariantIndex::new(idx)))
        .expect("pin_abr_variant: variant index out of bounds");
}
