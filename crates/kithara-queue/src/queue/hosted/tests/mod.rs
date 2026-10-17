#[cfg(not(target_arch = "wasm32"))]
mod admission_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod background_tests;
#[cfg(not(target_arch = "wasm32"))]
mod duration_contracts;
#[cfg(not(target_arch = "wasm32"))]
mod engine_event_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod entry_tests;
#[cfg(not(target_arch = "wasm32"))]
mod handover_restore;
#[cfg(not(target_arch = "wasm32"))]
mod legacy_processor;
#[cfg(not(target_arch = "wasm32"))]
mod lifecycle_restore;
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
#[cfg(not(target_arch = "wasm32"))]
use crate::mock::{
    answer_deck, answer_load, answer_loaded, empty_queue, loaded_fixture, pending_selection,
    queue_with_load_settings, tick_fixture, with_outbox,
};
use crate::{ActionAtItemEnd, QueueError, QueueEvent, TrackStatus};
