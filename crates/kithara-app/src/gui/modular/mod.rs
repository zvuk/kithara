mod controls;
mod dispatch;
pub(crate) mod endpoints;
mod filter;
mod mini_wave;
mod msg;
mod reads;
mod settings;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use msg::{ControlAction, ModularMsg};
pub(crate) use settings::render as render_settings;
pub(crate) use state::ModularView;
pub(crate) use update::{initial_view, update};
