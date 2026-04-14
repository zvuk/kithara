#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::Timeline;

pub trait TransferCoordination: Send + Sync + 'static {
    fn timeline(&self) -> Timeline;
}

impl<T> TransferCoordination for Arc<T>
where
    T: TransferCoordination + ?Sized,
{
    fn timeline(&self) -> Timeline {
        (**self).timeline()
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl TransferCoordination for () {
    fn timeline(&self) -> Timeline {
        Timeline::new()
    }
}
