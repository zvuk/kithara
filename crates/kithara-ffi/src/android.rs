use std::path::PathBuf;

use jni::{
    JNIEnv,
    objects::{JClass, JObject, JString},
};
use tracing::error;

use crate::config::FfiStoreOptions;

#[expect(unreachable_pub, reason = "JNI entrypoint must remain exported")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kithara_Kithara_nativeSetStoreOptions(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    store: JObject<'_>,
) {
    if let Err(err) = set_store_options(&mut env, store) {
        let message = format!("failed to set Android store options: {err}");
        error!(message = %message);
        let _ = env.throw_new("java/lang/IllegalStateException", &message);
    }
}

#[expect(unreachable_pub, reason = "JNI entrypoint must remain exported")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kithara_Kithara_nativeInitRustlsPlatformVerifier(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    context: JObject<'_>,
) {
    if let Err(err) = rustls_platform_verifier::android::init_with_env(&mut env, context) {
        let message = format!("failed to initialize rustls platform verifier: {err}");
        error!(message = %message);
        let _ = env.throw_new("java/lang/IllegalStateException", &message);
    }
}

fn set_store_options(env: &mut JNIEnv<'_>, store: JObject<'_>) -> Result<(), jni::errors::Error> {
    let cache_dir = env
        .call_method(&store, "getCacheDir", "()Ljava/lang/String;", &[])?
        .l()?;
    let cache_dir = if cache_dir.is_null() {
        None
    } else {
        Some(PathBuf::from(read_string(env, JString::from(cache_dir))?))
    };
    crate::config::set_store_options(FfiStoreOptions { cache_dir });
    Ok(())
}

fn read_string(env: &mut JNIEnv<'_>, value: JString<'_>) -> Result<String, jni::errors::Error> {
    Ok(env.get_string(&value)?.into())
}
