#![forbid(unsafe_code)]

use std::path::PathBuf;

use kithara_bufpool::ByteBuffer;
use kithara_platform::sync::{Arc, Mutex};
use rkyv::rancor::Error;

use super::{core::LruIndex, state::LruState};
use crate::{
    error::AssetsResult,
    index::{
        persistence::{IndexCell, IndexFile, read_archived, schema::LruIndexFile, write_record},
        schema::ArchivedLruIndexFile,
    },
};

impl LruIndex {
    /// Construct a disk-backed index rooted at `path`.
    ///
    /// An existing file is read and hydrated synchronously. Otherwise the
    /// disk file is **not** materialised — it appears on the first
    /// [`LruIndex::touch`] or [`LruIndex::remove`].
    pub(crate) fn with_persist_at(path: PathBuf, mut buffer: ByteBuffer) -> Self {
        let file = IndexFile::new(path);
        let initial = read_state(&file, &mut buffer).unwrap_or_else(|e| {
            tracing::debug!("read existing lru.bin failed: {e}");
            LruState::default()
        });
        let inner = IndexCell::new(Mutex::new(initial));
        inner.persist_to(file, write_state);
        Self {
            inner: Arc::new(inner),
        }
    }
}

fn read_state(file: &IndexFile, buf: &mut ByteBuffer) -> AssetsResult<LruState> {
    let Some(archived) = read_archived::<ArchivedLruIndexFile>(file, buf)? else {
        return Ok(LruState::default());
    };
    let index = rkyv::deserialize::<LruIndexFile, Error>(archived)
        .expect("BUG: LRU archived → owned deserialize");
    Ok(LruState::from(index))
}

fn write_state(state: &Mutex<LruState>, file: &IndexFile, durable: bool) -> AssetsResult<()> {
    let state = state.lock().clone();
    write_record(file, &LruIndexFile::from(&state), durable)
}
