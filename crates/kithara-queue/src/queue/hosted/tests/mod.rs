#[cfg(not(target_arch = "wasm32"))]
mod admission_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod background_tests;
#[cfg(not(target_arch = "wasm32"))]
mod engine_event_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod entry_tests;
#[cfg(not(target_arch = "wasm32"))]
mod legacy_processor;
#[cfg(not(target_arch = "wasm32"))]
mod playback_tests;
#[cfg(not(target_arch = "wasm32"))]
mod player_internal;
#[cfg(not(target_arch = "wasm32"))]
mod selection_tests;
mod terminal_tests;

use std::num::NonZeroU32;

use kithara_command::Seq;
use kithara_play::{
    DeckMixerConfig, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, Player, TrackReceipt,
    TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

#[cfg(not(target_arch = "wasm32"))]
use self::entry_tests::*;
use super::super::{Queue, QueueCommand, slots::Role};
use crate::{ActionAtItemEnd, QueueError, QueueEvent, TrackStatus};
