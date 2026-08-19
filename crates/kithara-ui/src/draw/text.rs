use std::{fmt, ops::Deref};

use kithara_bufpool::{PooledOwned, Reuse, SharedPool};

const SHARDS: usize = 1;

#[derive(Debug, Default)]
pub(super) struct TextBuffer(pub(super) String);

impl Reuse for TextBuffer {
    fn byte_size(&self) -> usize {
        self.0.capacity()
    }

    fn reuse(&mut self, max_capacity: usize) -> bool {
        self.0.clear();
        self.0.capacity() > 0 && self.0.capacity() <= max_capacity
    }
}

pub(super) type TextPool = SharedPool<SHARDS, TextBuffer>;
type TextGuard = PooledOwned<SHARDS, TextBuffer>;

/// UTF-8 text whose allocation can return to its owning draw-pool family.
pub struct PoolText {
    storage: TextStorage,
}

enum TextStorage {
    Owned(String),
    Pooled(TextGuard),
}

impl PoolText {
    pub(super) fn pooled(content: &str, pool: &TextPool) -> Self {
        let mut guard = pool.get();
        guard.0.push_str(content);
        Self {
            storage: TextStorage::Pooled(guard),
        }
    }

    /// Returns the retained UTF-8 text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.storage {
            TextStorage::Owned(content) => content,
            TextStorage::Pooled(guard) => &guard.0,
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

impl fmt::Debug for PoolText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
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
