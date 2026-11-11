#![forbid(unsafe_code)]

use std::{collections::BTreeMap, path::PathBuf};

use dashmap::DashMap;
use kithara_bufpool::ByteBuffer;
use kithara_platform::sync::Arc;

use super::core::{PinCounts, PinsIndex};
use crate::{
    error::AssetsResult,
    index::{
        persistence::{IndexCell, IndexFile, read_archived, schema::PinsIndexFile, write_record},
        schema::ArchivedPinsIndexFile,
    },
};

impl PinsIndex {
    /// Construct a disk-backed index rooted at `path`.
    ///
    /// An existing file is read and hydrated synchronously. Otherwise the
    /// disk file is **not** materialised — it appears the first time
    /// [`PinsIndex::add`] or [`PinsIndex::remove`] flush a real change.
    #[must_use]
    pub fn with_persist_at(path: PathBuf, mut buffer: ByteBuffer) -> Self {
        let file = IndexFile::new(path);
        let initial = read_pins(&file, &mut buffer).unwrap_or_else(|e| {
            tracing::debug!("read existing pins.bin failed: {e}");
            DashMap::new()
        });
        let inner = IndexCell::new(initial);
        inner.persist_to(file, write_pins);
        Self {
            inner: Arc::new(inner),
        }
    }
}

/// Counts for a root read back from `pins.bin`: whoever set that pin is gone,
/// so the surviving claim is a single durable one.
const fn hydrated_counts() -> PinCounts {
    PinCounts {
        durable: 1,
        local: 0,
    }
}

fn read_pins(file: &IndexFile, buf: &mut ByteBuffer) -> AssetsResult<DashMap<String, PinCounts>> {
    let Some(archived) = read_archived::<ArchivedPinsIndexFile>(file, buf)? else {
        return Ok(DashMap::new());
    };

    let pinned = archived
        .pinned
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| (k.as_str().to_string(), hydrated_counts()))
        .collect();
    Ok(pinned)
}

/// Writes the roots held by at least one durable pin — the set that belongs on disk.
fn write_pins(
    pins: &DashMap<String, PinCounts>,
    file: &IndexFile,
    durable: bool,
) -> AssetsResult<()> {
    let pinned: BTreeMap<String, bool> = pins
        .iter()
        .filter(|r| r.value().durable > 0)
        .map(|r| (r.key().clone(), true))
        .collect();
    write_record(file, &PinsIndexFile { version: 1, pinned }, durable)
}
