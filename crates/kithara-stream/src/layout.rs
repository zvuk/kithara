#![forbid(unsafe_code)]

use std::{hash::Hash, ops::Range, sync::Arc};

use kithara_platform::Mutex;

pub trait LayoutIndex: Send + Sync + 'static {
    type Item: Copy + Eq + Hash + Send + Sync + 'static;

    fn item_at_offset(&self, offset: u64) -> Option<Self::Item>;

    fn item_range(&self, item: Self::Item) -> Option<Range<u64>>;
}

impl LayoutIndex for () {
    type Item = ();

    fn item_at_offset(&self, _offset: u64) -> Option<Self::Item> {
        Some(())
    }

    fn item_range(&self, (): Self::Item) -> Option<Range<u64>> {
        None
    }
}

impl<T> LayoutIndex for Mutex<T>
where
    T: LayoutIndex,
{
    type Item = T::Item;

    fn item_at_offset(&self, offset: u64) -> Option<Self::Item> {
        self.lock_sync().item_at_offset(offset)
    }

    fn item_range(&self, item: Self::Item) -> Option<Range<u64>> {
        self.lock_sync().item_range(item)
    }
}

impl<T> LayoutIndex for Arc<T>
where
    T: LayoutIndex + ?Sized,
{
    type Item = T::Item;

    fn item_at_offset(&self, offset: u64) -> Option<Self::Item> {
        (**self).item_at_offset(offset)
    }

    fn item_range(&self, item: Self::Item) -> Option<Range<u64>> {
        (**self).item_range(item)
    }
}
