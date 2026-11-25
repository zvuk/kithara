#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    effects::{DeckEffect, FxParamChange, FxParamDescriptor, FxParamMapping, FxParamUnit},
    play::{
        CurveShape, DeckFxChange, DeckParameter, DeckProgram, FxMix, FxTarget, LayoutRevision,
        ParameterKnot, PlayError,
    },
    queue::QueueError,
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_command::When;
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;
use num_traits::ToPrimitive;

use crate::{deck_fx::Curve, mix_tap::load_source};

mod cases;
mod fixture;
mod nonblocking;
mod regressions;
