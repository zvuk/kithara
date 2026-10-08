//! The library tree of sources beside the selected source's page.
#[cfg(not(target_arch = "wasm32"))]
mod explorer;
#[cfg(not(target_arch = "wasm32"))]
mod folders;
#[cfg(not(target_arch = "wasm32"))]
mod listing;
mod pages;
mod shell;
mod sources;
mod startup;
#[cfg(test)]
mod tests;
mod track;

#[cfg(not(target_arch = "wasm32"))]
pub(in crate::gui) use self::{explorer::Explorer, folders::FolderPicker};
pub(in crate::gui) use self::{
    pages::{SourceAdditions, listed},
    shell::Library,
    sources::{FACTORIES, configured},
    startup::StartupSource,
};
