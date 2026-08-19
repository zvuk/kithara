pub(super) mod cache;
mod compile;
pub(crate) mod endpoints;
mod events;
pub(super) mod menu;
pub(super) mod modules;
pub(super) mod scope;
#[cfg(test)]
mod tests;
pub(super) mod window;

#[cfg(all(test, feature = "masonry"))]
pub(in crate::gui) use self::compile::compile_ui;
#[cfg(feature = "masonry")]
pub(crate) use self::compile::{entry, resolver, text};
pub(crate) use self::{
    compile::{AppUi, view},
    events::translate,
};
