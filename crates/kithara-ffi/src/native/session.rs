use std::sync::OnceLock;

use kithara::play::SessionHandle;

static SESSION: OnceLock<SessionHandle> = OnceLock::new();

pub(crate) fn handle() -> SessionHandle {
    SESSION.get_or_init(SessionHandle::spawn_native).clone()
}
