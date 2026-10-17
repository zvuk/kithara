use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};

/// The bar the grid claims, stated only where its phase is provable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Meter {
    pub beats_per_bar: NonZeroU16,
    pub origin_beat_ordinal: i64,
}
