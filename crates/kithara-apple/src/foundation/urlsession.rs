use std::ptr;

use kithara_platform::sync::Arc;

use super::{
    block::{DynBlock, RcBlock},
    ns::{
        NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSOperationQueue, NSString,
        NSURLAuthenticationChallenge, NSURLAuthenticationMethodServerTrust, NSURLCredential,
        NSURLProtectionSpace, NSURLResponse, NSURLSession, NSURLSessionAuthChallengeDisposition,
        NSURLSessionConfiguration, NSURLSessionDataDelegate, NSURLSessionDataTask,
        NSURLSessionDelegate, NSURLSessionResponseDisposition, NSURLSessionTask,
        NSURLSessionTaskDelegate, NSURLSessionTaskMetrics,
    },
    objc::{
        AnyThread, ClassType, DefinedClass, Encoding, RefEncode, define_class, msg_send,
        rc::Retained,
        runtime::{NSObject, NSObjectProtocol, ProtocolObject},
    },
};

pub type TaskId = super::ns::NSUInteger;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseParts {
    headers: Vec<(String, String)>,
    status: Option<u16>,
}

impl From<ResponseParts> for (Option<u16>, Vec<(String, String)>) {
    fn from(parts: ResponseParts) -> Self {
        (parts.status, parts.headers)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AuthenticationChallengeDisposition {
    PerformDefaultHandling,
    UseServerTrustCredential,
}

pub struct AuthenticationChallenge<'a> {
    challenge: &'a NSURLAuthenticationChallenge,
}

impl<'a> AuthenticationChallenge<'a> {
    fn new(challenge: &'a NSURLAuthenticationChallenge) -> Self {
        Self { challenge }
    }

    #[must_use]
    pub fn is_server_trust(&self) -> bool {
        is_server_trust_challenge(self.challenge)
    }
}

pub trait UrlSessionEvents: Send + Sync + 'static {
    fn did_complete(&self, task_id: TaskId, error: Option<&NSError>);

    fn did_finish_collecting_metrics(&self, task_id: TaskId, metrics: &NSURLSessionTaskMetrics);

    fn did_receive_authentication_challenge(
        &self,
        challenge: &AuthenticationChallenge<'_>,
    ) -> AuthenticationChallengeDisposition;

    fn did_receive_data(&self, task_id: TaskId, data: &NSData);

    fn did_receive_response(&self, task_id: TaskId, response: &NSURLResponse);
}

pub trait DataTaskCompletion: Send + Sync + 'static {
    fn complete(&self, completion: DataCompletion<'_>);
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct DataCompletion<'a> {
    #[field(get)]
    data: Option<&'a NSData>,
    #[field(get)]
    error: Option<&'a NSError>,
    response: Option<&'a NSURLResponse>,
}

impl<'a> DataCompletion<'a> {
    fn new(data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError) -> Self {
        // SAFETY: NSURLSession owns callback pointers for this callback.
        let error = unsafe { error.as_ref() };
        // SAFETY: NSURLSession owns callback pointers for this callback.
        let data = unsafe { data.as_ref() };
        // SAFETY: NSURLSession owns callback pointers for this callback.
        let response = unsafe { response.as_ref() };
        Self {
            data,
            error,
            response,
        }
    }

    pub fn response_parts(&self) -> Option<ResponseParts> {
        self.response.and_then(response_parts)
    }
}

pub struct DelegateIvars {
    events: Arc<dyn UrlSessionEvents>,
}

#[repr(C)]
struct SecTrust {
    _opaque: [u8; 0],
}

// SAFETY: SecTrustRef uses the CoreFoundation `^{__SecTrust=}` ObjC encoding.
unsafe impl RefEncode for SecTrust {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__SecTrust", &[]));
}

define_class!(
    // SAFETY: NSObject delegate ivars hold a Send + Sync event sink.
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[name = "KitharaAppleUrlSessionDelegate"]
    #[ivars = DelegateIvars]
    pub struct UrlSessionDelegate;

    impl UrlSessionDelegate {
        #[unsafe(method(URLSession:didReceiveChallenge:completionHandler:))]
        fn receive_session_challenge(
            &self,
            _session: &NSURLSession,
            challenge: &NSURLAuthenticationChallenge,
            completion_handler: &DynBlock<
                dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential),
            >,
        ) {
            self.complete_challenge(challenge, completion_handler);
        }

        #[unsafe(method(URLSession:task:didReceiveChallenge:completionHandler:))]
        fn receive_task_challenge(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            challenge: &NSURLAuthenticationChallenge,
            completion_handler: &DynBlock<
                dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential),
            >,
        ) {
            self.complete_challenge(challenge, completion_handler);
        }

        #[unsafe(method(URLSession:dataTask:didReceiveResponse:completionHandler:))]
        fn receive_response(
            &self,
            _session: &NSURLSession,
            data_task: &NSURLSessionDataTask,
            response: &NSURLResponse,
            completion_handler: &DynBlock<dyn Fn(NSURLSessionResponseDisposition)>,
        ) {
            self.ivars()
                .events
                .did_receive_response(data_task.taskIdentifier(), response);
            completion_handler.call((NSURLSessionResponseDisposition::Allow,));
        }

        #[unsafe(method(URLSession:dataTask:didReceiveData:))]
        fn receive_data(
            &self,
            _session: &NSURLSession,
            data_task: &NSURLSessionDataTask,
            data: &NSData,
        ) {
            self.ivars()
                .events
                .did_receive_data(data_task.taskIdentifier(), data);
        }

        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        fn complete_task(
            &self,
            _session: &NSURLSession,
            task: &NSURLSessionTask,
            error: Option<&NSError>,
        ) {
            self.ivars()
                .events
                .did_complete(task.taskIdentifier(), error);
        }

        #[unsafe(method(URLSession:task:didFinishCollectingMetrics:))]
        fn collect_metrics(
            &self,
            _session: &NSURLSession,
            task: &NSURLSessionTask,
            metrics: &NSURLSessionTaskMetrics,
        ) {
            let task_id = task.taskIdentifier();
            self.ivars()
                .events
                .did_finish_collecting_metrics(task_id, metrics);
        }

    }

    // SAFETY: NSObjectProtocol has no additional invariants for this delegate.
    unsafe impl NSObjectProtocol for UrlSessionDelegate {}

    // SAFETY: Delegate callbacks forward borrowed Objective-C references only.
    unsafe impl NSURLSessionDelegate for UrlSessionDelegate {}

    // SAFETY: Task callbacks share the same event sink contract.
    unsafe impl NSURLSessionTaskDelegate for UrlSessionDelegate {}

    // SAFETY: NSData is forwarded only for the callback duration.
    unsafe impl NSURLSessionDataDelegate for UrlSessionDelegate {}
);

