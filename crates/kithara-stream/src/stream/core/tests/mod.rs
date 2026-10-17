mod core;
mod gate_moved_tests;

use std::ops::Range;

use kithara_platform::time::Duration;

use self::core::*;
use super::{contract::StreamType, *};
use crate::{
    DeferredWake, SourceSeekAnchor,
    activity::{Activity, ActivityWriter},
    error::{SourceError, StreamResult},
    playhead::PlayheadWrite,
    source::Source,
};
