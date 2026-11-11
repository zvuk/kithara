#![forbid(unsafe_code)]

use std::{
    fs,
    fs::File,
    io::{Error, ErrorKind, Read, Write},
    path::PathBuf,
};

use kithara_bufpool::ByteBuffer;
use kithara_platform::sync::Mutex;
use kithara_storage::StorageError;
use rkyv::{
    api::high::{HighSerializer, HighValidator},
    bytecheck::CheckBytes,
    rancor::Error as RkyvError,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};
use tempfile::NamedTempFile;

use super::IndexCell;
use crate::error::{AssetsError, AssetsResult};

/// One on-disk index snapshot, read and replaced whole.
///
/// No handle outlives a call: a read copies the file into the caller's
/// buffer and a write publishes a sibling temp file by rename. Nothing keeps
/// the file open or mapped between flushes, so no platform can refuse the
/// rename that replaces it.
pub(crate) struct IndexFile {
    path: PathBuf,
}

impl IndexFile {
    pub(crate) const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Read the whole snapshot into `buf`, which ends up holding exactly the
    /// bytes read. A file that does not exist yet reads as empty.
    pub(crate) fn read_into(&self, buf: &mut ByteBuffer) -> AssetsResult<()> {
        buf.clear();
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                self.report_io_error("open", &error);
                return Err(error.into());
            }
        };
        let len = usize::try_from(
            file.metadata()
                .inspect_err(|error| self.report_io_error("metadata", error))?
                .len(),
        )
        .map_err(|error| AssetsError::Io(Error::other(error)))?;
        buf.ensure_len(len)?;
        let mut read = 0;
        while read < len {
            match file
                .read(&mut buf[read..len])
                .inspect_err(|error| self.report_io_error("read", error))?
            {
                0 => break,
                n => read += n,
            }
        }
        buf.truncate(read);
        Ok(())
    }

    /// Replace the snapshot with `bytes`. `durable` forces them onto the
    /// medium before the rename names them.
    pub(crate) fn write(&self, bytes: &[u8], durable: bool) -> AssetsResult<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::other("index file has no parent directory"))?;
        fs::create_dir_all(parent)
            .inspect_err(|error| self.report_io_error("create-directory", error))?;
        let mut tmp = NamedTempFile::new_in(parent)
            .inspect_err(|error| self.report_io_error("create-tempfile", error))?;
        tmp.write_all(bytes)
            .inspect_err(|error| self.report_io_error("write", error))?;
        if durable {
            tmp.as_file()
                .sync_data()
                .inspect_err(|error| self.report_io_error("sync-data", error))?;
        }
        tmp.persist(&self.path)
            .map_err(|error| error.error)
            .inspect_err(|error| self.report_io_error("persist", error))?;
        Ok(())
    }

    fn report_io_error(&self, operation: &'static str, error: &Error) {
        tracing::error!(operation, path = %self.path.display(), error = ?error, "asset index I/O failed");
    }
}

/// Snapshots an index's state and writes it to its file.
pub(crate) type WriteIndex<S> = fn(&S, &IndexFile, bool) -> AssetsResult<()>;

/// Where a persisted index lands: its file, how its state is written there,
/// and the lock that keeps one snapshot-and-rename in flight at a time.
pub(crate) struct IndexTarget<S> {
    file: IndexFile,
    write: WriteIndex<S>,
    writing: Mutex<()>,
}

impl<S> IndexTarget<S> {
    pub(crate) fn publish(&self, state: &S, durable: bool) -> AssetsResult<()> {
        let _writing = self.writing.lock();
        (self.write)(state, &self.file, durable)
    }
}

impl<S> IndexCell<S> {
    /// Points the index at its file; the first target stays.
    pub(crate) fn persist_to(&self, file: IndexFile, write: WriteIndex<S>) {
        let _ = self.target.set(IndexTarget {
            file,
            write,
            writing: Mutex::new(()),
        });
    }
}

/// Reads an index file and validates it as `A`. An empty file or one that
/// fails validation reads as `None`: the index starts empty and its next
/// flush replaces the file.
pub(crate) fn read_archived<'b, A>(
    file: &IndexFile,
    buf: &'b mut ByteBuffer,
) -> AssetsResult<Option<&'b A>>
where
    A: rkyv::Portable + for<'a> CheckBytes<HighValidator<'a, RkyvError>>,
{
    file.read_into(buf)?;
    if buf.is_empty() {
        return Ok(None);
    }
    match rkyv::access::<A, RkyvError>(buf) {
        Ok(archived) => Ok(Some(archived)),
        Err(error) => {
            tracing::debug!(%error, "index file failed validation; starting empty");
            Ok(None)
        }
    }
}

/// Serialises `record` and publishes it to `file`.
pub(crate) fn write_record<T>(file: &IndexFile, record: &T, durable: bool) -> AssetsResult<()>
where
    T: for<'a> rkyv::Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RkyvError>>,
{
    let bytes = rkyv::to_bytes::<RkyvError>(record)
        .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
    file.write(&bytes, durable)
}
