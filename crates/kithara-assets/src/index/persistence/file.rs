#![forbid(unsafe_code)]

use std::{
    fs,
    fs::File,
    io::{Error, ErrorKind, Read, Write},
    path::PathBuf,
};

use kithara_bufpool::ByteBuffer;
use tempfile::NamedTempFile;

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
