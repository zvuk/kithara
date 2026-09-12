use std::path::Path;

use anyhow::Result;
use kithara_devtools::viz::trace::{TraceRecord, TraceWriter};

pub fn write(path: &Path, records: impl IntoIterator<Item = TraceRecord>) -> Result<()> {
    let mut writer = TraceWriter::create(path)?;
    for record in records {
        writer.write(&record)?;
    }
    writer.finish()
}
