pub(crate) use ::tokio as backend;
pub use ::tokio::{join, main, pin, select, task_local, try_join};
#[cfg(feature = "signal")]
pub use backend::signal;

#[cfg(feature = "tokio-net")]
pub mod io {
    pub use super::backend::io::{AsyncReadExt, AsyncWriteExt};
}

#[cfg(feature = "tokio-net")]
pub mod net {
    pub use super::backend::net::{TcpListener, TcpStream};
}

pub mod runtime {
    pub use super::backend::runtime::*;
}

pub(crate) mod sync {
    pub(crate) use super::backend::sync::broadcast;

    pub(crate) mod mpsc {
        pub use super::super::backend::sync::mpsc::error;
    }
}

pub(crate) mod task {
    pub use super::backend::task::{JoinError, JoinHandle};

    pub(crate) fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        super::backend::task::spawn_blocking(f)
    }
}

pub(crate) mod thread_pool {
    #[inline]
    pub async fn ensure_thread_pool() {}
}
