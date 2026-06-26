#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    ptr,
    sync::{Arc, Mutex},
};

use block2::DynBlock;
use bytes::Bytes;
use dashmap::DashMap;
use kithara_platform::tokio::sync::{mpsc, oneshot};
use objc2::{
    AnyThread, ClassType, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol},
};
use objc2_foundation::{
    NSData, NSError, NSURLAuthenticationChallenge, NSURLAuthenticationMethodServerTrust,
    NSURLCredential, NSURLProtectionSpace, NSURLResponse, NSURLSession,
    NSURLSessionAuthChallengeDisposition, NSURLSessionDataDelegate, NSURLSessionDataTask,
    NSURLSessionDelegate, NSURLSessionResponseDisposition, NSURLSessionTask,
    NSURLSessionTaskDelegate,
};

use super::{
    response::{StreamHead, copy_data, error_from_nserror, http_parts, store_terminal},
    session::TaskId,
};
use crate::error::NetError;

pub(super) struct DelegateIvars {
    streams: DashMap<TaskId, StreamState>,
    is_insecure: bool,
}

pub(super) struct StreamState {
    pub(super) terminal: Arc<Mutex<Option<NetError>>>,
    pub(super) body_sender: Option<mpsc::Sender<Result<Bytes, NetError>>>,
    pub(super) head_sender: Option<oneshot::Sender<Result<StreamHead, NetError>>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements for a delegate object.
    // - DelegateIvars is Send + Sync; stream state is synchronized for callbacks
    //   from the URLSession delegate queue.
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[name = "KitharaNetAppleSessionDelegate"]
    #[ivars = DelegateIvars]
    pub(super) struct AppleSessionDelegate;

    impl AppleSessionDelegate {
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
            let task_id = data_task.taskIdentifier();
            let head = http_parts(response)
                .map(|(status, headers)| StreamHead { headers, status })
                .ok_or_else(|| {
                    NetError::Network("NSURLSession returned a non-HTTP stream response".to_string())
                });
            if let Some(mut state) = self.ivars().streams.get_mut(&task_id)
                && let Some(sender) = state.head_sender.take()
            {
                let _ = sender.send(head);
            }
            completion_handler.call((NSURLSessionResponseDisposition::Allow,));
        }

        #[unsafe(method(URLSession:dataTask:didReceiveData:))]
        fn receive_data(
            &self,
            _session: &NSURLSession,
            data_task: &NSURLSessionDataTask,
            data: &NSData,
        ) {
            let task_id = data_task.taskIdentifier();
            let stream = {
                self.ivars().streams.get(&task_id).and_then(|state| {
                    state
                        .body_sender
                        .as_ref()
                        .map(|sender| (sender.clone(), Arc::clone(&state.terminal)))
                })
            };
            if let Some((sender, terminal)) = stream {
                // CONTEXT: NSURLSession invokes this on the session's
                // NSOperationQueue delegate thread. Delegate callbacks must
                // never block that thread or depend on Tokio runtime context,
                // so body backpressure is handled with try_send and task
                // cancellation instead of blocking_send.
                match sender.try_send(Ok(copy_data(data))) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(Ok(_))) => {
                        store_terminal(&terminal, NetError::Timeout);
                        data_task.cancel();
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        data_task.cancel();
                    }
                    Err(mpsc::error::TrySendError::Full(Err(error))) => {
                        store_terminal(&terminal, error);
                        data_task.cancel();
                    }
                }
            }
        }

        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        fn complete_task(
            &self,
            _session: &NSURLSession,
            task: &NSURLSessionTask,
            error: Option<&NSError>,
        ) {
            let task_id = task.taskIdentifier();
            let Some(mut state) = self.take_stream(task_id) else {
                return;
            };
            let terminal = error.map(error_from_nserror);
            if let Some(sender) = state.head_sender.take() {
                let result = terminal.clone().map_or_else(
                    || Err(NetError::Network("NSURLSession completed before response headers".to_string())),
                    Err,
                );
                let _ = sender.send(result);
            }
            if let Some(error) = terminal
                && let Some(sender) = state.body_sender.take()
            {
                // CONTEXT: Completion also runs on the NSURLSession delegate
                // queue, outside Tokio's async task context. try_send keeps
                // the queue unblocked; if the bounded channel is full, the
                // stream observes the stored terminal error on its next poll.
                if let Err(mpsc::error::TrySendError::Full(Err(error))) =
                    sender.try_send(Err(error))
                {
                    store_terminal(&state.terminal, error);
                }
            }
        }
    }

    // SAFETY: NSObjectProtocol has no additional invariants for this delegate.
    unsafe impl NSObjectProtocol for AppleSessionDelegate {}

    // SAFETY: The delegate stores Send + Sync Rust state, and all callbacks
    // synchronize mutable access through `streams`.
    unsafe impl NSURLSessionDelegate for AppleSessionDelegate {}

    // SAFETY: Task callbacks share the same synchronized delegate state.
    unsafe impl NSURLSessionTaskDelegate for AppleSessionDelegate {}

    // SAFETY: Data callbacks copy NSData into Bytes before crossing into Rust.
    unsafe impl NSURLSessionDataDelegate for AppleSessionDelegate {}
);

impl AppleSessionDelegate {
    pub(super) fn new(is_insecure: bool) -> Retained<Self> {
        let streams = DashMap::new();
        let this = Self::alloc().set_ivars(DelegateIvars {
            streams,
            is_insecure,
        });
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
        if self.ivars().is_insecure && is_server_trust_challenge(challenge) {
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

    pub(super) fn register_stream(&self, task_id: TaskId, state: StreamState) {
        self.ivars().streams.insert(task_id, state);
    }

    pub(super) fn remove_stream(&self, task_id: TaskId) {
        self.ivars().streams.remove(&task_id);
    }

    fn take_stream(&self, task_id: TaskId) -> Option<StreamState> {
        self.ivars()
            .streams
            .remove(&task_id)
            .map(|(_task_id, state)| state)
    }
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
    // SAFETY: `credentialForTrust:` is skipped by objc2-foundation, but it is
    // the documented NSURLCredential constructor for a non-null SecTrustRef.
    unsafe { msg_send![NSURLCredential::class(), credentialForTrust: trust] }
}

fn server_trust(space: &NSURLProtectionSpace) -> *mut c_void {
    // SAFETY: `serverTrust` is skipped by objc2-foundation. It is valid only
    // for server-trust protection spaces, which callers check before this.
    unsafe { msg_send![space, serverTrust] }
}
