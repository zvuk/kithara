//! Whether this machine reaches the outside endpoints its lanes fetch from.
//!
//! The runners lost name resolution and then every connection for minutes at a
//! time, and the only trace was lanes failing their fetches. Health asks the
//! same questions on a timer, so an outage shows in the journal with the time
//! it started and which step failed.

use std::{
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use tracing::{info, warn};

/// Resolves and connects to every `host:port` in `endpoints`, logging how long
/// each step took. Returns one line per endpoint that could not be reached,
/// naming the step that failed.
pub(super) fn unreachable(endpoints: &[&str], timeout: Duration) -> Vec<String> {
    endpoints
        .iter()
        .filter_map(|endpoint| {
            let started = Instant::now();
            let address = match endpoint.to_socket_addrs().map(|mut found| found.next()) {
                Ok(Some(address)) => address,
                Ok(None) => return Some(failed(endpoint, "dns", "no address", started)),
                Err(error) => return Some(failed(endpoint, "dns", &error.to_string(), started)),
            };
            let resolved = started.elapsed();
            if let Err(error) = TcpStream::connect_timeout(&address, timeout) {
                return Some(failed(endpoint, "tcp", &error.to_string(), started));
            }
            info!(
                endpoint,
                %address,
                dns_ms = resolved.as_millis(),
                connect_ms = (started.elapsed() - resolved).as_millis(),
                "outside endpoint reachable"
            );
            None
        })
        .collect()
}

fn failed(endpoint: &str, step: &str, error: &str, started: Instant) -> String {
    let after_ms = started.elapsed().as_millis();
    warn!(
        endpoint,
        step, error, after_ms, "outside endpoint unreachable"
    );
    format!("{endpoint} ({step}: {error})")
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(1);

    #[test]
    fn a_listening_endpoint_is_reachable() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local endpoint");
        let endpoint = listener.local_addr().expect("local address").to_string();
        assert!(unreachable(&[&endpoint], TIMEOUT).is_empty());
    }

    #[test]
    fn each_unreachable_endpoint_names_the_step_that_failed() {
        let closed = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local endpoint");
            listener.local_addr().expect("local address").to_string()
        };
        let failures = unreachable(&[&closed, "not an endpoint"], TIMEOUT);
        assert_eq!(failures.len(), 2, "{failures:?}");
        assert!(
            failures[0].starts_with(&format!("{closed} (tcp: ")),
            "{failures:?}"
        );
        assert!(
            failures[1].starts_with("not an endpoint (dns: "),
            "{failures:?}"
        );
    }
}
