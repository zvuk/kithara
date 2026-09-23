use std::{
    fmt,
    sync::{
        LazyLock,
        atomic::{AtomicI64, Ordering},
    },
};

use dashmap::DashMap;
use jni::{
    Env, jni_sig, jni_str,
    objects::{Global, JClass, JObject, JObjectArray, JString, JValue},
    signature::MethodSignature,
    sys::jlong,
};
use kithara_net::host::{
    self, HostBuffer, HostCall, HostEvents, HostFailure, HostRequest, HostRequestBody,
    HostTransport,
};
use kithara_platform::sync::{Arc, Mutex, MutexGuard};

use crate::{
    buffer::DirectBuffer, error::AndroidBackendError, method::BoundMethod,
    runtime::with_attached_env,
};

/// The calls in flight, found by the handle their callbacks carry.
static CALLS: LazyLock<Calls> = LazyLock::new(|| Calls {
    slots: DashMap::new(),
    next_handle: AtomicI64::new(1),
});

/// Wrap the Kotlin `transport` and install it into `kithara-net`.
pub(super) fn install(
    env: &mut Env<'_>,
    transport: &JObject<'_>,
) -> Result<(), AndroidBackendError> {
    // Found from the installing call, whose class loader sees the callback.
    let callback = env
        .find_class(jni_str!("com/kithara/net/NativeHttpCallback"))
        .map_err(AndroidBackendError::jni("jni-callback-class"))?;
    let callback = env
        .new_global_ref(&callback)
        .map_err(AndroidBackendError::jni("jni-callback-class-global"))?;
    let object = env
        .new_global_ref(transport)
        .map_err(AndroidBackendError::jni("jni-transport-global"))?;
    let start = BoundMethod::bind(env, object, jni_str!("start"), JniTransport::START)?;
    host::install(Arc::new(JniTransport { start, callback })).map_err(|error| {
        AndroidBackendError::operation("http-transport-install", error.to_string())
    })
}

pub(super) fn slot(handle: jlong) -> Option<Arc<Slot>> {
    CALLS
        .slots
        .get(&handle)
        .map(|entry| Arc::clone(entry.value()))
}

pub(super) fn finish(handle: jlong) -> Option<Arc<Slot>> {
    CALLS.slots.remove(&handle).map(|(_, slot)| slot)
}

struct Calls {
    slots: DashMap<jlong, Arc<Slot>>,
    next_handle: AtomicI64,
}

/// One call between its start and the transport's terminal callback. The
/// buffers it lent live as long as it does.
pub(super) struct Slot {
    pub(super) events: HostEvents,
    lent: Mutex<Lent>,
}

#[derive(Default)]
struct Lent {
    body: Option<DirectBuffer<HostRequestBody>>,
    read: Option<DirectBuffer<HostBuffer>>,
}

impl Slot {
    fn lend_read(&self, buffer: DirectBuffer<HostBuffer>) {
        self.lent().read = Some(buffer);
    }

    pub(super) fn take_read(&self) -> Option<HostBuffer> {
        self.lent().read.take().map(DirectBuffer::into_inner)
    }

    fn lent(&self) -> MutexGuard<'_, Lent> {
        self.lent.lock()
    }
}

struct JniTransport {
    start: BoundMethod,
    callback: Global<JClass<'static>>,
}

impl JniTransport {
    const START: MethodSignature<'static, 'static> = jni_sig!(
        (method: java.lang.String, url: java.lang.String, headers: [java.lang.String],
         body: java.nio.ByteBuffer, callback: "com.kithara.net.HttpCallback")
            -> "com.kithara.net.HttpCall"
    );

    /// Hand `request` to the Kotlin transport and return the call it started.
    fn start_call<'local>(
        &self,
        env: &mut Env<'local>,
        request: HostRequest,
        handle: jlong,
        slot: &Slot,
    ) -> Result<JObject<'local>, AndroidBackendError> {
        let method = new_string(env, request.method.as_str())?;
        let url = new_string(env, request.url.as_str())?;
        let headers = string_array(env, &request.headers)?;
        let callback = env
            .new_object(
                &*self.callback,
                jni_sig!((handle: jlong)),
                &[JValue::Long(handle)],
            )
            .map_err(AndroidBackendError::jni("jni-new-callback"))?;
        let body = request
            .body
            .map(|body| DirectBuffer::new(env, body))
            .transpose()?;
        let absent = JObject::null();
        let body_object = body
            .as_ref()
            .map_or(&absent, |buffer| buffer.object().as_obj());
        let body_argument = env
            .new_local_ref(body_object)
            .map_err(AndroidBackendError::jni("jni-body-local"))?;
        slot.lent().body = body;

        self.start
            .call(
                env,
                &[
                    JValue::Object(&method),
                    JValue::Object(&url),
                    JValue::Object(&headers),
                    JValue::Object(&body_argument),
                    JValue::Object(&callback),
                ],
            )?
            .l()
            .map_err(AndroidBackendError::jni("jni-transport-start"))
    }
}

