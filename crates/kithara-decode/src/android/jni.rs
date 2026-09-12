#![allow(unsafe_code)]

use jni::{JavaVM, errors::Error};

use super::error::AndroidBackendError;

pub(crate) fn ensure_current_thread_attached() -> Result<(), AndroidBackendError> {
    let context = std::panic::catch_unwind(ndk_context::android_context).map_err(|_| {
        AndroidBackendError::operation(
            "jni-attach-current-thread",
            "android context was not initialized",
        )
    })?;

    // SAFETY: `context.vm()` is the process JavaVM, valid for the process lifetime.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) };
    vm.attach_current_thread(|_env| Ok::<(), Error>(()))
        .map_err(|error| {
            AndroidBackendError::operation("jni-attach-current-thread", error.to_string())
        })
}
