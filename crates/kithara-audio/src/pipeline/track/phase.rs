use std::marker::PhantomData;

use super::CurrentFsm;

pub(super) mod sealed {
    pub(crate) trait Sealed {}
}

/// Phase marker for the worker-thread track FSM.
///
/// Each phase owns exactly the data it needs via [`TrackPhase::Data`].
/// Per-phase behaviour lives with each phase marker, so an operation only valid
/// in one phase does not exist on the other `Track<_>` types.
pub(crate) trait TrackPhase: sealed::Sealed + Sized {
    type Data;

    fn erase(track: Track<Self>) -> CurrentFsm;
}

/// Phantom-typed handle for a single FSM phase. Owns the phase's data;
/// transitions consume `self` and produce the next phase. Stored,
/// type-erased, in [`CurrentFsm`].
#[derive(fieldwork::Fieldwork)]
pub(crate) struct Track<S: TrackPhase> {
    #[field(get(vis = "pub(crate)"))]
    data: S::Data,
    _phase: PhantomData<S>,
}

impl<S: TrackPhase> Track<S> {
    pub(crate) fn new(data: S::Data) -> Self {
        Self {
            data,
            _phase: PhantomData,
        }
    }

    pub(crate) fn data_mut(&mut self) -> &mut S::Data {
        &mut self.data
    }

    pub(crate) fn into_inner(self) -> S::Data {
        self.data
    }

    pub(crate) fn erase(self) -> CurrentFsm {
        S::erase(self)
    }
}
