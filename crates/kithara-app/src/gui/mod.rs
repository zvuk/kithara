mod app;
#[cfg(all(test, feature = "masonry"))]
mod capture;
mod deck;
#[cfg(not(target_arch = "wasm32"))]
mod desktop;
mod frontend;
mod message;
mod overlay;
mod reads;
#[cfg(feature = "masonry")]
pub(crate) mod retained;
#[cfg(test)]
pub(crate) mod rig;
mod subscription;
#[cfg(test)]
mod test_fixture;
mod theme;
mod ui;
mod update;
mod view;

#[cfg(not(target_arch = "wasm32"))]
pub use desktop::{Host, run};
pub use frontend::FrontendError;
#[cfg(target_arch = "wasm32")]
pub(crate) use frontend::{Boot, immediate};
