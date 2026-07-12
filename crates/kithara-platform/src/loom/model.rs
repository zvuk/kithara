pub(crate) fn model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    ::loom::model(f);
}
