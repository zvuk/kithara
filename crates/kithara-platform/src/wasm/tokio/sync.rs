//! Enumerated async-channel surface (wasm). Identical set to native — the wasm
//! tree re-imports the real `tokio` primitives (pure-async, allocation-only, so
//! they compile to wasm). The glob is dead by design (design §2). `Notify` lives
//! in the `sync` facade.
pub use tokio_with_wasm::alias::sync::{OnceCell, Semaphore, broadcast, mpsc, oneshot, watch};