impl HostTransport for JniTransport {
    fn start(
        &self,
        request: HostRequest,
        events: HostEvents,
    ) -> Result<Box<dyn HostCall>, HostFailure> {
        let handle = CALLS.next_handle.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(Slot {
            events,
            lent: Mutex::default(),
        });
        CALLS.slots.insert(handle, Arc::clone(&slot));
        let mut started = false;
        let call = with_attached_env(|env| {
            let call = self.start_call(env, request, handle, &slot)?;
            // From here the Kotlin call owns the slot: its terminal callback
            // removes it, and the slot keeps the lent body until then.
            started = true;
            JniCall::bind(env, &call, &slot).inspect_err(|_| cancel_unbound(env, &call))
        });
        call.map(|call| Box::new(call) as Box<dyn HostCall>)
            .map_err(|error| {
                if !started {
                    CALLS.slots.remove(&handle);
                }
                HostFailure::Protocol(error.to_string())
            })
    }
}

/// Stop a call Kithara could not bind, so its terminal callback settles the slot.
fn cancel_unbound(env: &mut Env<'_>, call: &JObject<'_>) {
    // A failed bind may leave its exception pending.
    env.exception_clear();
    if let Err(error) = env.call_method(call, jni_str!("cancel"), JniCall::CANCEL, &[]) {
        env.exception_clear();
        tracing::warn!(%error, "cancelling an unbound HTTP call failed");
    }
}

impl fmt::Debug for JniTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JniTransport")
            .field("calls", &CALLS.slots.len())
            .finish_non_exhaustive()
    }
}

struct JniCall {
    read: BoundMethod,
    cancel: BoundMethod,
    slot: Arc<Slot>,
}

impl JniCall {
    const CANCEL: MethodSignature<'static, 'static> = jni_sig!(());
    const READ: MethodSignature<'static, 'static> = jni_sig!((buffer: java.nio.ByteBuffer));

    fn bind(
        env: &mut Env<'_>,
        call: &JObject<'_>,
        slot: &Arc<Slot>,
    ) -> Result<Self, AndroidBackendError> {
        Ok(Self {
            read: BoundMethod::bind(env, call_global(env, call)?, jni_str!("read"), Self::READ)?,
            cancel: BoundMethod::bind(
                env,
                call_global(env, call)?,
                jni_str!("cancel"),
                Self::CANCEL,
            )?,
            slot: Arc::clone(slot),
        })
    }

    fn lend(&self, env: &mut Env<'_>, buffer: HostBuffer) -> Result<(), AndroidBackendError> {
        let buffer = DirectBuffer::new(env, buffer)?;
        let argument = env
            .new_local_ref(buffer.object())
            .map_err(AndroidBackendError::jni("jni-buffer-local"))?;
        // The transport may answer before `read` returns, so lend first.
        self.slot.lend_read(buffer);
        self.read.call(env, &[JValue::Object(&argument)])?;
        Ok(())
    }
}

impl HostCall for JniCall {
    fn read(&self, buffer: HostBuffer) {
        if let Err(error) = with_attached_env(|env| self.lend(env, buffer)) {
            self.slot
                .events
                .fail(HostFailure::Protocol(error.to_string()));
        }
    }

    fn cancel(&self) {
        if let Err(error) = with_attached_env(|env| self.cancel.call(env, &[]).map(drop)) {
            tracing::debug!(%error, "cancelling an HTTP call failed");
        }
    }
}

fn call_global(
    env: &Env<'_>,
    call: &JObject<'_>,
) -> Result<Global<JObject<'static>>, AndroidBackendError> {
    env.new_global_ref(call)
        .map_err(AndroidBackendError::jni("jni-call-global"))
}

fn new_string<'local>(
    env: &mut Env<'local>,
    value: &str,
) -> Result<JString<'local>, AndroidBackendError> {
    env.new_string(value)
        .map_err(AndroidBackendError::jni("jni-new-string"))
}

fn string_array<'local>(
    env: &mut Env<'local>,
    pairs: &[(String, String)],
) -> Result<JObjectArray<'local, JString<'local>>, AndroidBackendError> {
    let array = JObjectArray::<'_, JString<'_>>::new(env, pairs.len() * 2, JString::default())
        .map_err(AndroidBackendError::jni("jni-new-string-array"))?;
    for (index, value) in pairs
        .iter()
        .flat_map(|(name, value)| [name, value])
        .enumerate()
    {
        let element = new_string(env, value)?;
        array
            .set_element(env, index, &element)
            .map_err(AndroidBackendError::jni("jni-set-string-array"))?;
        // Bounds the local reference frame for any header count.
        env.delete_local_ref(element);
    }
    Ok(array)
}