impl UrlSessionDelegate {
    pub fn new(events: Arc<dyn UrlSessionEvents>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { events });
        // SAFETY: The ivars were initialized before calling NSObject init.
        unsafe { msg_send![super(this), init] }
    }

    fn complete_challenge(
        &self,
        challenge: &NSURLAuthenticationChallenge,
        completion_handler: &DynBlock<
            dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential),
        >,
    ) {
        let disposition = self
            .ivars()
            .events
            .did_receive_authentication_challenge(&AuthenticationChallenge::new(challenge));
        if disposition == AuthenticationChallengeDisposition::UseServerTrustCredential {
            let credential = server_trust_credential(challenge);
            if !credential.is_null() {
                completion_handler.call((
                    NSURLSessionAuthChallengeDisposition::UseCredential,
                    credential,
                ));
                return;
            }
        }
        completion_handler.call((
            NSURLSessionAuthChallengeDisposition::PerformDefaultHandling,
            ptr::null_mut(),
        ));
    }
}

pub fn data_task_with_completion(
    session: &NSURLSession,
    request: &NSMutableURLRequest,
    completion: Arc<dyn DataTaskCompletion>,
) -> Retained<NSURLSessionDataTask> {
    let completion = RcBlock::new(
        move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            completion.complete(DataCompletion::new(data, response, error));
        },
    );
    // SAFETY: NSURLSession copies the block before returning the task.
    unsafe { session.dataTaskWithRequest_completionHandler(request, &completion) }
}

pub fn session_with_delegate(
    configuration: &NSURLSessionConfiguration,
    delegate: &UrlSessionDelegate,
    queue: &NSOperationQueue,
) -> Retained<NSURLSession> {
    let delegate_ref: &ProtocolObject<dyn NSURLSessionDelegate> =
        ProtocolObject::from_ref(delegate);
    // SAFETY: The delegate implements NSURLSessionDelegate.
    unsafe {
        NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
            configuration,
            Some(delegate_ref),
            Some(queue),
        )
    }
}

pub fn data_bytes(data: &NSData) -> &[u8] {
    // SAFETY: The slice is tied to the borrowed NSData lifetime.
    unsafe { data.as_bytes_unchecked() }
}

pub fn response_parts(response: &NSURLResponse) -> Option<ResponseParts> {
    let http = http_response(response)?;
    let status = u16::try_from(http.statusCode()).ok();
    Some(ResponseParts {
        status,
        headers: header_pairs(http),
    })
}

fn http_response(response: &NSURLResponse) -> Option<&NSHTTPURLResponse> {
    if response.isKindOfClass(NSHTTPURLResponse::class()) {
        let raw = ptr::from_ref(response).cast::<NSHTTPURLResponse>();
        // SAFETY: Foundation reported this object as NSHTTPURLResponse.
        Some(unsafe { &*raw })
    } else {
        None
    }
}

fn header_pairs(response: &NSHTTPURLResponse) -> Vec<(String, String)> {
    let fields = response.allHeaderFields();
    // SAFETY: URLSession HTTP header dictionaries use NSString pairs.
    let fields = unsafe { fields.cast_unchecked::<NSString, NSString>() };
    let (keys, values) = fields.to_vecs();
    keys.into_iter()
        .zip(values)
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn is_server_trust_challenge(challenge: &NSURLAuthenticationChallenge) -> bool {
    let space = challenge.protectionSpace();
    let method = space.authenticationMethod();
    // SAFETY: Foundation owns this constant NSString for the process lifetime.
    let server_trust = unsafe { NSURLAuthenticationMethodServerTrust };
    method.isEqualToString(server_trust)
}

fn server_trust_credential(challenge: &NSURLAuthenticationChallenge) -> *mut NSURLCredential {
    let space = challenge.protectionSpace();
    let trust = server_trust(space.as_ref());
    if trust.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `credentialForTrust:` is the documented SecTrust constructor.
    unsafe { msg_send![NSURLCredential::class(), credentialForTrust: trust] }
}

fn server_trust(space: &NSURLProtectionSpace) -> *mut SecTrust {
    // SAFETY: Callers already checked for a server-trust protection space.
    unsafe { msg_send![space, serverTrust] }
}
