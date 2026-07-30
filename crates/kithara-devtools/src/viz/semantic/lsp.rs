use std::{
    fmt,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;

const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) enum ClientError {
    Unavailable(String),
    TimedOut { method: String },
    Protocol(String),
    Io(std::io::Error),
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Protocol(message) => formatter.write_str(message),
            Self::TimedOut { method } => {
                write!(formatter, "rust-analyzer timed out during {method}")
            }
            Self::Io(error) => write!(formatter, "rust-analyzer I/O: {error}"),
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Position {
    pub(super) line: u32,
    pub(super) character: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Range {
    pub(super) start: Position,
    pub(super) end: Position,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CallHierarchyItem {
    pub(super) name: String,
    pub(super) kind: u32,
    pub(super) uri: Url,
    pub(super) range: Range,
    pub(super) selection_range: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) data: Option<Value>,
}

impl CallHierarchyItem {
    #[cfg(test)]
    pub(super) fn fixture(name: &str, uri: Url, line: u32) -> Self {
        let position = Position { line, character: 0 };
        let range = Range {
            start: position.clone(),
            end: position,
        };
        Self {
            name: name.to_string(),
            kind: 12,
            uri,
            range: range.clone(),
            selection_range: range,
            detail: None,
            data: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct OutgoingCall {
    pub(super) to: CallHierarchyItem,
}

pub(super) struct Client {
    child: Option<Child>,
    stdin: ChildStdin,
    responses: Receiver<Result<Value, String>>,
    next_id: u64,
    deadline: Instant,
}

impl Client {
    pub(super) fn start(root: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let mut child = Command::new("rust-analyzer")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ClientError::Unavailable("rust-analyzer is not available on PATH".to_string())
                } else {
                    ClientError::Io(error)
                }
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            ClientError::Protocol("rust-analyzer stdin is unavailable".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ClientError::Protocol("rust-analyzer stdout is unavailable".to_string())
        })?;
        let (sender, responses) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(message)) => {
                        if sender.send(Ok(message)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Err("rust-analyzer closed stdout".to_string()));
                        break;
                    }
                    Err(error) => {
                        let _ =
                            sender.send(Err(format!("malformed rust-analyzer response: {error}")));
                        break;
                    }
                }
            }
        });
        let mut client = Self {
            child: Some(child),
            stdin,
            responses,
            next_id: 1,
            deadline: Instant::now() + timeout,
        };
        let root_uri = Url::from_directory_path(root).map_err(|()| {
            ClientError::Protocol(format!(
                "cannot convert workspace root to URI: {}",
                root.display()
            ))
        })?;
        let initialize = json!({
            "processId": Value::Null,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "callHierarchy": {
                        "dynamicRegistration": false
                    }
                },
                "experimental": {
                    "serverStatusNotification": true
                }
            },
            "workspaceFolders": [{
                "uri": root_uri,
                "name": root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace")
            }]
        });
        let _: Value = client.request("initialize", &initialize)?;
        client.notify("initialized", &json!({}))?;
        let ready_timeout = client.remaining("workspace loading")?;
        wait_until_ready(&client.responses, &mut client.stdin, ready_timeout)?;
        Ok(client)
    }

    pub(super) fn prepare(
        &mut self,
        uri: &Url,
        line: usize,
        column: usize,
    ) -> Result<Vec<CallHierarchyItem>, ClientError> {
        let params = json!({
            "textDocument": {"uri": uri},
            "position": {
                "line": line.saturating_sub(1),
                "character": column
            }
        });
        let result: Option<Vec<CallHierarchyItem>> =
            self.request("textDocument/prepareCallHierarchy", &params)?;
        Ok(result.unwrap_or_default())
    }

    pub(super) fn open(&mut self, uri: &Url, text: &str) -> Result<(), ClientError> {
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": "rust",
                "version": 1,
                "text": text
            }
        });
        self.notify("textDocument/didOpen", &params)
    }

    pub(super) fn outgoing(
        &mut self,
        item: &CallHierarchyItem,
    ) -> Result<Vec<OutgoingCall>, ClientError> {
        let result: Option<Vec<OutgoingCall>> =
            self.request("callHierarchy/outgoingCalls", &json!({"item": item}))?;
        Ok(result.unwrap_or_default())
    }

    pub(super) fn shutdown(mut self) -> Result<(), ClientError> {
        let _: Value = self.request("shutdown", &Value::Null)?;
        self.notify("exit", &Value::Null)?;
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        Ok(())
    }

    fn request<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<T, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        write_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }),
        )?;
        let request_timeout = self.remaining(method)?;
        let response = match receive_response(&self.responses, &mut self.stdin, id, request_timeout)
        {
            Ok(response) => response,
            Err(ClientError::TimedOut { .. }) => {
                let _ = self.notify("$/cancelRequest", &json!({"id": id}));
                return Err(ClientError::TimedOut {
                    method: method.to_string(),
                });
            }
            Err(error) => return Err(error),
        };
        if let Some(error) = response.get("error") {
            return Err(ClientError::Protocol(format!(
                "rust-analyzer {method} failed: {error}"
            )));
        }
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        serde_json::from_value(result).map_err(|error| {
            ClientError::Protocol(format!("invalid rust-analyzer {method} result: {error}"))
        })
    }

    fn notify(&mut self, method: &str, params: &Value) -> Result<(), ClientError> {
        write_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            }),
        )
        .map_err(ClientError::from)
    }

    fn remaining(&self, method: &str) -> Result<Duration, ClientError> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| ClientError::TimedOut {
                method: method.to_string(),
            })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_until_ready(
    responses: &Receiver<Result<Value, String>>,
    writer: &mut impl std::io::Write,
    timeout: Duration,
) -> Result<(), ClientError> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| ClientError::TimedOut {
                method: "workspace loading".to_string(),
            })?;
        let message = match responses.recv_timeout(remaining) {
            Ok(Ok(message)) => message,
            Ok(Err(message)) => return Err(ClientError::Protocol(message)),
            Err(RecvTimeoutError::Timeout) => {
                return Err(ClientError::TimedOut {
                    method: "workspace loading".to_string(),
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ClientError::Protocol(
                    "rust-analyzer response channel closed".to_string(),
                ));
            }
        };
        if respond_to_server_request(writer, &message)? {
            continue;
        }
        if message.get("method").and_then(Value::as_str) != Some("experimental/serverStatus") {
            continue;
        }
        let Some(params) = message.get("params") else {
            continue;
        };
        if !params
            .get("quiescent")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let health = params
            .get("health")
            .and_then(Value::as_str)
            .unwrap_or("error");
        if health == "error" {
            return Err(ClientError::Protocol(format!(
                "rust-analyzer workspace failed to load: {}",
                params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            )));
        }
        return Ok(());
    }
}

