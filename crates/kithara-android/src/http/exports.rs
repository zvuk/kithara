use jni::{
    EnvUnowned,
    errors::{Error, ThrowRuntimeExAndDefault},
    objects::{JClass, JObject, JObjectArray, JString},
    sys::{jboolean, jint, jlong},
};

use super::{callback, transport};

#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_net_NativeHttpTransport_install(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    transport: JObject<'_>,
) {
    env.with_env(|env| transport::install(env, &transport))
        .resolve::<ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_net_NativeHttpCallback_nativeOnResponse(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    status: jint,
    headers: JObjectArray<'_, JString<'_>>,
) {
    env.with_env(|env| callback::on_response(env, handle, status, &headers))
        .resolve::<ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_net_NativeHttpCallback_nativeOnRead(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    bytes: jint,
) {
    env.with_env(|_env| {
        callback::on_read(handle, bytes);
        Ok::<(), Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_net_NativeHttpCallback_nativeOnEnd(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    env.with_env(|_env| {
        callback::on_end(handle);
        Ok::<(), Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_com_kithara_net_NativeHttpCallback_nativeOnFailed(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    message: JString<'_>,
    permanent: jboolean,
) {
    env.with_env(|env| {
        callback::on_failed(env, handle, &message, permanent);
        Ok::<(), Error>(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}
