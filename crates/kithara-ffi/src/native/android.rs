#[cfg(feature = "test")]
mod capture;
mod init;
#[cfg(feature = "test")]
mod probe;

use jni::{
    EnvUnowned,
    errors::ThrowRuntimeExAndDefault,
    objects::{JClass, JObject},
    sys::jint,
};
#[cfg(feature = "test")]
use jni::{objects::JString, sys::jlong};

/// Publish the host runtime and install logging.
#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_Kithara_nativeInit(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    context: JObject<'_>,
    log_level: jint,
) {
    env.with_env(|env| init::run(env, context, log_level))
        .resolve::<ThrowRuntimeExAndDefault>();
}

/// Render `seconds` of `input` through an offline Host into a float WAV at
/// `output`.
#[cfg(feature = "test")]
#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_Kithara_nativeRunOfflineCapture(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    input: JString<'_>,
    output: JString<'_>,
    seconds: jint,
) {
    env.with_env(|env| capture::run(env, &input, &output, seconds))
        .resolve::<ThrowRuntimeExAndDefault>();
}

/// Report the default output sample format and log every supported config.
#[cfg(feature = "test")]
#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_Kithara_nativeProbeAndroidAudio(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jlong {
    env.with_env(|_env| probe::default_output_format())
        .resolve::<ThrowRuntimeExAndDefault>()
}
