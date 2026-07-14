use kithara_decode::{PcmChunk, PcmSpec};

use crate::waveform::bucket::Waveform;

#[derive(Default)]
pub(crate) struct Config;

#[derive(Default)]
pub(crate) struct Slot;

pub(crate) fn build(_config: &Config, _spec: PcmSpec) -> Slot {
    Slot
}

pub(crate) fn config_is_empty(_config: &Config) -> bool {
    true
}

pub(crate) fn finish(_slot: Slot) -> Option<Waveform> {
    None
}

pub(crate) fn push(_slot: &mut Slot, _chunk: &PcmChunk) {}
