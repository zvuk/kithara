use std::error::Error;

use rodio::{OutputStreamBuilder, Sink, Source};
use tracing::info;

type ExampleError = Box<dyn Error + Send + Sync>;
type ExampleResult<T = ()> = Result<T, ExampleError>;

pub(crate) async fn play_until_end<S>(source: S) -> ExampleResult
where
    S: Source + Send + 'static,
{
    let handle = tokio::task::spawn_blocking(move || -> ExampleResult {
        let stream_handle = OutputStreamBuilder::open_default_stream()?;
        let sink = Sink::connect_new(stream_handle.mixer());
        sink.set_volume(1.0);
        sink.append(source);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok(())
    });

    handle.await??;
    Ok(())
}
