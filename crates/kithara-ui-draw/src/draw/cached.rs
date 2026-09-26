/// A value kept beside the key it was built from.
///
/// The guard belongs at the call site: the point of this cache is that nothing
/// is prepared for a hit, so it holds no builder and offers no way to hand it
/// one. A caller asks whether the key still holds, and only then builds.
#[derive(fieldwork::Fieldwork)]
#[derive_where::derive_where(Default)]
#[fieldwork(opt_in, get)]
pub struct CachedValue<K: PartialEq + Default, V> {
    #[field(get, vis = "pub")]
    key: K,
    value: Option<V>,
}

impl<K: PartialEq + Default, V> CachedValue<K, V> {
    /// Takes the new pair when the old one no longer answers. A key that still
    /// holds keeps the value it was paired with, so a caller that re-derived
    /// the same key cannot replace a good value with a stale one.
    pub fn update(&mut self, key: K, value: Option<V>) {
        if self.value.is_none() || self.key != key {
            self.key = key;
            self.value = value;
        }
    }

    pub fn value(&self) -> Option<&V> {
        self.value.as_ref()
    }
}
