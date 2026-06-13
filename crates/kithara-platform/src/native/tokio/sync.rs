//! Enumerated async-channel surface (native). The glob is dead by design — it is
//! how unvetted primitives leak in unnoticed (design §2): only the items the
//! workspace actually consumes are re-exported. These are the real `tokio`
//! primitives — native is the production path, where flash adds nothing.
//! `Notify` lives in the `sync` facade.
pub use tokio_with_wasm::alias::sync::{OnceCell, Semaphore, broadcast, mpsc, oneshot};