fn receive_response(
    responses: &Receiver<Result<Value, String>>,
    writer: &mut impl std::io::Write,
    id: u64,
    timeout: Duration,
) -> Result<Value, ClientError> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| ClientError::TimedOut {
                method: format!("request {id}"),
            })?;
        let message = match responses.recv_timeout(remaining) {
            Ok(Ok(message)) => message,
            Ok(Err(message)) => return Err(ClientError::Protocol(message)),
            Err(RecvTimeoutError::Timeout) => {
                return Err(ClientError::TimedOut {
                    method: format!("request {id}"),
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ClientError::Protocol(
                    "rust-analyzer response channel closed".to_string(),
                ));
            }
        };
        if respond_to_server_request(writer, &message)? {
            continue;
        }
        let Some(response_id) = message.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if response_id != id {
            return Err(ClientError::Protocol(format!(
                "unexpected rust-analyzer response id {response_id}, expected {id}"
            )));
        }
        return Ok(message);
    }
}

fn respond_to_server_request(
    writer: &mut impl std::io::Write,
    message: &Value,
) -> Result<bool, ClientError> {
    let (Some(id), Some(method)) = (
        message.get("id"),
        message.get("method").and_then(Value::as_str),
    ) else {
        return Ok(false);
    };
    let result = if method == "workspace/configuration" {
        let count = message
            .pointer("/params/items")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        Value::Array(vec![Value::Null; count])
    } else {
        Value::Null
    };
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
    )?;
    Ok(true)
}

