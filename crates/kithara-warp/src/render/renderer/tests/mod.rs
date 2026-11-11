#[cfg(all(test, any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
mod latency_tests;
mod mapping_tests;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod native_curves;
#[cfg(feature = "stretch-signalsmith")]
mod native_tests;
mod quantum_tests;
mod residency_tests;

use std::num::NonZeroU32;

use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stretch::StretchKind;
use kithara_test_macros as kithara;

use super::*;
use crate::{WarpConfig, consts};
