#![forbid(unsafe_code)]

use std::sync::Arc;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::OnceLock;

use crate::{DemandSlot, Timeline};

pub trait TransferCoordination<Demand>: Send + Sync + 'static
where
    Demand: Clone + Send + Sync + 'static,
{
    fn timeline(&self) -> Timeline;

    fn demand(&self) -> &DemandSlot<Demand>;
}

impl<Demand, T> TransferCoordination<Demand> for Arc<T>
where
    Demand: Clone + Send + Sync + 'static,
    T: TransferCoordination<Demand> + ?Sized,
{
    fn timeline(&self) -> Timeline {
        (**self).timeline()
    }

    fn demand(&self) -> &DemandSlot<Demand> {
        (**self).demand()
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn unit_demand_slot() -> &'static DemandSlot<()> {
    static SLOT: OnceLock<DemandSlot<()>> = OnceLock::new();
    SLOT.get_or_init(DemandSlot::new)
}

#[cfg(any(test, feature = "test-utils"))]
impl TransferCoordination<()> for () {
    fn timeline(&self) -> Timeline {
        Timeline::new()
    }

    fn demand(&self) -> &DemandSlot<()> {
        unit_demand_slot()
    }
}
