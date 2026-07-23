use crate::session::SessionHandle;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn default_session_handle() -> SessionHandle {
    SessionHandle::spawn_native()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn default_session_handle() -> SessionHandle {
    crate::session::local_session()
}
