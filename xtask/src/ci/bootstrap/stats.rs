use std::{io, path::Path, process::Command};
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    time::Instant,
};

#[cfg(unix)]
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use socket2::{Domain, SockAddr, Socket, Type};

use super::cache;
#[cfg(unix)]
use super::consts;

/// Read daemon policy and counters without probing compiler blob extents.
///
/// The caller supplies the executable and its cache environment. The runtime
/// must name that configuration's private daemon directory.
///
/// # Errors
/// Returns an error if startup, transport or the daemon response fails.
pub fn statistics(command: &mut Command, runtime: &Path) -> io::Result<String> {
    #[cfg(unix)]
    {
        let deadline = Instant::now() + consts::COMMAND_TIMEOUT;
        command.args(["daemon", "start"]);
        cache::output(command)?;
        query(runtime, deadline)
    }
    #[cfg(not(unix))]
    {
        let _ = runtime;
        command.args(["stats", "--json"]);
        cache::output(command)
    }
}

#[cfg(unix)]
fn query(runtime: &Path, deadline: Instant) -> io::Result<String> {
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    // A full listener backlog can block connect before read timeouts apply.
    socket.set_nonblocking(true)?;
    socket.connect(&SockAddr::unix(runtime.join("daemon.sock"))?)?;
    socket.set_nonblocking(false)?;
    let descriptor: OwnedFd = socket.into();
    let mut socket = UnixStream::from(descriptor);
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "compiler-cache startup exhausted its deadline",
            )
        })?;
    socket.set_write_timeout(Some(remaining))?;
    socket.write_all(b"{\"stats\":{\"include_entries\":false,\"include_summaries\":false,\"sort_by\":null,\"event_hours\":24,\"event_secs\":86400,\"client_epoch\":0}}\n")?;
    read_reply(&mut socket, deadline)
}

#[cfg(unix)]
fn read_reply(socket: &mut UnixStream, deadline: Instant) -> io::Result<String> {
    // Darwin rejects socket timeout changes after the peer closes a buffered reply.
    socket.set_nonblocking(true)?;
    let mut reply = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "compiler-cache statistics timed out",
                )
            })?;
        let timeout = PollTimeout::try_from(remaining)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut descriptors = [PollFd::new(socket.as_fd(), PollFlags::POLLIN)];
        match poll(&mut descriptors, timeout) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        let mut bytes = [0; 4096];
        let count = match socket.read(&mut bytes) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete compiler-cache response",
            ));
        }
        let end = bytes[..count].iter().position(|byte| *byte == b'\n');
        reply.extend_from_slice(&bytes[..end.unwrap_or(count)]);
        if reply.len() as u64 > consts::OUTPUT_LIMIT {
            return Err(io::Error::other("compiler-cache output exceeds its bound"));
        }
        if end.is_some() {
            return normalize(&reply);
        }
    }
}

#[cfg(unix)]
fn normalize(reply: &[u8]) -> io::Result<String> {
    let reply: Value = serde_json::from_slice(reply)
        .map_err(|_| io::Error::other("invalid compiler-cache daemon response"))?;
    if reply["ok"] != true || !reply["stats"].is_object() {
        return Err(io::Error::other(
            "compiler-cache daemon rejected statistics",
        ));
    }
    let stats = &reply["stats"];
    let policy = &stats["effective_config"];
    let mut report = json!({
        "remote": policy["remote_description"],
        "daemon_connected": true,
        "daemon_remote_readonly": policy["remote_readonly"],
        "disk": {"store_bytes": stats["blob_stats"]["total_blob_size"], "store_limit_bytes": stats["max_size"]},
        "time_saved_ms": stats["events"]["hit_compile_time_ms"],
    });
    if policy["local_only"] != false {
        report["remote"] = Value::Null;
    }
    for field in "local_hits prefetch_hits remote_hits dups misses total_elapsed_ms hit_elapsed_ms miss_elapsed_ms".split_whitespace() {
        report[field] = json!(stats["events"][field].as_u64());
    }
    Ok(report.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
    };

    use serde_json::{Value, json};

    use super::*;

    fn response() -> Value {
        json!({"ok":true,"stats":{
            "effective_config":{"remote_description":"s3://review/kache", "remote_readonly":false,"local_only":false},
            "events":{"remote_hits":7,"misses":2},"total_size":123,"max_size":456
        }})
    }

    #[test]
    fn policy_comes_from_the_running_daemon_and_fails_closed() {
        let ready = response();
        let stats = normalize(ready.to_string().as_bytes()).unwrap();
        super::super::require_remote(&stats).unwrap();
        let stats: Value = serde_json::from_str(&stats).unwrap();
        assert_eq!(stats["remote_hits"], 7);
        assert!(stats["local_hits"].is_null());
        for (field, invalid) in [
            ("remote_description", Value::Null),
            ("remote_readonly", json!(true)),
            ("remote_readonly", Value::Null),
            ("local_only", json!(true)),
            ("local_only", Value::Null),
        ] {
            let mut reply = ready.clone();
            reply["stats"]["effective_config"][field] = invalid;
            let stats = normalize(reply.to_string().as_bytes()).unwrap();
            assert!(super::super::require_remote(&stats).is_err(), "{field}");
        }
        for invalid in [b"invalid".as_slice(), b"{}", b"{\"ok\":false,\"stats\":{}}"] {
            assert!(normalize(invalid).is_err());
        }
    }

    #[test]
    fn daemon_transport_requests_counters_without_blob_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(directory.path().join("daemon.sock")).unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(socket.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["stats"]["include_entries"], false);
            assert_eq!(request["stats"]["include_summaries"], false);
            assert_eq!(request["stats"]["client_epoch"], 0);
            writeln!(socket, "{}", response()).unwrap();
        });
        let stats = query(directory.path(), Instant::now() + consts::COMMAND_TIMEOUT).unwrap();
        super::super::require_remote(&stats).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn buffered_reply_survives_the_daemon_closing_its_socket() {
        let (mut socket, mut daemon) = UnixStream::pair().unwrap();
        writeln!(daemon, "{}", response()).unwrap();
        drop(daemon);
        let stats = read_reply(&mut socket, Instant::now() + consts::COMMAND_TIMEOUT).unwrap();
        super::super::require_remote(&stats).unwrap();
    }

    #[test]
    fn an_unfinished_reply_keeps_the_original_deadline() {
        let (mut socket, mut daemon) = UnixStream::pair().unwrap();
        daemon.write_all(b"{\"ok\":true").unwrap();
        let error = read_reply(
            &mut socket,
            Instant::now() + std::time::Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn incomplete_and_oversized_responses_are_rejected() {
        for bytes in [b"{}".to_vec(), vec![b'x'; 1_048_577]] {
            let directory = tempfile::tempdir().unwrap();
            let listener = UnixListener::bind(directory.path().join("daemon.sock")).unwrap();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(socket.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let _ = socket.write_all(&bytes);
            });
            assert!(query(directory.path(), Instant::now() + consts::COMMAND_TIMEOUT).is_err());
            server.join().unwrap();
        }
    }
}
