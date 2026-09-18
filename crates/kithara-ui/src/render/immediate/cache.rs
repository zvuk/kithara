//! The cache group every canvas in this toolkit draws into.
//!
//! A geometry cache carries a group, and the renderer keeps one text atlas per
//! group. A cache built with `Cache::new` gets a group of its own, so a window
//! with a dozen canvases would stand up a dozen atlases, each growing to its
//! own full size and holding it for the life of the process — the same glyphs,
//! paid for once per canvas.
//!
//! One shared group is therefore not a micro-optimisation but the difference
//! between one atlas and one per canvas. Nothing here decides *when* a picture
//! is rebuilt: that stays with each canvas, which knows what its own picture
//! is made of.

use std::sync::OnceLock;

use iced::widget::canvas::{Cache, Group};

/// The group shared by every canvas cache this toolkit builds.
fn group() -> Group {
    static GROUP: OnceLock<Group> = OnceLock::new();
    *GROUP.get_or_init(Group::unique)
}

/// An empty canvas cache in the shared group.
pub(crate) fn canvas() -> Cache {
    Cache::with_group(group())
}
