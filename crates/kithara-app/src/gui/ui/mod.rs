mod artwork;
pub(super) mod cache;
mod compile;
pub(crate) mod endpoints;
mod events;
pub(super) mod modules;
pub(super) mod package;
#[cfg(all(test, feature = "masonry"))]
mod settings;
#[cfg(test)]
mod tests;
pub(super) mod window;

#[cfg(all(test, feature = "masonry"))]
pub(in crate::gui) use self::compile::compile_ui;
pub(crate) use self::{
    compile::{AppUi, settle, view},
    events::translate,
};
