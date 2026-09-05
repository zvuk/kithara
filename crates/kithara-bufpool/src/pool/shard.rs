use crossbeam_queue::ArrayQueue;

use super::storage::Storage;

pub(super) struct PoolShard<B> {
    free: ArrayQueue<B>,
    max_retained_capacity: usize,
    trim_capacity: usize,
}

impl<B> PoolShard<B>
where
    B: Storage,
{
    pub(super) const MAX_SLOTS: usize = 1024;

    pub(super) fn new(
        max_buffers: usize,
        max_retained_capacity: usize,
        trim_capacity: usize,
    ) -> Self {
        Self {
            free: ArrayQueue::new(max_buffers.min(Self::MAX_SLOTS)),
            max_retained_capacity,
            trim_capacity,
        }
    }

    delegate::delegate! {
        to self.free {
            #[call(pop)]
            pub(super) fn try_get(&self) -> Option<B>;
            pub(super) fn len(&self) -> usize;
        }
    }

    pub(super) fn normalize(&self, value: &mut B) -> Option<usize> {
        const TRIM_HYSTERESIS: usize = 2;

        value.clear();
        if self.max_retained_capacity > 0 && value.capacity() > self.max_retained_capacity {
            return None;
        }
        if self.trim_capacity > 0
            && value.capacity() > self.trim_capacity.saturating_mul(TRIM_HYSTERESIS)
        {
            value.shrink_to(self.trim_capacity);
        }
        if value.capacity() == 0 {
            return None;
        }
        Some(B::bytes_for_capacity(value.capacity()).unwrap_or(usize::MAX))
    }

    pub(super) fn try_put(&self, mut value: B) -> Result<usize, B> {
        let Some(kept) = self.normalize(&mut value) else {
            return Err(value);
        };
        self.free.push(value).map(|()| kept)
    }

    pub(super) fn drain(&self, mut release: impl FnMut(B)) {
        while let Some(value) = self.free.pop() {
            release(value);
        }
    }
}
