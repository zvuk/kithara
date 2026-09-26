use std::ops::Deref;

use kithara_bufpool::PooledString;

const SHARDS: usize = 1;

type TextGuard = PooledString<SHARDS>;

/// UTF-8 text whose allocation can return to its owning draw-pool family.
#[derive(derive_more::Debug)]
#[debug("{:?}", self.as_str())]
pub struct PoolText {
    storage: TextStorage,
}

enum TextStorage {
    Owned(String),
    Pooled(TextGuard),
}

impl PoolText {
    /// Returns the retained UTF-8 text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.storage {
            TextStorage::Owned(content) => content,
            TextStorage::Pooled(guard) => guard,
        }
    }

    pub(in crate::draw) fn pooled(content: &str, mut guard: TextGuard) -> Self {
        if let Err(error) = guard.try_push_str(content) {
            panic!("draw text growth failed: {error}");
        }
        Self {
            storage: TextStorage::Pooled(guard),
        }
    }
}

impl From<String> for PoolText {
    fn from(content: String) -> Self {
        Self {
            storage: TextStorage::Owned(content),
        }
    }
}

impl From<&str> for PoolText {
    fn from(content: &str) -> Self {
        content.to_owned().into()
    }
}

impl Clone for PoolText {
    fn clone(&self) -> Self {
        self.as_str().to_owned().into()
    }
}

impl PartialEq for PoolText {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for PoolText {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for PoolText {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for PoolText {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&String> for PoolText {
    fn eq(&self, other: &&String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Deref for PoolText {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}
