use kithara_bufpool::PooledVec;

const SHARDS: usize = 1;

pub(in crate::draw) type VecGuard<T> = PooledVec<T, SHARDS>;

#[derive(derive_more::Debug)]
#[debug(bound(T: std::fmt::Debug))]
pub(in crate::draw) enum Buffer<T> {
    #[debug("{:?}", _0.as_slice())]
    Owned(Vec<T>),
    #[debug("{:?}", &**_0)]
    Pooled(VecGuard<T>),
}

impl<T> Buffer<T> {
    pub(in crate::draw) fn as_slice(&self) -> &[T] {
        match self {
            Self::Owned(values) => values,
            Self::Pooled(guard) => guard,
        }
    }

    pub(in crate::draw) fn into_pooled(self, acquire: impl FnOnce() -> VecGuard<T>) -> Self {
        match self {
            pooled @ Self::Pooled(_) => pooled,
            Self::Owned(values) => {
                let mut pooled = Self::pooled(acquire());
                if let Self::Pooled(guard) = &mut pooled
                    && let Err(error) = guard.try_extend(values)
                {
                    panic!("draw buffer growth failed: {error}");
                }
                pooled
            }
        }
    }

    pub(in crate::draw) const fn owned(values: Vec<T>) -> Self {
        Self::Owned(values)
    }

    pub(in crate::draw) const fn pooled(guard: VecGuard<T>) -> Self {
        Self::Pooled(guard)
    }

    pub(in crate::draw) fn push(&mut self, value: T) {
        match self {
            Self::Owned(values) => values.push(value),
            Self::Pooled(guard) => {
                if let Err(error) = guard.try_push(value) {
                    panic!("draw buffer growth failed: {error}");
                }
            }
        }
    }
}

impl<T> Default for Buffer<T> {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}

impl<T: Clone> Clone for Buffer<T> {
    fn clone(&self) -> Self {
        Self::Owned(self.as_slice().to_vec())
    }
}

impl<T: PartialEq> PartialEq for Buffer<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
