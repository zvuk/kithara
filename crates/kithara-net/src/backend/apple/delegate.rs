use dashmap::DashMap;
use kithara_apple::foundation::{
    ns::{NSData, NSError, NSURLResponse, NSURLSessionTaskMetrics},
    urlsession::{
        self, AuthenticationChallenge, AuthenticationChallengeDisposition, TaskId, UrlSessionEvents,
    },
};
use kithara_platform::{sync::Arc, tokio::sync::oneshot};
use url::Url;

use super::{
    response::{StreamHead, error_from_nserror, http_parts},
    stream::AppleBodyQueue,
};
use crate::{error::NetError, metrics::ConnectionMetrics, types::AcceptEncodingPolicy};

pub(super) struct AppleSessionEvents {
    allow_invalid_tls: bool,
    streams: DashMap<TaskId, StreamState>,
    task_metrics: DashMap<TaskId, ConnectionMetrics>,
}

pub(super) struct StreamState {
    pub(super) accept_encoding: AcceptEncodingPolicy,
    pub(super) body_queue: Option<Arc<AppleBodyQueue>>,
    pub(super) head_sender: Option<oneshot::Sender<Result<StreamHead, NetError>>>,
    pub(super) url: Url,
}

impl AppleSessionEvents {
    pub(super) fn new(allow_invalid_tls: bool) -> Arc<Self> {
        Arc::new(Self {
            allow_invalid_tls,
            streams: DashMap::new(),
            task_metrics: DashMap::new(),
        })
    }

    pub(super) fn register_metrics(&self, task_id: TaskId, metrics: ConnectionMetrics) {
        self.task_metrics.insert(task_id, metrics);
    }

    pub(super) fn register_stream(&self, task_id: TaskId, state: StreamState) {
        self.streams.insert(task_id, state);
    }

    pub(super) fn remove_stream(&self, task_id: TaskId) {
        self.streams.remove(&task_id);
    }

    fn take_stream(&self, task_id: TaskId) -> Option<StreamState> {
        self.streams.remove(&task_id).map(|(_task_id, state)| state)
    }
}

impl UrlSessionEvents for AppleSessionEvents {
    fn did_complete(&self, task_id: TaskId, error: Option<&NSError>) {
        let terminal = error.map(error_from_nserror);
        let Some(mut state) = self.take_stream(task_id) else {
            return;
        };
        if let Some(sender) = state.head_sender.take() {
            let result = terminal.clone().map_or_else(
                || {
                    Err(NetError::Network(
                        "NSURLSession completed before response headers".to_string(),
                    ))
                },
                Err,
            );
            let _ = sender.send(result);
        }
        if let Some(queue) = state.body_queue.take() {
            queue.close(terminal);
        }
    }

    fn did_finish_collecting_metrics(&self, task_id: TaskId, metrics: &NSURLSessionTaskMetrics) {
        let Some((_, connection_metrics)) = self.task_metrics.remove(&task_id) else {
            return;
        };
        let transactions = metrics.transactionMetrics();
        for transaction in &transactions {
            if !transaction.isReusedConnection() {
                connection_metrics.record_opened_connection();
            }
        }
    }

    fn did_receive_authentication_challenge(
        &self,
        challenge: &AuthenticationChallenge<'_>,
    ) -> AuthenticationChallengeDisposition {
        if self.allow_invalid_tls && challenge.is_server_trust() {
            AuthenticationChallengeDisposition::UseServerTrustCredential
        } else {
            AuthenticationChallengeDisposition::PerformDefaultHandling
        }
    }

    fn did_receive_data(&self, task_id: TaskId, data: &NSData) {
        let queue = self
            .streams
            .get(&task_id)
            .and_then(|state| state.body_queue.as_ref().map(Arc::clone));
        if let Some(queue) = queue {
            queue.push_data(data);
        }
    }

    fn did_receive_response(&self, task_id: TaskId, response: &NSURLResponse) {
        let Some(mut state) = self.streams.get_mut(&task_id) else {
            return;
        };
        let head = http_parts(response, state.accept_encoding, &state.url).and_then(|parts| {
            parts
                .map(|(status, headers)| StreamHead { headers, status })
                .ok_or_else(|| {
                    NetError::Network(
                        "NSURLSession returned a non-HTTP stream response".to_string(),
                    )
                })
        });
        if let Some(sender) = state.head_sender.take() {
            let _ = sender.send(head);
        }
    }
}

pub(super) fn make_delegate(
    events: &Arc<AppleSessionEvents>,
) -> kithara_apple::foundation::objc::rc::Retained<urlsession::UrlSessionDelegate> {
    let event_sink: Arc<dyn UrlSessionEvents> = events.clone();
    urlsession::UrlSessionDelegate::new(event_sink)
}
