mod cell;
#[cfg(not(target_arch = "wasm32"))]
mod file;
mod flush;
pub mod schema;
#[cfg(target_arch = "wasm32")]
mod stub;
#[cfg(not(target_arch = "wasm32"))]
mod worker;
pub(crate) use cell::{IndexCell, IndexState};
pub(crate) use file::IndexTarget;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use file::{IndexFile, read_archived, write_record};
pub use flush::{FlushHub, FlushPolicy, FlushPolicyPatch};
pub(crate) use flush::{Flushable, flush_sync};
#[cfg(target_arch = "wasm32")]
use stub::{self as file, self as worker};
