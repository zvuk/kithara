use derive_more::Display;
use serde::{Deserialize, Serialize};

use crate::error::UiDocError;

#[derive(Clone, Debug, Display, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocId(pub String);

#[derive(Clone, Debug, Display, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

#[derive(Clone, Debug, Display, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(pub String);

#[derive(Clone, Debug, Display, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EndpointId(pub String);

#[derive(Clone, Debug, Display, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceUri(pub String);

/// Handle to a string interned in a [`StrArena`]. Valid only within the
/// `CompiledUi` that produced it -- a recompile rebuilds the arena.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InternId(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Span {
    start: u32,
    len: u32,
}

/// Interned-string storage owned by a compiled UI. Plain `String` buffer and
/// span table, bounded by `UiConfig.max_arena_bytes`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StrArena {
    bytes: String,
    spans: Vec<Span>,
}

impl StrArena {
    /// Resolves an interned string. Unknown IDs and invalid spans resolve to `""`.
    #[must_use]
    pub fn resolve(&self, id: InternId) -> &str {
        let Some(span) = usize::try_from(id.0)
            .ok()
            .and_then(|index| self.spans.get(index))
        else {
            return "";
        };
        let Some((start, end)) = usize::try_from(span.start)
            .ok()
            .zip(usize::try_from(span.len).ok())
            .and_then(|(start, len)| start.checked_add(len).map(|end| (start, end)))
        else {
            return "";
        };
        self.bytes.get(start..end).unwrap_or("")
    }
}

pub(crate) struct Interner {
    arena: StrArena,
    max_bytes: usize,
}

impl Interner {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            arena: StrArena::default(),
            max_bytes,
        }
    }

    pub(crate) fn intern(
        &mut self,
        value: &str,
        origin: &SourceUri,
    ) -> Result<InternId, UiDocError> {
        for index in 0..self.arena.spans.len() {
            let raw = u32::try_from(index).map_err(|_| UiDocError::ArenaFull {
                origin: origin.clone(),
                max: self.max_bytes,
            })?;
            let id = InternId(raw);
            if self.resolve(id) == value {
                return Ok(id);
            }
        }

        let end = self
            .arena
            .bytes
            .len()
            .checked_add(value.len())
            .ok_or_else(|| UiDocError::ArenaFull {
                origin: origin.clone(),
                max: self.max_bytes,
            })?;
        if end > self.max_bytes {
            return Err(UiDocError::ArenaFull {
                origin: origin.clone(),
                max: self.max_bytes,
            });
        }

        let start = u32::try_from(self.arena.bytes.len()).map_err(|_| UiDocError::ArenaFull {
            origin: origin.clone(),
            max: self.max_bytes,
        })?;
        let len = u32::try_from(value.len()).map_err(|_| UiDocError::ArenaFull {
            origin: origin.clone(),
            max: self.max_bytes,
        })?;
        let id = u32::try_from(self.arena.spans.len())
            .map(InternId)
            .map_err(|_| UiDocError::ArenaFull {
                origin: origin.clone(),
                max: self.max_bytes,
            })?;
        self.arena
            .bytes
            .try_reserve(value.len())
            .map_err(|_| UiDocError::ArenaFull {
                origin: origin.clone(),
                max: self.max_bytes,
            })?;
        self.arena.bytes.push_str(value);
        self.arena.spans.push(Span { start, len });
        Ok(id)
    }

    pub(crate) fn resolve(&self, id: InternId) -> &str {
        self.arena.resolve(id)
    }

    pub(crate) fn finish(self) -> StrArena {
        self.arena
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn ids_display_inner_string() {
        assert_eq!(NodeId("play".into()).to_string(), "play");
        assert_eq!(
            SourceUri("modules/deck.kmodule.ron".into()).to_string(),
            "modules/deck.kmodule.ron"
        );
    }

    #[kithara::test]
    fn interner_deduplicates_and_roundtrips_strings() {
        let origin = SourceUri("test.ron".to_owned());
        let mut interner = Interner::new(1024);

        let first = interner.intern("deck-a", &origin).unwrap();
        let duplicate = interner.intern("deck-a", &origin).unwrap();
        let other = interner.intern("deck-b", &origin).unwrap();
        let multibyte = interner
            .intern("\u{434}\u{435}\u{43a}\u{430}-\u{3b1}", &origin)
            .unwrap();

        assert_eq!(first, duplicate);
        assert_ne!(first, other);
        assert_eq!(interner.resolve(first), "deck-a");
        assert_eq!(
            interner.resolve(multibyte),
            "\u{434}\u{435}\u{43a}\u{430}-\u{3b1}"
        );
    }

    #[kithara::test]
    fn interner_enforces_the_byte_cap() {
        let origin = SourceUri("test.ron".to_owned());
        let mut interner = Interner::new(3);

        let error = interner.intern("four", &origin).unwrap_err();

        assert!(matches!(error, UiDocError::ArenaFull { max: 3, .. }));
    }

    #[kithara::test]
    fn arena_resolves_unknown_id_to_empty_string() {
        let origin = SourceUri("test.ron".to_owned());
        let mut first = Interner::new(1024);
        first.intern("first", &origin).unwrap();
        let arena = first.finish();
        let mut second = Interner::new(1024);
        second.intern("other", &origin).unwrap();
        let foreign = second.intern("foreign", &origin).unwrap();

        assert_eq!(arena.resolve(foreign), "");
    }
}
