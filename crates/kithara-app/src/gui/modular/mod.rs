mod endpoints;
mod msg;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use msg::ModularMsg;
pub(crate) use state::{ModularView, ViewMode};
pub(crate) use update::update;
