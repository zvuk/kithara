mod catalog;
pub(crate) mod command;
mod frontend;
mod legacy;
mod request;
mod resolve;
mod view;

use view::{prepare_view, require_policy_checkout};
use xtask::ci::lane::plan::wire;

#[cfg(test)]
mod tests;
