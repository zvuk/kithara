#![allow(unsafe_code)]

use jni::JavaVM;

use super::error::AndroidBackendError;

pub(crate) fn ensure_current_thread_attached() -> Result<(), AndroidBackendError> {
    let context = std::panic::catch_unwind(ndk_context::android_context).map_err(|_| {
        AndroidBackendError::operation(
            "jni-attach-current-thread",
            "android context was not initialized",
        )
    })?;

    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) };
    vm.attach_current_thread(|_env| Ok::<(), jni::errors::Error>(()))
        .map_err(|error| {
            AndroidBackendError::operation("jni-attach-current-thread", error.to_string())
        })
}
