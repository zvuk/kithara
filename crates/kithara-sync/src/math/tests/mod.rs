mod core;
mod fixtures;
mod preparation;
mod relocation;

use kithara_signal::SessionFrame;
use kithara_warp::SessionBeat;
use num_traits::ToPrimitive;

use super::core::*;
use crate::{Bound, TempoTrajectory};
