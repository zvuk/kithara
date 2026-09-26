//! What a studio page is called, where it is photographed, and what it records
//! about the draw pools it was drawn through.
use std::fmt::{self, Display};

use kithara::ui::{capture::Geometry, draw::PoolStats};

use crate::gui::{retained::window_size, ui::cache::DeckLayout};

/// The window the studio opens at, which is what both hosts are photographed
/// at so the two sets can be compared at all.
pub(super) fn studio() -> Geometry {
    let (width, height) = window_size();
    Geometry {
        height,
        width,
        scale: 1.0,
    }
}

pub(super) struct PoolSample {
    pub(super) first: PoolStats,
    pub(super) second: PoolStats,
}

impl PoolSample {
    fn line(&self, page: impl Display) -> String {
        format!(
            "{page} first_misses={} second_misses={} first_home_hits={} second_home_hits={} \
             first_drops={} second_drops={}\n",
            self.first.alloc_misses,
            self.second.alloc_misses,
            self.first.home_hits,
            self.second.home_hits,
            self.first.put_drops,
            self.second.put_drops,
        )
    }

    fn stable(&self) -> bool {
        self.first.alloc_misses > 0
            && self.first.alloc_misses == self.second.alloc_misses
            && self.first.put_drops == self.second.put_drops
    }
}

/// One page of the studio capture: a deck layout, named the way its file is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Page(pub(super) DeckLayout);

impl Display for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            DeckLayout::Single => "studio-single",
            DeckLayout::Dual => "studio-dual",
        })
    }
}

/// What every page of a set records about the draw pools it drew through, one
/// line per page, written beside the set.
pub(super) fn pooled(pools: &mut String, page: Page, sample: &PoolSample) -> Result<(), String> {
    pools.push_str(&sample.line(page));
    if sample.stable() {
        return Ok(());
    }
    Err(format!(
        "draw pools allocated again on the second {page} frame: {}",
        sample.line(page).trim(),
    ))
}
