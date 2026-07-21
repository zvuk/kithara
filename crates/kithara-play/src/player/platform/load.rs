use kithara_bufpool::PcmPool;

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

impl<'a> ItemLoadContext<'a> {
    pub(crate) const fn bound(
        rate: f32,
        pitch_bend: f32,
        pool: &'a PcmPool,
        context: PreparationContext,
    ) -> Self {
        Self {
            pool,
            mode: ItemLoadMode::Bound(context),
            pitch_bend,
            rate,
        }
    }

    pub(crate) const fn linear(
        rate: f32,
        pitch_bend: f32,
        pool: &'a PcmPool,
        shape: StreamShape,
    ) -> Self {
        Self {
            pool,
            mode: ItemLoadMode::Linear(shape),
            pitch_bend,
            rate,
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
