use std::{borrow::Borrow, collections::HashMap, hash::Hash};

use thunderdome::{Arena, Index};

pub(crate) struct ArenaRegistry<K, V> {
    by_index: HashMap<Index, K>,
    by_key: HashMap<K, Index>,
    values: Arena<V>,
}

impl<K, V> ArenaRegistry<K, V>
where
    K: Clone + Eq + Hash,
{
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            by_index: HashMap::with_capacity(cap),
            by_key: HashMap::with_capacity(cap),
            values: Arena::with_capacity(cap),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.by_index.clear();
        self.by_key.clear();
        self.values.clear();
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let idx = *self.by_key.get(key)?;
        self.values.get(idx)
    }

    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let idx = *self.by_key.get(key)?;
        self.values.get_mut(idx)
    }

    pub(crate) fn get_by_index(&self, idx: Index) -> Option<&V> {
        self.values.get(idx)
    }

    pub(crate) fn insert(&mut self, key: K, value: V) {
        if let Some(old_idx) = self.by_key.remove(&key) {
            let _ = self.values.remove(old_idx);
            self.by_index.remove(&old_idx);
        }

        let idx = self.values.insert(value);
        self.by_index.insert(idx, key.clone());
        self.by_key.insert(key, idx);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (Index, &V)> {
        self.values.iter()
    }

    pub(crate) fn iter_keys(&self) -> impl Iterator<Item = (&K, &Index)> {
        self.by_key.iter()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (Index, &mut V)> {
        self.values.iter_mut()
    }

    pub(crate) fn len(&self) -> usize {
        self.by_key.len()
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let idx = self.by_key.remove(key)?;
        self.by_index.remove(&idx);
        self.values.remove(idx)
    }

    pub(crate) fn remove_by_index(&mut self, idx: Index) -> Option<V> {
        let key = self.by_index.remove(&idx)?;
        self.by_key.remove(&key);
        self.values.remove(idx)
    }
}
