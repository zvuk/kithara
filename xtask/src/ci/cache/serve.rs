use std::{
    convert::Infallible,
    process::{Child, Command},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
use tiny_http::Server;
use tracing::info;

use super::{
    evict::{self, Evictor},
    provision,
};
use crate::{
    child::{self, Cancel},
    consts,
};

/// Runs the cache stack whole: the store, the setup it is served with, and
/// the evictor that keeps it under its quotas, in one process tree that
/// starts and stops together. The evictor listens for the store's audit log
/// before the store starts, since the store gives up on a webhook that does
/// not answer as it starts.
pub(super) fn run() -> Result<()> {
    let cancel = Cancel::install()?;
    provision::credentials()?;
    let buckets = evict::buckets()?;
    let audit = Server::http(consts::EVICT_LISTEN)
        .map_err(|error| anyhow!("listen for the store's audit log: {error}"))?;
    let evictor = Evictor::listen(audit, buckets);
    let mut store = child::spawn(Command::new("rustfs").args(["server", "/data"]))?;
    let ready_url = format!("{}/health/ready", consts::CACHE_STORE_URL);
    if let Err(error) =
        ready(&mut store, &ready_url, &cancel).and_then(|()| provision::initialize(&cancel))
    {
        return Err(match child::terminate(&mut store, consts::GRACE) {
            Ok(_) => error,
            Err(stop) => error.context(stop),
        });
    }
    watch(&mut store, thread::spawn(|| evictor.run()), &cancel)
}

/// Waits until the store answers ready: the setup that follows talks to it.
fn ready(store: &mut Child, url: &str, cancel: &Cancel) -> Result<()> {
    let client = Client::builder()
        .timeout(consts::CACHE_READY_REQUEST)
        .build()
        .context("building the readiness client")?;
    loop {
        child::check(Some(cancel))?;
        if let Some(status) = store.try_wait().context("waiting on the store")? {
            bail!("the store exited with {status} before it was ready");
        }
        if client
            .get(url)
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        thread::sleep(consts::CACHE_READY_POLL);
    }
}

/// Keeps the stack up until a stop signal, the store's exit or the evictor's
/// end. A stop signal is a clean end; the store stops with the evictor, and
/// the stack with the store.
fn watch(
    store: &mut Child,
    evictor: JoinHandle<Result<Infallible>>,
    cancel: &Cancel,
) -> Result<()> {
    loop {
        if let Err(signal) = child::check(Some(cancel)) {
            let status = child::terminate(store, consts::GRACE)?;
            info!(reason = %signal, %status, "the store stopped");
            return Ok(());
        }
        if let Some(status) = store.try_wait().context("waiting on the store")? {
            bail!("the store exited with {status}");
        }
        if evictor.is_finished() {
            let error = match evictor.join() {
                Ok(Ok(never)) => match never {},
                Ok(Err(error)) => error,
                Err(_) => anyhow!("the evictor panicked"),
            };
            let status = child::terminate(store, consts::GRACE)?;
            info!(%status, "the store stopped");
            return Err(error.context("the evictor stopped"));
        }
        thread::sleep(consts::CHILD_POLL);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
    };

    use anyhow::anyhow;
    use signal_hook::{consts::signal::SIGTERM, low_level};
    use tiny_http::{Response, Server};

    use super::*;
    use crate::testing::signals;

    /// A store that says it is up, and stops cleanly when asked.
    fn store(then: &str) -> Child {
        let mut store = child::spawn(
            Command::new("sh")
                .args(["-c", &format!("trap 'exit 0' TERM; echo up; {then}")])
                .stdout(Stdio::piped()),
        )
        .expect("the stand-in store starts");
        let mut line = String::new();
        BufReader::new(store.stdout.take().expect("the store's output is piped"))
            .read_line(&mut line)
            .expect("the stand-in store says it is up");
        assert_eq!(line, "up\n");
        store
    }

    /// A store that serves until it is asked to stop.
    fn serving() -> Child {
        store("while :; do sleep 0.05; done")
    }

    fn evicting() -> JoinHandle<Result<Infallible>> {
        thread::spawn(|| {
            loop {
                thread::park();
            }
        })
    }

    #[test]
    fn a_stop_signal_asks_the_store_to_stop_and_ends_the_stack_cleanly() {
        let _signals = signals();
        let cancel = Cancel::install().unwrap();
        let mut store = serving();

        low_level::raise(SIGTERM).unwrap();
        watch(&mut store, evicting(), &cancel).expect("a stop signal is a clean end");

        let status = store
            .try_wait()
            .unwrap()
            .expect("the store outlived the stack");
        assert_eq!(status.code(), Some(0), "the store was killed: {status}");
    }

    #[test]
    fn the_stack_ends_with_the_store() {
        let _signals = signals();
        let cancel = Cancel::install().unwrap();
        let mut store = store("exit 3");

        let error = watch(&mut store, evicting(), &cancel).expect_err("the store is gone");

        assert!(error.to_string().contains("store exited"), "{error:#}");
    }

    #[test]
    fn the_store_stops_with_the_evictor() {
        let _signals = signals();
        let cancel = Cancel::install().unwrap();
        let mut store = serving();
        let evictor = thread::spawn(|| Err(anyhow!("the audit log receiver stopped")));

        let error = watch(&mut store, evictor, &cancel).expect_err("the evictor is gone");

        assert!(
            format!("{error:#}").contains("the audit log receiver stopped"),
            "{error:#}"
        );
        let status = store
            .try_wait()
            .unwrap()
            .expect("the store outlived the stack");
        assert_eq!(status.code(), Some(0), "the store was killed: {status}");
    }

    /// `RustFS` answers 503 while it starts, and the setup that follows talks
    /// to it.
    #[test]
    fn the_store_is_ready_once_it_answers_ready() {
        let _signals = signals();
        let cancel = Cancel::install().unwrap();
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/health/ready", server.server_addr());
        thread::spawn(move || {
            for (answered, request) in server.incoming_requests().enumerate() {
                let code = if answered == 0 { 503 } else { 200 };
                request.respond(Response::empty(code)).unwrap();
            }
        });
        let mut store = serving();

        let outcome = ready(&mut store, &url, &cancel);

        child::terminate(&mut store, consts::GRACE).unwrap();
        outcome.expect("the store answered ready");
    }

    #[test]
    fn a_store_that_exits_while_starting_ends_the_start() {
        let _signals = signals();
        let cancel = Cancel::install().unwrap();
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/health/ready", server.server_addr());
        thread::spawn(move || {
            for request in server.incoming_requests() {
                request.respond(Response::empty(503)).unwrap();
            }
        });
        let mut store = store("exit 5");

        let error = ready(&mut store, &url, &cancel).expect_err("the store is gone");

        assert!(error.to_string().contains("store exited"), "{error:#}");
    }

    #[test]
    fn the_wait_for_the_store_is_given_up_on_a_stop_signal() {
        let _signals = signals();
        let cancel = Cancel::install().unwrap();
        let mut store = serving();

        low_level::raise(SIGTERM).unwrap();
        let outcome = ready(&mut store, "http://127.0.0.1:9/", &cancel);

        child::terminate(&mut store, consts::GRACE).unwrap();
        assert!(outcome.is_err(), "the start went on past a stop signal");
    }
}
