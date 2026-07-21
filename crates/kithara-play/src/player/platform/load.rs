use kithara_bufpool::PcmPool;
use kithara_platform::traits::FromWithParams;

use crate::{player::node::StreamShape, session::protocol::PreparationContext};

#[derive(Clone, Copy)]
enum ItemLoadMode {
    Bound(PreparationContext),
    Linear(StreamShape),
}

#[derive(Clone, Copy)]
pub(crate) struct ItemLoadContext<'a> {
    pool: &'a PcmPool,
    mode: ItemLoadMode,
    pitch_bend: f32,
    rate: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct ItemLoadParams<'a> {
    pub(crate) pool: &'a PcmPool,
    pub(crate) pitch_bend: f32,
    pub(crate) rate: f32,
}

impl<'a> ItemLoadContext<'a> {
    const fn from_mode(mode: ItemLoadMode, params: ItemLoadParams<'a>) -> Self {
        Self {
            mode,
            pitch_bend: params.pitch_bend,
            pool: params.pool,
            rate: params.rate,
        }
    }

    pub(crate) const fn pitch_bend(self) -> f32 {
        self.pitch_bend
    }

    pub(crate) const fn pool(self) -> &'a PcmPool {
        self.pool
    }

    pub(crate) const fn preparation(self) -> Option<PreparationContext> {
        match self.mode {
            ItemLoadMode::Bound(context) => Some(context),
            ItemLoadMode::Linear(_) => None,
        }
    }

    pub(crate) const fn rate(self) -> f32 {
        self.rate
    }

    pub(crate) const fn shape(self) -> StreamShape {
        match self.mode {
            ItemLoadMode::Bound(context) => context.shape(),
            ItemLoadMode::Linear(shape) => shape,
        }
    }
}

impl<'a> FromWithParams<PreparationContext, ItemLoadParams<'a>> for ItemLoadContext<'a> {
    fn build(context: PreparationContext, params: ItemLoadParams<'a>) -> Self {
        Self::from_mode(ItemLoadMode::Bound(context), params)
    }
}

impl<'a> FromWithParams<StreamShape, ItemLoadParams<'a>> for ItemLoadContext<'a> {
    fn build(shape: StreamShape, params: ItemLoadParams<'a>) -> Self {
        Self::from_mode(ItemLoadMode::Linear(shape), params)
    }
}
