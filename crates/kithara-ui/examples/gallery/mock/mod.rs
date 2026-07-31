mod consts;
mod endpoints;
mod menu;
mod quality;
mod reads;

pub(crate) use endpoints::{MockRegistry, registry};
pub(crate) use reads::MockReads;
