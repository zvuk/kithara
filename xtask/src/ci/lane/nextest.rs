use std::path::PathBuf;

use anyhow::Result;

pub(super) use super::tools::prepend;
use super::tools::{self, Tool};
use crate::ci::{config::CiPins, process::Process};

pub(super) fn prepare(process: &Process, pins: &CiPins, cargo: &str) -> Result<Option<PathBuf>> {
    tools::prepare(process, pins, cargo, Tool::Nextest)
}
