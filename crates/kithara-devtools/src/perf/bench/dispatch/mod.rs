mod command;
mod macos;

pub(super) use command::{DispatchArgs, run};
use command::{encoded, gh, git, output};
