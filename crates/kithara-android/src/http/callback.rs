use jni::{
    Env,
    errors::Error,
    objects::{JObjectArray, JString},
    sys::{jboolean, jint, jlong},
};
use kithara_net::host::HostFailure;
use kithara_platform::sync::Arc;

use super::transport::{self, Slot};

pub(super) fn on_response(
    env: &mut Env<'_>,
    handle: jlong,
    status: jint,
    headers: &JObjectArray<'_, JString<'_>>,
) -> Result<(), Error> {
    let Some(slot) = slot(handle) else {
        return Ok(());
    };
    match u16::try_from(status) {
        Ok(status) => slot.events.response(status, header_pairs(env, headers)?),
        Err(_) => violate(&slot, format!("HTTP status {status} is out of range")),
    }
    Ok(())
}

pub(super) fn on_read(handle: jlong, bytes: jint) {
    let Some(slot) = slot(handle) else {
        return;
    };
    match (slot.take_read(), usize::try_from(bytes)) {
        (Some(buffer), Ok(len)) => slot.events.read(buffer, len),
        (None, _) => violate(&slot, "a read of a buffer nobody lent".to_owned()),
        (Some(_), Err(_)) => violate(&slot, format!("a read of {bytes} bytes")),
    }
}

pub(super) fn on_end(handle: jlong) {
    if let Some(slot) = finish(handle) {
        slot.events.end();
    }
}

/// Kotlin reports failures from its catch blocks, so this settles the call
/// and returns nothing that could throw there.
pub(super) fn on_failed(
    env: &mut Env<'_>,
    handle: jlong,
    message: &JString<'_>,
    permanent: jboolean,
) {
    let Some(slot) = finish(handle) else {
        return;
    };
    let failure = match message.try_to_string(env) {
        Ok(message) if permanent => HostFailure::Permanent(message),
        Ok(message) => HostFailure::Transport(message),
        Err(error) => {
            env.exception_clear();
            HostFailure::Protocol(format!("an unreadable failure message: {error}"))
        }
    };
    slot.events.fail(failure);
}

fn slot(handle: jlong) -> Option<Arc<Slot>> {
    let slot = transport::slot(handle);
    if slot.is_none() {
        tracing::warn!(handle, "a callback for no call in flight");
    }
    slot
}

fn finish(handle: jlong) -> Option<Arc<Slot>> {
    let slot = transport::finish(handle);
    if slot.is_none() {
        tracing::warn!(handle, "a terminal callback for no call in flight");
    }
    slot
}

fn violate(slot: &Slot, message: String) {
    slot.events.fail(HostFailure::Protocol(message));
}

fn header_pairs(
    env: &mut Env<'_>,
    array: &JObjectArray<'_, JString<'_>>,
) -> Result<Vec<(String, String)>, Error> {
    let len = array.len(env)?;
    (0..len / 2)
        .map(|pair| {
            let name = string_at(env, array, pair * 2)?;
            let value = string_at(env, array, pair * 2 + 1)?;
            Ok((name, value))
        })
        .collect()
}

fn string_at(
    env: &mut Env<'_>,
    array: &JObjectArray<'_, JString<'_>>,
    index: usize,
) -> Result<String, Error> {
    let element = array.get_element(env, index)?;
    let value = element.try_to_string(env)?;
    // Bounds the local reference frame for any header count.
    env.delete_local_ref(element);
    Ok(value)
}
