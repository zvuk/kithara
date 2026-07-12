#[inline]
pub fn model<F>(f: F)
where
    F: FnOnce(),
{
    f();
}
