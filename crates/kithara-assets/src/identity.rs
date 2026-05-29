//! Request identity: stable canonical headers for inflight cache keying.
//!
//! `RequestIdentity` differentiates inflight resource handles within one
//! `AssetStore` by the headers that produced the request. Names are
//! lowercased; duplicate names keep the last value (HTTP multi-value is
//! out of scope, see crate `README.md`). `Debug` prints only a stable
//! hash of the headers - never names or values -- so auth tokens and
//! cookies cannot leak through logs.

use std::{
    collections::BTreeMap,
    fmt,
    hash::{DefaultHasher, Hash, Hasher},
};

/// Stable, canonicalised set of request headers.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RequestIdentity {
    headers: BTreeMap<String, Vec<u8>>,
}

impl RequestIdentity {
    /// Empty identity (no headers).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            headers: BTreeMap::new(),
        }
    }

    /// Build from an iterator of `(name, value)` pairs. Names are
    /// lowercased; duplicates keep the last value.
    pub fn from_headers<I, N, V>(it: I) -> Self
    where
        I: IntoIterator<Item = (N, V)>,
        N: AsRef<str>,
        V: AsRef<[u8]>,
    {
        let mut headers = BTreeMap::new();
        for (n, v) in it {
            headers.insert(n.as_ref().to_ascii_lowercase(), v.as_ref().to_vec());
        }
        Self { headers }
    }

    /// Whether this identity carries no headers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }
}

impl Default for RequestIdentity {
    fn default() -> Self {
        Self::empty()
    }
}

impl Hash for RequestIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for (k, v) in &self.headers {
            k.hash(state);
            v.hash(state);
        }
    }
}

impl fmt::Debug for RequestIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.headers.is_empty() {
            return f.write_str("RequestIdentity(empty)");
        }
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        write!(f, "RequestIdentity({:016x})", hasher.finish())
    }
}
