use std::sync::OnceLock;

use jni::{
    Env, JavaVM,
    errors::Error,
    objects::{Global, JObject},
};

use crate::error::AndroidBackendError;

/// Keeps the published application context reference alive, and claims the
/// single write to the platform global.
static CONTEXT: OnceLock<Global<JObject<'static>>> = OnceLock::new();

/// Publish the host runtime handle and application context so every reader of
/// the platform global, Kithara's own and third-party alike, observes an
/// initialized runtime. A repeated call returns without writing again.
pub fn initialize(vm: &JavaVM, context: Global<JObject<'static>>) {
    if CONTEXT.set(context).is_err() {
        return;
    }
    let Some(context) = CONTEXT.get() else {
        return;
    };

    // SAFETY: the write happens once, `vm` is the process JavaVM, and CONTEXT
    // retains the application context for the rest of the process lifetime.
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_raw().cast(),
            context.as_obj().as_raw().cast(),
        );
    }
}

/// Attach the calling thread to the host runtime for the rest of its life.
///
/// # Errors
///
/// Returns [`AndroidBackendError::NotInitialized`] when nothing published the
/// runtime handle, and [`AndroidBackendError::Operation`] when the attach
/// itself fails.
pub fn attach_current_thread() -> Result<(), AndroidBackendError> {
    with_attached_env(|_env| Ok(()))
}

/// Run `call` with an [`Env`], leaving the calling thread attached to the host runtime.
pub(crate) fn with_attached_env<T, F>(call: F) -> Result<T, AndroidBackendError>
where
    F: FnOnce(&mut Env<'_>) -> Result<T, AndroidBackendError>,
{
    let context = std::panic::catch_unwind(ndk_context::android_context)
        .map_err(|_| AndroidBackendError::NotInitialized)?;

    // SAFETY: `context.vm()` is the process JavaVM, valid for the process lifetime.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) };
    vm.attach_current_thread(|env| Ok::<_, Error>(call(env)))
        .map_err(AndroidBackendError::jni("jni-attach-current-thread"))?
}
