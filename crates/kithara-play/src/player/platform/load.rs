use kithara_bufpool::PcmPool;

use super::super::state::playlist::PreparedBindingStamp;
use crate::api::Tempo;

#[derive(Clone, Copy)]
pub(crate) struct ItemLoadContext<'a> {
    pub(crate) pool: &'a PcmPool,
    pub(crate) tempo: Option<Tempo>,
    pub(crate) stamp: PreparedBindingStamp,
    pub(crate) pitch_bend: f32,
    pub(crate) rate: f32,
}

impl<'a> ItemLoadContext<'a> {
    pub(crate) const fn new(
        rate: f32,
        pitch_bend: f32,
        tempo: Option<Tempo>,
        pool: &'a PcmPool,
        stamp: PreparedBindingStamp,
    ) -> Self {
        Self {
            pool,
            tempo,
            stamp,
            pitch_bend,
            rate,
        }
    }
}