fn write_message(writer: &mut impl std::io::Write, message: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut content_length = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return if saw_header {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "LSP headers ended unexpectedly",
                ))
            } else {
                Ok(None)
            };
        }
        saw_header = true;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .strip_prefix("Content-Length:")
            .map(str::trim)
            .and_then(|value| value.parse::<usize>().ok())
        {
            content_length = Some(value);
        }
    }
    let content_length = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
    })?;
    if content_length > MAX_MESSAGE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LSP message exceeds size budget",
        ));
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn lsp_framing_round_trips_json() {
        let message = json!({"jsonrpc": "2.0", "id": 7, "result": []});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &message).expect("write frame");

        assert_eq!(
            read_message(&mut Cursor::new(bytes)).expect("read frame"),
            Some(message)
        );
    }

    #[test]
    fn malformed_or_oversized_frames_are_rejected() {
        let malformed = b"Content-Length: 4\r\n\r\nnope";
        assert!(read_message(&mut Cursor::new(malformed)).is_err());

        let oversized = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        assert!(read_message(&mut Cursor::new(oversized)).is_err());
    }

    #[test]
    fn response_correlation_ignores_notifications() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(json!({"jsonrpc": "2.0", "method": "window/logMessage"})))
            .expect("notification");
        sender
            .send(Ok(json!({"jsonrpc": "2.0", "id": 2, "result": null})))
            .expect("response");

        let response = receive_response(&receiver, &mut Vec::new(), 2, Duration::from_secs(1))
            .expect("correlated response");
        assert_eq!(response["id"], 2);
    }

    #[test]
    fn response_correlation_answers_server_requests() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "workspace/configuration",
                "params": {"items": [{}, {}]}
            })))
            .expect("server request");
        sender
            .send(Ok(json!({"jsonrpc": "2.0", "id": 3, "result": []})))
            .expect("response");
        let mut replies = Vec::new();

        let response = receive_response(&receiver, &mut replies, 3, Duration::from_secs(1))
            .expect("correlated response");

        assert_eq!(response["id"], 3);
        let reply = read_message(&mut Cursor::new(replies))
            .expect("reply frame")
            .expect("reply");
        assert_eq!(reply["id"], 0);
        assert_eq!(reply["result"], json!([null, null]));
    }

    #[test]
    fn readiness_waits_for_quiescent_server_status() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(json!({
                "jsonrpc": "2.0",
                "method": "experimental/serverStatus",
                "params": {"health": "ok", "quiescent": false}
            })))
            .expect("busy status");
        sender
            .send(Ok(json!({
                "jsonrpc": "2.0",
                "method": "experimental/serverStatus",
                "params": {"health": "ok", "quiescent": true}
            })))
            .expect("ready status");

        wait_until_ready(&receiver, &mut Vec::new(), Duration::from_secs(1)).expect("ready");
    }

    #[test]
    fn response_timeout_is_bounded() {
        let (_sender, receiver) = mpsc::channel();
        let started = Instant::now();

        assert!(matches!(
            receive_response(&receiver, &mut Vec::new(), 3, Duration::from_millis(10)),
            Err(ClientError::TimedOut { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn mismatched_response_id_is_a_protocol_error() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(json!({"jsonrpc": "2.0", "id": 9, "result": null})))
            .expect("response");

        assert!(matches!(
            receive_response(&receiver, &mut Vec::new(), 2, Duration::from_secs(1)),
            Err(ClientError::Protocol(_))
        ));
    }
}
