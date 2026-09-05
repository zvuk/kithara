use kithara::{
    broadcast::{Broadcast, BroadcastHandle},
    output::OutputGroup,
};

use super::state::{BroadcastResult, Packager};
use crate::{config::AppBroadcastConfig, pools::AppHost};

pub(crate) struct Backend;

/// The running stream fed by the Host master-output group.
pub(crate) struct Stream {
    handle: BroadcastHandle,
}

trait BroadcastHost {
    fn measured_sample_rate(&self) -> BroadcastResult<Option<u32>>;
    fn enable_outputs(&self, outputs: OutputGroup) -> BroadcastResult<()>;
    fn disable_outputs(&self) -> BroadcastResult<()>;
}

impl BroadcastHost for AppHost {
    fn measured_sample_rate(&self) -> BroadcastResult<Option<u32>> {
        Ok(self.sample_rate()?.measured)
    }

    fn enable_outputs(&self, outputs: OutputGroup) -> BroadcastResult<()> {
        AppHost::enable_outputs(self, outputs)?;
        Ok(())
    }

    fn disable_outputs(&self) -> BroadcastResult<()> {
        AppHost::disable_outputs(self)?;
        Ok(())
    }
}

impl Packager for Backend {
    type Config = AppBroadcastConfig;
    type Live = Stream;

    const IS_AVAILABLE: bool = true;

    fn is_live(live: &Stream) -> bool {
        live.handle.status().is_live
    }

    fn start(host: &AppHost, config: &AppBroadcastConfig) -> BroadcastResult<Option<Stream>> {
        let Some(config) = measured_config(host, config)? else {
            return Ok(None);
        };
        start(host, config).map(Some)
    }

    fn release(host: &AppHost) -> BroadcastResult<()> {
        release(host)
    }

    fn stop(live: Stream) {
        live.handle.stop();
    }

    fn url(live: &Stream) -> &str {
        live.handle.url()
    }
}

fn start<H: BroadcastHost>(host: &H, config: AppBroadcastConfig) -> BroadcastResult<Stream> {
    let (output, handle) = Broadcast::start(config)?;
    let mut outputs = OutputGroup::new();
    outputs.push(output);
    host.enable_outputs(outputs)?;
    Ok(Stream { handle })
}

fn release<H: BroadcastHost>(host: &H) -> BroadcastResult<()> {
    host.disable_outputs()
}

fn measured_config<H: BroadcastHost>(
    host: &H,
    config: &AppBroadcastConfig,
) -> BroadcastResult<Option<AppBroadcastConfig>> {
    Ok(host
        .measured_sample_rate()?
        .map(|sample_rate| config.with_sample_rate(sample_rate)))
}

#[cfg(test)]
mod tests {
    use kithara::{
        platform::{
            CancelToken,
            sync::{
                Mutex,
                atomic::{AtomicU32, Ordering},
            },
            thread,
            time::Duration,
        },
        worker::{Worker, WorkerConfig},
    };

    use super::*;
    use crate::{document::Document, pools};

    struct SampleRateSession {
        outputs: Mutex<Option<OutputGroup>>,
        sample_rate: AtomicU32,
    }

    impl SampleRateSession {
        fn new(sample_rate: u32) -> Self {
            Self {
                outputs: Mutex::new(None),
                sample_rate: AtomicU32::new(sample_rate),
            }
        }
    }

    impl BroadcastHost for SampleRateSession {
        fn measured_sample_rate(&self) -> BroadcastResult<Option<u32>> {
            let sample_rate = self.sample_rate.load(Ordering::Relaxed);
            Ok((sample_rate != 0).then_some(sample_rate))
        }

        fn enable_outputs(&self, outputs: OutputGroup) -> BroadcastResult<()> {
            *self.outputs.lock() = Some(outputs);
            Ok(())
        }

        fn disable_outputs(&self) -> BroadcastResult<()> {
            self.outputs.lock().take();
            Ok(())
        }
    }

    fn config(shutdown: &CancelToken) -> AppBroadcastConfig {
        AppBroadcastConfig::builder(
            Worker::new(WorkerConfig::new()),
            pools::build(&pools::PoolsSection::default()).expect("test pools"),
        )
        .cancel(shutdown.child())
        .build()
    }

    fn on_air(sample_rate: u32) -> (Stream, SampleRateSession, CancelToken) {
        let session = SampleRateSession::new(sample_rate);
        let shutdown = CancelToken::root();
        let config = measured_config(&session, &config(&shutdown))
            .expect("sample-rate query")
            .expect("a measured rate yields a config");
        let stream = start(&session, config).expect("the packager starts");
        (stream, session, shutdown)
    }

    #[kithara::test(native, flash(false))]
    fn configuration_waits_for_the_measured_session_sample_rate() {
        let session = SampleRateSession::new(0);
        let shutdown = CancelToken::root();
        let configured = AppBroadcastConfig::builder(
            Worker::new(WorkerConfig::new()),
            pools::build(&pools::PoolsSection::default()).expect("test pools"),
        )
        .cancel(shutdown.child())
        .bit_rate(192_000)
        .build();

        assert!(measured_config(&session, &configured).unwrap().is_none());
        assert!(
            measured_config(&session, &configured).unwrap().is_none(),
            "an unmeasured rate is a retry, not a failure"
        );

        session.sample_rate.store(48_000, Ordering::Relaxed);
        let config = measured_config(&session, &configured)
            .unwrap()
            .expect("measured sample rate");

        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.bit_rate, 192_000);
    }

    #[kithara::test(native, flash(false))]
    fn starting_takes_the_output_group_and_stopping_gives_it_back() {
        let (stream, session, shutdown) = on_air(48_000);
        assert!(
            session.outputs.lock().is_some(),
            "the running stream holds the session's output group"
        );

        release(&session).expect("release output group");
        Backend::stop(stream);

        assert!(
            session.outputs.lock().is_none(),
            "the drained stream returns the output group"
        );
        shutdown.cancel();
    }

    #[kithara::test(native, flash(false))]
    fn a_dropped_output_group_ends_the_stream() {
        let (stream, session, shutdown) = on_air(48_000);
        session.outputs.lock().take();

        for _ in 0..1_000 {
            if !Backend::is_live(&stream) {
                break;
            }
            thread::paced_backoff(Duration::from_millis(1));
        }

        assert!(!Backend::is_live(&stream));
        shutdown.cancel();
    }

    /// A document that names `broadcast: {bit_rate: ...}` must parse under
    /// the real configuration document schema, and the patch it carries must
    /// reach a `BroadcastConfig` without disturbing fields the document left
    /// unnamed.
    #[kithara::test(native, flash(false))]
    fn the_broadcast_section_parses_and_reaches_the_config() {
        let document: Document = serde_yaml_ng::from_str("broadcast:\n  bit_rate: 256000\n")
            .expect("the document types");
        let shutdown = CancelToken::root();
        let mut config = AppBroadcastConfig::builder(
            Worker::new(WorkerConfig::new()),
            pools::build(&pools::PoolsSection::default()).expect("test pools"),
        )
        .cancel(shutdown.child())
        .channels(4)
        .build();

        config.apply(document.broadcast);

        assert_eq!(config.bit_rate, 256_000);
        assert_eq!(
            config.channels, 4,
            "a document naming only bit_rate must not reset the seeded channel count"
        );
    }
}
