use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use jni::{
    JNIEnv,
    objects::{GlobalRef, JClass, JObject},
    sys::jint,
};
use rustls_platform_verifier::android as rustls_android;
use tracing::error;
use tracing_subscriber::{filter::LevelFilter, prelude::*};

static ANDROID_CONTEXT_READY: AtomicBool = AtomicBool::new(false);
static ANDROID_CONTEXT_GLOBAL: OnceLock<GlobalRef> = OnceLock::new();

#[expect(unreachable_pub, reason = "JNI entrypoint must remain exported")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kithara_Kithara_nativeInit(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    context: JObject<'_>,
    log_level: jint,
) {
    if !ANDROID_CONTEXT_READY.load(Ordering::Acquire) {
        let filter = level_filter(log_level);
        if let Ok(layer) = tracing_android::layer("kithara") {
            let _ = tracing_subscriber::registry()
                .with(layer.with_filter(filter))
                .try_init();
        }
        match init_android_context(&mut env, &context) {
            Ok(()) => {
                ANDROID_CONTEXT_READY.store(true, Ordering::Release);
            }
            Err(message) => {
                error!(message = %message);
                let _ = env.throw_new("java/lang/IllegalStateException", &message);
                return;
            }
        }
    }

    if let Err(err) = rustls_android::init_with_env(&mut env, context) {
        let message = format!("failed to initialize rustls platform verifier: {err}");
        error!(message = %message);
        let _ = env.throw_new("java/lang/IllegalStateException", &message);
    }
}

const LOG_LEVEL_INFO: jint = 2;
const LOG_LEVEL_WARN: jint = 3;
const LOG_LEVEL_ERROR: jint = 4;

fn level_filter(ordinal: jint) -> LevelFilter {
    match ordinal {
        0 => LevelFilter::TRACE,
        1 => LevelFilter::DEBUG,
        LOG_LEVEL_INFO => LevelFilter::INFO,
        LOG_LEVEL_WARN => LevelFilter::WARN,
        LOG_LEVEL_ERROR => LevelFilter::ERROR,
        _ => LevelFilter::OFF,
    }
}

fn init_android_context(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<(), String> {
    let java_vm = env
        .get_java_vm()
        .map_err(|err| format!("failed to get JavaVM: {err}"))?;
    // Keep Context as a process-wide global JNI ref.
    // A local JNI ref would become invalid after this JNI call returns.
    let context_global = env
        .new_global_ref(context)
        .map_err(|err| format!("failed to create global context ref: {err}"))?;

    let _ = ANDROID_CONTEXT_GLOBAL.set(context_global);
    let Some(global) = ANDROID_CONTEXT_GLOBAL.get() else {
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
            java_vm.get_java_vm_pointer().cast(),
            global.as_obj().as_raw().cast(),
        );
    }
    Ok(())
}
