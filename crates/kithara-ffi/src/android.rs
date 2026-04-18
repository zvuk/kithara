use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use jni::{
    Env, JNIEnv,
    objects::{Global, JClass, JObject},
    strings::JNIString,
    sys::jint,
};
use rustls_platform_verifier::android as rustls_android;
use tracing::error;
use tracing_subscriber::{filter::LevelFilter, prelude::*};

mod android_context {
    use super::{AtomicBool, Global, JObject, OnceLock};

    pub(super) static READY: AtomicBool = AtomicBool::new(false);
    pub(super) static GLOBAL: OnceLock<Global<JObject<'static>>> = OnceLock::new();
}

#[expect(unreachable_pub, reason = "JNI entrypoint must remain exported")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kithara_Kithara_nativeInit(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    context: JObject<'_>,
    log_level: jint,
) {
    if !android_context::READY.load(Ordering::Acquire) {
        let filter = level_filter(log_level);
        if let Ok(layer) = tracing_android::layer("kithara") {
            let _ = tracing_subscriber::registry()
                .with(layer.with_filter(filter))
                .try_init();
        }

        let mut init_ok = false;
        let _ = env.with_env_no_catch(|env| -> Result<(), jni::errors::Error> {
            match init_android_context(env, &context) {
                Ok(()) => {
                    android_context::READY.store(true, Ordering::Release);
                    init_ok = true;
                    Ok(())
                }
                Err(message) => {
                    error!(message = %message);
                    env.throw_new(
                        JNIString::from("java/lang/IllegalStateException"),
                        JNIString::from(message.as_str()),
                    )
                }
            }
        });
        if !init_ok {
            return;
        }
    }

    let _ = env.with_env_no_catch(|env| -> Result<(), jni::errors::Error> {
        if let Err(err) = rustls_android::init_with_env(env, context) {
            let message = format!("failed to initialize rustls platform verifier: {err}");
            error!(message = %message);
            env.throw_new(
                JNIString::from("java/lang/IllegalStateException"),
                JNIString::from(message.as_str()),
            )?;
        }
        Ok(())
    });
}

fn level_filter(ordinal: jint) -> LevelFilter {
    const LOG_LEVEL_INFO: jint = 2;
    const LOG_LEVEL_WARN: jint = 3;
    const LOG_LEVEL_ERROR: jint = 4;

    match ordinal {
        0 => LevelFilter::TRACE,
        1 => LevelFilter::DEBUG,
        LOG_LEVEL_INFO => LevelFilter::INFO,
        LOG_LEVEL_WARN => LevelFilter::WARN,
        LOG_LEVEL_ERROR => LevelFilter::ERROR,
        _ => LevelFilter::OFF,
    }
}

fn init_android_context(env: &mut Env<'_>, context: &JObject<'_>) -> Result<(), String> {
    let java_vm = env
        .get_java_vm()
        .map_err(|err| format!("failed to get JavaVM: {err}"))?;
    // Keep Context as a process-wide global JNI ref.
    // A local JNI ref would become invalid after this JNI call returns.
    let context_global = env
        .new_global_ref(context)
        .map_err(|err| format!("failed to create global context ref: {err}"))?;

    let _ = android_context::GLOBAL.set(context_global);
    let Some(global) = android_context::GLOBAL.get() else {
        return Err("failed to store android context global ref".into());
    };

    // SAFETY:
    // - `java_vm` pointer comes from the current valid JNI env.
    // - `global` is a global JNI ref stored for process lifetime.
    // - guarded by `ANDROID_CONTEXT_READY`, so we initialize only once.
    // This is required for Android audio backend startup (cpal/AAudio):
    // in our embedding (Kotlin host + JNI library), ndk-glue is not used,
    // so ndk_context is not auto-initialized by runtime.
    unsafe {
        ndk_context::initialize_android_context(
            java_vm.get_raw().cast(),
            global.as_obj().as_raw().cast(),
        );
    }
    Ok(())
}
