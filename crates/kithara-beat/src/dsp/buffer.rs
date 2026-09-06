use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};

pub(super) fn collected<S, I>(
    pools: &PoolRegion<S>,
    len: usize,
    values: I,
) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
    I: IntoIterator<Item = f32>,
{
    let mut out = pools.get_with_len::<f32>(len)?;
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value;
    }
    Ok(out)
}
